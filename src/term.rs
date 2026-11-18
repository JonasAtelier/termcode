//! The integrated terminal: the user's shell on a pseudo-terminal, in a panel under the
//! editor groups. vt100 keeps its screen (and 5000 rows of scrollback); each frame draws
//! that screen cell by cell. The event loop waits on the PTY beside the keyboard
//! (`fd`), so output shows as it comes, and reads it without blocking (`pump`).

use crate::panel::{App, Drag, Focus, PAct, Prompt};
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use std::cell::RefCell;
use std::fs::{self, File};
use std::os::unix::fs::OpenOptionsExt;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const SCROLLBACK: usize = 5000;

thread_local! {
    /// The open terminals' PTYs, for the event loop (on this thread) to wait on.
    static FDS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
}

/// The user's shell: `$SHELL`, else /bin/sh.
pub fn shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
}

/// `s` as one shell word: in single quotes, a quote in it closed and escaped.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn fds() -> Vec<i32> {
    FDS.with(|f| f.borrow().clone())
}

pub struct Term {
    master: File,
    /// The shell: started here (`child`), or (None) taken over across a reload; `pid`
    /// either way.
    child: Option<Child>,
    pid: i32,
    parser: vt100::Parser,
    /// Selected text: where the drag started and where it is, (row, column) on screen.
    sel: Option<((u16, u16), (u16, u16))>,
    /// Its tab's name and when it was worked out (looked up again after a second).
    name: RefCell<Option<(Instant, String)>>,
}

impl Term {
    /// `shell` in `cwd`, on a new PTY of `rows` x `cols`.
    pub fn spawn(shell: &str, cwd: &Path, rows: u16, cols: u16) -> io::Result<Term> {
        let (rows, cols) = (rows.max(2), cols.max(2));
        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        let (mut m, mut s) = (0, 0);
        // SAFETY: valid out pointers and winsize; the name and termios may be null.
        if unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), &ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openpty just opened both; each File now owns one.
        let (master, slave) = unsafe { (File::from_raw_fd(m), File::from_raw_fd(s)) };
        // Neither end leaks into other children (or a reload's exec); the loop reads the
        // master only when poll says so, and never blocks on it.
        // SAFETY: both descriptors are open.
        unsafe {
            libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(s, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(m, libc::F_SETFL, libc::fcntl(m, libc::F_GETFL) | libc::O_NONBLOCK);
        }
        let mut cmd = Command::new(shell);
        cmd.current_dir(cwd)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "termcode")
            .env(crate::api::ENV, crate::api::path().map(|p| p.as_os_str()).unwrap_or_default())
            .stdin(slave.try_clone()?)
            .stdout(slave.try_clone()?)
            .stderr(slave);
        // The shell leads its own session with the PTY as its terminal, so job control
        // and Ctrl+C work in it, and closing the master hangs it up.
        // SAFETY: setsid and ioctl are async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        FDS.with(|f| f.borrow_mut().push(m));
        let pid = child.id() as i32;
        Ok(Term { master, child: Some(child), pid, parser: vt100::Parser::new(rows, cols, SCROLLBACK), sel: None, name: RefCell::new(None) })
    }

    /// For a reload's exec: the PTY stays open across it (no close-on-exec), and what the
    /// terminal shows (its scrollback as plain text, its screen as it is) goes to a file
    /// in `dir`. Returns "fd,pid,rows,cols" for `adopt`.
    pub fn hand_over(&mut self, dir: &Path) -> io::Result<String> {
        let file = screen_file(dir, self.master.as_raw_fd());
        let (rows, cols) = self.parser.screen().size();
        let mut out = Vec::new();
        if self.parser.screen().alternate_screen() {
            // A full-screen program's screen; the shell's under it is not kept.
            out.extend_from_slice(b"\x1b[?1049h");
        } else {
            let lines = self.lines();
            let back = lines.len().saturating_sub(rows as usize);
            for l in &lines[..back] {
                out.extend_from_slice(l.trim_end().as_bytes());
                out.extend_from_slice(b"\r\n");
            }
            // Pushed up into the scrollback, under the screen drawn next.
            out.extend(std::iter::repeat_n(b'\n', rows as usize));
        }
        out.extend(self.parser.screen().state_formatted());
        out.extend(self.parser.screen().cursor_state_formatted());
        fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&file)?.write_all(&out)?;
        let fd = self.master.as_raw_fd();
        // SAFETY: an open descriptor; only its close-on-exec flag changes.
        unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
        Ok(format!("{fd},{},{rows},{cols}", self.pid))
    }

    /// After a reload: the terminal `hand_over` described, its shell still running on the
    /// same PTY (this process's child still: the pid survived the exec), its screen back
    /// from its file in `dir` (removed).
    pub fn adopt(spec: &str, dir: &Path) -> Option<Term> {
        let n: Vec<i32> = spec.split(',').map(|x| x.parse().ok()).collect::<Option<_>>()?;
        let [fd, pid, rows, cols] = n[..] else { return None };
        let file = screen_file(dir, fd);
        // Its screen file is read (or not wanted) once, whatever comes of it.
        let screen = fs::read(&file).ok();
        let _ = fs::remove_file(&file);
        let (rows, cols) = (u16::try_from(rows).ok()?, u16::try_from(cols).ok()?);
        // Never stdin, stdout or stderr, only a terminal (a PTY's master): a stale or
        // made-up variable must not take the screen's descriptor over.
        // SAFETY: fcntl and isatty on a descriptor number only read; -1/0 if not open.
        if fd <= 2 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 || unsafe { libc::isatty(fd) } != 1 {
            return None;
        }
        // SAFETY: this process opened it before the exec and nothing else owns it.
        let master = unsafe { File::from_raw_fd(fd) };
        // SAFETY: an open descriptor.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        let mut parser = vt100::Parser::new(rows, cols, SCROLLBACK);
        if let Some(screen) = screen {
            parser.process(&screen);
        }
        FDS.with(|f| f.borrow_mut().push(fd));
        Some(Term { master, child: None, pid, parser, sel: None, name: RefCell::new(None) })
    }

    /// Reads what the shell wrote (1 MB at most a turn: keys still come first). Returns
    /// whether anything came, and false for `alive` once the shell and everything it
    /// started have closed the terminal.
    pub fn pump(&mut self) -> (bool, bool) {
        let mut buf = [0u8; 1 << 16];
        let mut got = false;
        for _ in 0..16 {
            match self.master.read(&mut buf) {
                Ok(0) => return (got, false),
                Ok(n) => {
                    self.parser.process(&buf[..n]);
                    got = true;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return (got, true),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // EIO: no process has the terminal open any more.
                Err(_) => return (got, false),
            }
        }
        (got, true)
    }

    /// Writes `b` to the shell, reading its output meanwhile so neither side stalls on a
    /// full buffer; gives up after a second.
    pub fn send(&mut self, mut b: &[u8]) {
        self.parser.screen_mut().set_scrollback(0);
        self.sel = None;
        let deadline = Instant::now() + Duration::from_secs(1);
        while !b.is_empty() && Instant::now() < deadline {
            match self.master.write(b) {
                Ok(n) => b = &b[n..],
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if !self.pump().0 {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }

    pub fn key(&mut self, k: KeyEvent) {
        let b = key_bytes(k, self.parser.screen().application_cursor());
        if !b.is_empty() {
            self.send(&b);
        }
    }

    pub fn paste(&mut self, s: &str) {
        if self.parser.screen().bracketed_paste() {
            self.send(format!("\x1b[200~{s}\x1b[201~").as_bytes());
        } else {
            self.send(s.as_bytes());
        }
    }

    /// The wheel: to a program that asked for mouse reports, its wheel; in a full-screen
    /// program (less, man), arrow keys; otherwise through the scrollback. `at` is the
    /// pointer's cell in the screen.
    pub fn wheel(&mut self, d: isize, at: (u16, u16)) {
        let s = self.parser.screen();
        let up = d < 0;
        if s.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
            let (col, row) = (at.0 + 1, at.1 + 1);
            let b = if up { 64 } else { 65 };
            let ev = if s.mouse_protocol_encoding() == vt100::MouseProtocolEncoding::Sgr {
                format!("\x1b[<{b};{col};{row}M").into_bytes()
            } else {
                vec![0x1b, b'[', b'M', 32 + b, (32 + col).min(255) as u8, (32 + row).min(255) as u8]
            };
            return self.send(&ev);
        }
        if s.alternate_screen() {
            let arrow = key_bytes(KeyEvent::from(if up { KeyCode::Up } else { KeyCode::Down }), s.application_cursor());
            return self.send(&arrow.repeat(d.unsigned_abs()));
        }
        let off = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(if up { off + d.unsigned_abs() } else { off.saturating_sub(d.unsigned_abs()) });
    }

    /// What runs in it besides the shell waiting at its prompt, by name: the foreground
    /// job (the terminal's process group, as tmux names a pane), else a background child.
    pub fn running(&self) -> Option<String> {
        let pid = self.pid;
        let name = |p: i32| std::fs::read_to_string(format!("/proc/{p}/comm")).map_or_else(|_| "a program".into(), |s| s.trim().to_string());
        // SAFETY: an open PTY master.
        let fg = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        if fg > 0 && fg != pid {
            return Some(name(fg));
        }
        let kids = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).ok()?;
        kids.split_whitespace().next()?.parse().ok().map(name)
    }

    /// Whether the program in it asked for the mouse (vim's mouse=a, htop, less --mouse).
    pub fn wants_mouse(&self) -> bool {
        self.parser.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    /// A press (`button` 0 left, 1 middle, 2 right), its release (`press` false) or a drag
    /// (`motion`), at `at` (row, column), reported as the program asked: X10, normal or SGR.
    pub fn mouse(&mut self, button: u8, press: bool, motion: bool, mods: KeyModifiers, at: (u16, u16)) {
        let s = self.parser.screen();
        if let Some(ev) = mouse_bytes((s.mouse_protocol_mode(), s.mouse_protocol_encoding()), button, press, motion, mods, at) {
            self.send(&ev);
        }
    }

    /// Selects from `from` to `to`, (row, column) on screen, either way round.
    pub fn select(&mut self, from: (u16, u16), to: (u16, u16)) {
        self.sel = Some((from, to));
    }

    /// Selects the word (a run of non-blank cells) at `at`.
    pub fn select_word(&mut self, at: (u16, u16)) {
        let s = self.parser.screen();
        let blank = |c: u16| s.cell(at.0, c).is_none_or(|c| c.contents().trim().is_empty());
        if blank(at.1) {
            return;
        }
        let cols = s.size().1;
        let a = (0..=at.1).rev().take_while(|&c| !blank(c)).last().unwrap_or(at.1);
        let b = (at.1..cols).take_while(|&c| !blank(c)).last().unwrap_or(at.1);
        self.sel = Some(((at.0, a), (at.0, b)));
    }

    /// The selection in reading order, None when there is none.
    fn sel_range(&self) -> Option<((u16, u16), (u16, u16))> {
        let (a, b) = self.sel?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// The selected text, None when nothing is selected.
    pub fn selected(&self) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.sel_range()?;
        Some(self.parser.screen().contents_between(r0, c0, r1, c1 + 1)).filter(|t| !t.is_empty())
    }

    pub fn clear_selection(&mut self) {
        self.sel = None;
    }

    /// Fits the screen and the shell's idea of it to `rows` x `cols`.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(2));
        if self.parser.screen().size() == (rows, cols) {
            return;
        }
        self.parser.screen_mut().set_size(rows, cols);
        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: an open PTY master and a valid winsize.
        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) };
    }

    /// The screen into `area` of `buf`, its default colours `fg` on `bg`, the selection on
    /// `sel_bg`; the cursor as a reversed cell when `focused` and the live screen (not the
    /// scrollback) shows.
    pub fn draw(&self, buf: &mut Buffer, area: Rect, (fg, bg, sel_bg): (Color, Color, Color), focused: bool) {
        let s = self.parser.screen();
        let sel = self.sel_range();
        let selected = |row: u16, col: u16| sel.is_some_and(|(a, b)| (row, col) >= a && (row, col) <= b);
        for row in 0..area.height {
            for col in 0..area.width {
                let Some(c) = s.cell(row, col) else { continue };
                let cell = &mut buf[(area.x + col, area.y + row)];
                let (mut f, mut b) = (color(c.fgcolor(), fg), color(c.bgcolor(), bg));
                if c.inverse() {
                    (f, b) = (b, f);
                }
                if selected(row, col) {
                    b = sel_bg;
                }
                let text = c.contents();
                // A wide character's second cell is drawn over by its first.
                cell.set_symbol(if text.is_empty() || c.is_wide_continuation() { " " } else { text }).set_fg(f).set_bg(b);
                let mut m = Modifier::empty();
                m.set(Modifier::BOLD, c.bold());
                m.set(Modifier::ITALIC, c.italic());
                m.set(Modifier::UNDERLINED, c.underline());
                cell.modifier = m;
            }
        }
        let (row, col) = s.cursor_position();
        if focused && !s.hide_cursor() && s.scrollback() == 0 && row < area.height && col < area.width {
            let cell = &mut buf[(area.x + col, area.y + row)];
            cell.modifier.toggle(Modifier::REVERSED);
        }
    }

    /// Every row: the scrollback's, oldest first, then the screen's.
    pub fn lines(&mut self) -> Vec<String> {
        let keep = self.parser.screen().scrollback();
        let cols = self.parser.screen().size().1;
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let n = self.parser.screen().scrollback();
        let mut v: Vec<String> = self.parser.screen().rows(0, cols).take(n).collect();
        self.parser.screen_mut().set_scrollback(0);
        v.extend(self.parser.screen().rows(0, cols));
        self.parser.screen_mut().set_scrollback(keep);
        v
    }

    /// Scrolls row `abs` of `lines` into the middle of the view and selects its columns
    /// `cols` (start, end exclusive).
    pub fn show(&mut self, abs: usize, cols: (u16, u16)) {
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let n = self.parser.screen().scrollback();
        let rows = self.parser.screen().size().0 as usize;
        let top = abs.saturating_sub(rows / 2).min(n);
        self.parser.screen_mut().set_scrollback(n - top);
        let row = (abs - top) as u16;
        self.sel = Some(((row, cols.0), (row, cols.1.saturating_sub(1).max(cols.0))));
    }

    /// Back to the live screen, nothing selected.
    pub fn scroll_to_bottom(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
        self.sel = None;
    }

    /// Whether a full-screen program (vim, less, htop) has the screen.
    pub fn full_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// Its tab's name: what runs in it, else the shell's name; looked up once a second
    /// (it reads /proc), not every frame.
    pub fn name(&self) -> String {
        let mut cache = self.name.borrow_mut();
        if let Some((_, n)) = cache.as_ref().filter(|(at, _)| at.elapsed() < Duration::from_secs(1)) {
            return n.clone();
        }
        let n = self.running().unwrap_or_else(|| Path::new(&shell()).file_name().map_or_else(|| "sh".into(), |n| n.to_string_lossy().into_owned()));
        *cache = Some((Instant::now(), n.clone()));
        n
    }

    #[cfg(test)]
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }
}

impl Drop for Term {
    /// The master closes after this, which hangs the shell up as closing a terminal
    /// window does; it is reaped in the background (no zombie, no signal sent).
    fn drop(&mut self) {
        let fd = self.master.as_raw_fd();
        FDS.with(|f| f.borrow_mut().retain(|&x| x != fd));
        match self.child.take() {
            Some(c) => drop(crate::nav::reap(c, None)),
            // Taken over across a reload: still this process's child, reaped by pid.
            None => {
                let pid = self.pid;
                // SAFETY: waitpid on our own child; it blocks this thread only.
                std::thread::spawn(move || unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) });
            }
        }
    }
}

/// What xterm reports for a mouse press, release or drag (see `Term::mouse`) under the
/// program's mode and encoding; None when its mode leaves this one out.
pub fn mouse_bytes(
    (mode, enc): (vt100::MouseProtocolMode, vt100::MouseProtocolEncoding),
    button: u8,
    press: bool,
    motion: bool,
    mods: KeyModifiers,
    at: (u16, u16),
) -> Option<Vec<u8>> {
    use vt100::MouseProtocolMode as M;
    let ok = match mode {
        M::None => false,
        M::Press => press && !motion,
        M::PressRelease => !motion,
        M::ButtonMotion | M::AnyMotion => true,
    };
    if !ok {
        return None;
    }
    let m = u8::from(mods.contains(KeyModifiers::SHIFT)) * 4 + u8::from(mods.contains(KeyModifiers::ALT)) * 8 + u8::from(mods.contains(KeyModifiers::CONTROL)) * 16;
    let code = button + m + if motion { 32 } else { 0 };
    let (row, col) = (at.0 + 1, at.1 + 1);
    Some(if enc == vt100::MouseProtocolEncoding::Sgr {
        format!("\x1b[<{code};{col};{row}{}", if press { 'M' } else { 'm' }).into_bytes()
    } else {
        // Normal (X10) encoding: a release is button 3; positions past 223 are clipped.
        let b = if press { code } else { 3 + m };
        vec![0x1b, b'[', b'M', 32 + b, (32 + col).min(255) as u8, (32 + row).min(255) as u8]
    })
}

/// Where a reload keeps terminal `fd`'s screen: named by this process (the pid outlives
/// the exec) and the descriptor.
pub fn screen_file(dir: &Path, fd: i32) -> std::path::PathBuf {
    dir.join(format!("{}-{fd}", std::process::id()))
}

/// Ctrl+` (a legacy terminal sends it as Ctrl+Space).
pub fn toggle_key(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('`' | ' '))
}

/// Find in a terminal's scrollback (Ctrl+F while the shell, not a full-screen program,
/// has the screen): the text sought and the match shown, (row, start, end) in `lines`.
#[derive(Default)]
pub struct TermFind {
    pub query: String,
    pub at: Option<(usize, usize, usize)>,
    /// The match shown's place among them all, and how many there are.
    pub count: (usize, usize),
}

/// Each case-insensitive match of `q` in `lines`: (row, start, end), in display columns.
fn matches(lines: &[String], q: &str) -> Vec<(usize, usize, usize)> {
    let q = q.to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut v = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let low = l.to_lowercase();
        // Lowercasing keeps byte offsets for ASCII; other text is matched as is.
        let hay = if low.len() == l.len() { low.as_str() } else { l.as_str() };
        let mut from = 0;
        while let Some(k) = hay[from..].find(&q) {
            let s = from + k;
            let e = s + q.len();
            v.push((i, crate::editor::display_col(l, s), crate::editor::display_col(l, e)));
            from = e;
        }
    }
    v
}

impl App {
    /// The terminal the panel shows.
    pub fn term(&self) -> Option<&Term> {
        self.terms.get(self.term_at)
    }

    /// View: Toggle Terminal, as VS Code's: a focused terminal hides, any other shows and
    /// takes the keys (a shell started in the workspace root if none runs).
    pub fn toggle_terminal(&mut self) {
        if self.term_open && self.focus == Focus::Term {
            return self.hide_terminal();
        }
        if self.terms.is_empty() {
            return self.new_terminal();
        }
        self.prog.log_shown = false;
        (self.term_open, self.focus) = (true, Focus::Term);
    }

    /// `cmd` typed into a new terminal's shell (never one in use: it may run vim, be on
    /// another host over ssh, or hold half a command). False when no shell started.
    pub fn run_in_new_terminal(&mut self, cmd: &str) -> bool {
        let before = self.terms.len();
        self.new_terminal();
        if self.terms.len() == before {
            return false;
        }
        let at = self.terms.len() - 1;
        self.terms[at].send(format!("{cmd}\r").as_bytes());
        true
    }

    /// Children an earlier image of this process (before a reload's or a switch's exec)
    /// started and no longer waits for (a git status, a search still running then): waited
    /// for, so none lingers as a zombie. The terminals' shells are their own terminals'.
    pub fn reap_strays(&self) {
        let shells: Vec<i32> = self.terms.iter().map(|t| t.pid).collect();
        for task in fs::read_dir("/proc/self/task").into_iter().flatten().flatten() {
            let Ok(kids) = fs::read_to_string(task.path().join("children")) else { continue };
            for pid in kids.split_whitespace().filter_map(|p| p.parse::<i32>().ok()).filter(|p| !shells.contains(p)) {
                // SAFETY: waitpid on a child of this process; it blocks this thread only.
                std::thread::spawn(move || unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) });
            }
        }
    }

    /// Terminal: Create New Terminal (Ctrl+Shift+`, the panel's +): another shell, shown.
    pub fn new_terminal(&mut self) {
        // The panel's size is known at its first frame; until then, a fair guess.
        let (ed, term) = (self.ui.editor_area, self.ui.term);
        let (rows, cols) = if term.is_empty() { (self.term_h.unwrap_or(ed.height / 3).saturating_sub(2), ed.width.saturating_sub(2).max(20)) } else { (term.height, term.width) };
        match Term::spawn(&shell(), &self.root, rows, cols) {
            Ok(t) => {
                self.terms.push(t);
                self.term_at = self.terms.len() - 1;
                self.term_find = None;
                self.prog.log_shown = false;
                (self.term_open, self.focus) = (true, Focus::Term);
            }
            Err(e) => self.msg = format!("terminal: {e}"),
        }
    }

    /// Terminal: Kill the Active Terminal Instance (the panel's ×): terminal `i` closed,
    /// asked first while a program runs in it.
    pub fn kill_terminal(&mut self, i: usize, ask: bool) {
        let Some(t) = self.terms.get(i) else { return };
        if ask {
            if let Some(p) = t.running() {
                self.prompt = Some(Prompt {
                    text: vec![format!("{p} is still running in terminal {}.", i + 1), "Closing the terminal ends it.".into()],
                    buttons: vec![("Kill".into(), PAct::KillTerminal(i)), ("Cancel".into(), PAct::Cancel)],
                    sel: 1,
                });
                return;
            }
        }
        self.terms.remove(i); // its drop hangs the shell up
        self.term_find = None;
        if self.term_at >= i && self.term_at > 0 {
            self.term_at -= 1;
        }
        if self.terms.is_empty() {
            self.hide_terminal();
        }
    }

    /// Terminal: Focus Next / Previous Terminal (Ctrl+PgDn / Ctrl+PgUp in it, a tab's click).
    pub fn switch_terminal(&mut self, to: usize) {
        if to < self.terms.len() {
            self.term_at = to;
            self.term_find = None;
            self.prog.log_shown = false;
            (self.term_open, self.focus) = (true, Focus::Term);
        }
    }

    /// Hides the panel; the shells go on running.
    pub fn hide_terminal(&mut self) {
        self.term_open = false;
        if self.focus == Focus::Term {
            self.focus = Focus::Editor;
        }
    }

    /// Every shell's output since the last look (one hidden or in the background too: it
    /// must not stall on a full PTY); true when the frame changes. A shell that exited
    /// takes its tab with it, and the last one the panel.
    pub fn term_pump(&mut self) -> bool {
        let mut changed = false;
        let mut i = 0;
        while i < self.terms.len() {
            let (got, alive) = self.terms[i].pump();
            if alive {
                changed |= got && self.term_open && i == self.term_at;
                i += 1;
                continue;
            }
            changed |= self.term_open;
            self.terms.remove(i);
            if self.term_at >= i && self.term_at > 0 {
                self.term_at -= 1;
            }
        }
        // (The panel stays for the STM32 Programmer's log.)
        if self.terms.is_empty() && self.term_open && !self.prog.log_shown {
            self.term_find = None;
            self.hide_terminal();
        }
        changed
    }

    /// Terminal: Find (Ctrl+F in the shell): the find box over the panel.
    pub fn term_find_open(&mut self) {
        if self.term().is_some_and(|t| !t.full_screen()) {
            self.term_find = Some(TermFind::default());
            (self.term_open, self.focus) = (true, Focus::Term);
        }
    }

    /// The next match of the find box's text, older (`up`, as Enter goes) or newer
    /// (Shift+Enter); from the newest when none is shown. Wraps around.
    fn term_find_step(&mut self, up: bool) {
        let at = self.term_at;
        let (Some(f), Some(t)) = (self.term_find.as_mut(), self.terms.get_mut(at)) else { return };
        let lines = t.lines();
        let all = matches(&lines, &f.query);
        if all.is_empty() {
            (f.at, f.count) = (None, (0, 0));
            t.scroll_to_bottom();
            return;
        }
        let k = match f.at {
            None => all.len() - 1,
            Some(cur) if up => all.iter().rposition(|m| *m < cur).unwrap_or(all.len() - 1),
            Some(cur) => all.iter().position(|m| *m > cur).unwrap_or(0),
        };
        let m = all[k];
        (f.at, f.count) = (Some(m), (k + 1, all.len()));
        t.show(m.0, (m.1 as u16, m.2 as u16));
    }

    /// A key while the find box is open: text, Enter / Shift+Enter (older, newer), Esc.
    fn term_find_key(&mut self, k: KeyEvent) {
        let Some(f) = self.term_find.as_mut() else { return };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => {
                self.term_find = None;
                if let Some(t) = self.terms.get_mut(self.term_at) {
                    t.scroll_to_bottom();
                }
            }
            KeyCode::Enter => self.term_find_step(!k.modifiers.contains(KeyModifiers::SHIFT)),
            KeyCode::Up => self.term_find_step(true),
            KeyCode::Down => self.term_find_step(false),
            KeyCode::Backspace => {
                f.query.pop();
                f.at = None;
                self.term_find_step(true);
            }
            KeyCode::Char('v' | 'V') if ctrl => {
                let t = crate::panel::clipboard_get().unwrap_or_else(|| self.clip.clone());
                self.term_find_paste(&t);
            }
            KeyCode::Char(c) if !ctrl => {
                f.query.push(c);
                f.at = None;
                self.term_find_step(true);
            }
            _ => {}
        }
    }

    /// A paste into the find box (its first line).
    pub fn term_find_paste(&mut self, s: &str) {
        let Some(f) = self.term_find.as_mut() else { return };
        f.query.push_str(s.lines().next().unwrap_or(""));
        f.at = None;
        self.term_find_step(true);
    }

    /// A mouse event on the terminal, or on from a drag that began there; None when it is
    /// not the terminal's (the wheel goes through `wheel`). A program that asked for the
    /// mouse gets it (Shift+drag still selects); otherwise a drag selects text, a
    /// double-click a word, and letting go copies it. Some(redraw) when handled.
    pub fn term_mouse(&mut self, m: MouseEvent) -> Option<bool> {
        let area = self.ui.term;
        let p = Position::new(m.column, m.row);
        let ours = matches!(self.drag, Drag::TermSelect(_) | Drag::TermMouse);
        // A drag begun elsewhere (the panel's top edge, a scrollbar) keeps its events
        // over the terminal: the sash dragged down crosses into it.
        let theirs = !ours && !matches!(self.drag, Drag::None);
        if theirs || area.is_empty() || !(area.contains(p) || ours) || self.terms.is_empty() || self.prog.log_shown {
            return None;
        }
        let at = (p.y.clamp(area.y, area.bottom() - 1) - area.y, p.x.clamp(area.x, area.right() - 1) - area.x);
        let button = |b: MouseButton| match b {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        };
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        let t = self.terms.get_mut(self.term_at)?;
        if matches!(m.kind, MouseEventKind::Down(_)) {
            self.focus = Focus::Term;
        }
        // The program's own mouse: clicks, releases, drags (motion only while a button is down).
        let forward = t.wants_mouse() && !shift && !matches!(self.drag, Drag::TermSelect(_));
        if forward {
            match m.kind {
                MouseEventKind::Down(b) => {
                    self.drag = Drag::TermMouse;
                    t.mouse(button(b), true, false, m.modifiers, at);
                }
                MouseEventKind::Drag(b) => t.mouse(button(b), true, true, m.modifiers, at),
                MouseEventKind::Up(b) => {
                    self.drag = Drag::None;
                    t.mouse(button(b), false, false, m.modifiers, at);
                }
                _ => return None,
            }
            return Some(true);
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let double = self.last_click.is_some_and(|(t, q)| t.elapsed() < crate::panel::DOUBLE_CLICK && q == p);
                self.last_click = Some((std::time::Instant::now(), p));
                t.clear_selection();
                if double {
                    t.select_word(at);
                    self.copy_term_selection();
                } else {
                    self.drag = Drag::TermSelect(at);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Drag::TermSelect(from) = self.drag {
                    t.select(from, at);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag = Drag::None;
                self.copy_term_selection();
            }
            MouseEventKind::Down(_) => {}
            _ => return None,
        }
        Some(true)
    }

    /// The terminal's selection onto the clipboard (as a terminal copies on select).
    fn copy_term_selection(&mut self) {
        if let Some(text) = self.term().and_then(Term::selected) {
            self.msg = format!("copied via {}", crate::panel::clipboard_set(&text));
            self.clip = text;
        }
    }

    /// A key while the terminal has the focus: Toggle Terminal, F1 and Ctrl+Q stay
    /// termcode's, and VS Code's terminal keys (Ctrl+Shift+` a new one, Ctrl+PgUp/PgDn the
    /// others, Ctrl+F find while the shell has the screen); everything else is the shell's.
    pub fn term_key(&mut self, k: KeyEvent) {
        if self.term_find.is_some() {
            return self.term_find_key(k);
        }
        let chord = crate::parse::chord(&k);
        let bound = self.settings.keybindings.iter().any(|(c, n)| *c == chord && n == "View: Toggle Terminal");
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let ctrl_q = k.modifiers == KeyModifiers::CONTROL && matches!(k.code, KeyCode::Char('q' | 'Q'));
        // Ctrl+C with text selected copies it instead of interrupting.
        let ctrl_c = ctrl && matches!(k.code, KeyCode::Char('c' | 'C'));
        if ctrl_c && self.term().is_some_and(|t| t.selected().is_some()) {
            self.copy_term_selection();
            if let Some(t) = self.terms.get_mut(self.term_at) {
                t.clear_selection();
            }
            return;
        }
        let n = self.terms.len();
        if ctrl && k.modifiers.contains(KeyModifiers::SHIFT) && matches!(k.code, KeyCode::Char('`' | '~')) {
            self.new_terminal();
        } else if bound || toggle_key(&k) {
            self.toggle_terminal();
        } else if k.code == KeyCode::F(1) {
            self.open_palette();
        } else if ctrl_q {
            self.request_quit();
        } else if k.modifiers == KeyModifiers::CONTROL && matches!(k.code, KeyCode::PageDown | KeyCode::PageUp) && n > 1 {
            let d = if k.code == KeyCode::PageDown { 1 } else { n - 1 };
            self.switch_terminal((self.term_at + d) % n);
        } else if k.modifiers == KeyModifiers::CONTROL && matches!(k.code, KeyCode::Char('f')) && self.term().is_some_and(|t| !t.full_screen()) {
            self.term_find_open();
        } else if let Some(t) = self.terms.get_mut(self.term_at) {
            t.key(k);
        }
    }
}

fn color(c: vt100::Color, default: Color) -> Color {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// What xterm sends for a key: `app_cursor` is the screen's application cursor mode.
pub fn key_bytes(k: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let m = k.modifiers;
    let (ctrl, alt, shift) = (m.contains(KeyModifiers::CONTROL), m.contains(KeyModifiers::ALT), m.contains(KeyModifiers::SHIFT));
    // xterm's modifier parameter: 1 + Shift 1, Alt 2, Ctrl 4.
    let mods = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let esc = |b: &[u8]| [&b"\x1b"[..], b].concat();
    let plain = |b: &[u8]| if alt { esc(b) } else { b.to_vec() };
    let cursor = |c: char| match (mods, app_cursor) {
        (1, true) => format!("\x1bO{c}").into_bytes(),
        (1, false) => format!("\x1b[{c}").into_bytes(),
        _ => format!("\x1b[1;{mods}{c}").into_bytes(),
    };
    let tilde = |n: u8| if mods == 1 { format!("\x1b[{n}~") } else { format!("\x1b[{n};{mods}~") }.into_bytes();
    match k.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 & 0x1f,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '/' | '7' => 0x1f,
                '?' | '8' => 0x7f,
                _ => return plain(c.to_string().as_bytes()),
            };
            plain(&[b])
        }
        KeyCode::Char(c) => plain(c.to_string().as_bytes()),
        KeyCode::Enter => plain(b"\r"),
        KeyCode::Tab => plain(b"\t"),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => plain(if ctrl { b"\x08" } else { b"\x7f" }),
        KeyCode::Esc => plain(b"\x1b"),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let c = (b'P' + n - 1) as char;
            if mods == 1 { format!("\x1bO{c}").into_bytes() } else { format!("\x1b[1;{mods}{c}").into_bytes() }
        }
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_terminal_descriptor_past_stdio_is_adopted() {
        let dir = std::env::temp_dir().join(format!("termcode-adopt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(screen_file(&dir, 1), "screen").unwrap();
        assert!(Term::adopt("1,1,24,80", &dir).is_none(), "never stdout");
        assert!(!screen_file(&dir, 1).exists(), "its screen file gone all the same");
        let f = fs::File::create(dir.join("plain")).unwrap();
        assert!(Term::adopt(&format!("{},1,24,80", f.as_raw_fd()), &dir).is_none(), "never a plain file");
        assert!(Term::adopt("99,1,99999,80", &dir).is_none(), "a size past u16");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_shell_word_survives_quotes_and_spaces() {
        assert_eq!(sh_quote("/opt/my tools/gh"), "'/opt/my tools/gh'");
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn a_handed_over_terminal_is_adopted_with_its_screen_and_its_shell_still_running() {
        let dir = std::env::temp_dir().join(format!("termcode-handover-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut t = Term::spawn("/bin/sh", &dir, 6, 40).unwrap();
        let until = |t: &mut Term, what: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !t.lines().iter().any(|l| l.contains(what)) {
                assert!(Instant::now() < deadline, "no {what}: {:?}", t.lines());
                t.pump();
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        t.send(b"seq 1 20; echo before-$((1+1))\r");
        until(&mut t, "before-2");
        let spec = t.hand_over(&dir).unwrap();
        // The exec, played here: the old one is never dropped (that would close the PTY).
        let fd = t.master.as_raw_fd();
        std::mem::forget(t);
        // SAFETY: fd was just handed over and is open.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC, 0, "open across an exec");
        let mut back = Term::adopt(&spec, &dir).expect("adopted");
        let lines = back.lines();
        assert!(lines.iter().any(|l| l.trim_end() == "3") && lines.iter().any(|l| l.contains("before-2")), "scrollback and screen back: {lines:?}");
        assert!(!screen_file(&dir, fd).exists(), "its screen file read and gone");
        back.send(b"echo after-$((2+2))\r");
        until(&mut back, "after-4");
        drop(back);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keys_are_sent_as_xterm_sends_them() {
        let k = |c, m| key_bytes(KeyEvent::new(c, m), false);
        let (none, ctrl, alt, shift) = (KeyModifiers::NONE, KeyModifiers::CONTROL, KeyModifiers::ALT, KeyModifiers::SHIFT);
        assert_eq!(k(KeyCode::Char('c'), ctrl), b"\x03");
        assert_eq!(k(KeyCode::Char('D'), ctrl | shift), b"\x04");
        assert_eq!(k(KeyCode::Char('é'), none), "é".as_bytes());
        assert_eq!(k(KeyCode::Char('b'), alt), b"\x1bb");
        assert_eq!(k(KeyCode::Enter, none), b"\r");
        assert_eq!(k(KeyCode::Backspace, none), b"\x7f");
        assert_eq!(k(KeyCode::Up, none), b"\x1b[A");
        assert_eq!(key_bytes(KeyEvent::from(KeyCode::Up), true), b"\x1bOA", "application cursor mode");
        assert_eq!(k(KeyCode::Right, ctrl), b"\x1b[1;5C");
        assert_eq!(k(KeyCode::Delete, none), b"\x1b[3~");
        assert_eq!(k(KeyCode::PageUp, shift), b"\x1b[5;2~");
        assert_eq!(k(KeyCode::F(1), none), b"\x1bOP");
        assert_eq!(k(KeyCode::F(5), none), b"\x1b[15~");
    }

    #[test]
    fn mouse_reports_follow_the_programs_mode_and_encoding() {
        use vt100::{MouseProtocolEncoding as E, MouseProtocolMode as M};
        let none = KeyModifiers::NONE;
        let r = |mode, enc, b, press, motion, mods| mouse_bytes((mode, enc), b, press, motion, mods, (4, 9));
        assert_eq!(r(M::PressRelease, E::Sgr, 0, true, false, none).unwrap(), b"\x1b[<0;10;5M");
        assert_eq!(r(M::PressRelease, E::Sgr, 0, false, false, none).unwrap(), b"\x1b[<0;10;5m");
        assert_eq!(r(M::ButtonMotion, E::Sgr, 0, true, true, KeyModifiers::CONTROL).unwrap(), b"\x1b[<48;10;5M", "drag + Ctrl");
        assert_eq!(r(M::PressRelease, E::Default, 2, true, false, none).unwrap(), [0x1b, b'[', b'M', 34, 42, 37]);
        assert_eq!(r(M::PressRelease, E::Default, 0, false, false, none).unwrap(), [0x1b, b'[', b'M', 35, 42, 37], "release: 3");
        assert_eq!(r(M::Press, E::Sgr, 0, false, false, none), None, "X10 mode: presses only");
        assert_eq!(r(M::PressRelease, E::Sgr, 0, true, true, none), None, "no motion");
        assert_eq!(r(M::None, E::Sgr, 0, true, false, none), None);
    }

    #[test]
    fn a_shell_runs_on_the_pty_and_its_output_is_drawn() {
        let mut t = Term::spawn("/bin/sh", &std::env::temp_dir(), 5, 40).unwrap();
        assert!(fds().contains(&t.master.as_raw_fd()));
        t.send(b"stty size; echo hi-$((40+2))\r");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !t.contents().contains("hi-42") {
            assert!(Instant::now() < deadline, "no output: {:?}", t.contents());
            t.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(t.contents().contains("5 40"), "the shell sees the panel's size: {:?}", t.contents());
        t.resize(6, 30);
        t.send(b"stty size\r");
        while !t.contents().contains("6 30") {
            assert!(Instant::now() < deadline, "resize: {:?}", t.contents());
            t.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 6));
        let area = buf.area;
        t.draw(&mut buf, area, (Color::White, Color::Black, Color::Blue), true);
        let text: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("6 30"));
        t.send(b"exit\r");
        while t.pump().1 {
            assert!(Instant::now() < deadline, "the shell exited, the terminal is gone");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(t);
        assert!(fds().is_empty());
    }
}

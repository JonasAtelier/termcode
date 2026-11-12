//! The integrated terminal: the user's shell on a pseudo-terminal, in a panel under the
//! editor groups. vt100 keeps its screen (and 5000 rows of scrollback); each frame draws
//! that screen cell by cell. The event loop waits on the PTY beside the keyboard
//! (`fd`), so output shows as it comes, and reads it without blocking (`pump`).

use crate::panel::{App, Focus};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use std::cell::Cell;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const SCROLLBACK: usize = 5000;

thread_local! {
    /// The open terminal's PTY, for the event loop (on this thread) to wait on; -1: none.
    static FD: Cell<i32> = const { Cell::new(-1) };
}

/// The user's shell: `$SHELL`, else /bin/sh.
pub fn shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
}

pub fn fd() -> i32 {
    FD.with(|f| f.get())
}

pub struct Term {
    master: File,
    child: Option<Child>,
    parser: vt100::Parser,
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
        FD.with(|f| f.set(m));
        Ok(Term { master, child: Some(child), parser: vt100::Parser::new(rows, cols, SCROLLBACK) })
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
        self.parser.set_scrollback(0);
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
        self.parser.set_scrollback(if up { off + d.unsigned_abs() } else { off.saturating_sub(d.unsigned_abs()) });
    }

    /// Fits the screen and the shell's idea of it to `rows` x `cols`.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(2));
        if self.parser.screen().size() == (rows, cols) {
            return;
        }
        self.parser.set_size(rows, cols);
        let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: an open PTY master and a valid winsize.
        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) };
    }

    /// The screen into `area` of `buf`, its default colours `fg` on `bg`; the cursor as a
    /// reversed cell when `focused` and the live screen (not the scrollback) shows.
    pub fn draw(&self, buf: &mut Buffer, area: Rect, fg: Color, bg: Color, focused: bool) {
        let s = self.parser.screen();
        for row in 0..area.height {
            for col in 0..area.width {
                let Some(c) = s.cell(row, col) else { continue };
                let cell = &mut buf[(area.x + col, area.y + row)];
                let (mut f, mut b) = (color(c.fgcolor(), fg), color(c.bgcolor(), bg));
                if c.inverse() {
                    (f, b) = (b, f);
                }
                let text = c.contents();
                // A wide character's second cell is drawn over by its first.
                cell.set_symbol(if text.is_empty() || c.is_wide_continuation() { " " } else { &text }).set_fg(f).set_bg(b);
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

    #[cfg(test)]
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }
}

impl Drop for Term {
    /// The master closes after this, which hangs the shell up as closing a terminal
    /// window does; it is reaped in the background (no zombie, no signal sent).
    fn drop(&mut self) {
        FD.with(|f| {
            if f.get() == self.master.as_raw_fd() {
                f.set(-1);
            }
        });
        if let Some(c) = self.child.take() {
            crate::nav::reap(c, None);
        }
    }
}

/// Ctrl+` (a legacy terminal sends it as Ctrl+Space).
pub fn toggle_key(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('`' | ' '))
}

impl App {
    /// View: Toggle Terminal, as VS Code's: a focused terminal hides, any other shows and
    /// takes the keys (its shell started in the workspace root if none runs).
    pub fn toggle_terminal(&mut self) {
        if self.term_open && self.focus == Focus::Term {
            return self.hide_terminal();
        }
        if self.term.is_none() {
            // The panel's size is known at its first frame; until then, a fair guess.
            let ed = self.ui.editor_area;
            let rows = self.term_h.unwrap_or(ed.height / 3).saturating_sub(2);
            match Term::spawn(&shell(), &self.root, rows, ed.width.saturating_sub(2).max(20)) {
                Ok(t) => self.term = Some(t),
                Err(e) => {
                    self.msg = format!("terminal: {e}");
                    return;
                }
            }
        }
        (self.term_open, self.focus) = (true, Focus::Term);
    }

    /// Hides the panel; the shell goes on running.
    pub fn hide_terminal(&mut self) {
        self.term_open = false;
        if self.focus == Focus::Term {
            self.focus = Focus::Editor;
        }
    }

    /// The shell's output since the last look; true when the frame changes. A shell that
    /// exited takes its panel with it.
    pub fn term_pump(&mut self) -> bool {
        let Some(t) = self.term.as_mut() else { return false };
        let (got, alive) = t.pump();
        if alive {
            return got && self.term_open;
        }
        self.term = None;
        let shown = self.term_open;
        self.hide_terminal();
        shown
    }

    /// A key while the terminal has the focus: Toggle Terminal, F1 and Ctrl+Q stay
    /// termcode's; everything else is the shell's.
    pub fn term_key(&mut self, k: KeyEvent) {
        let chord = crate::parse::chord(&k);
        let bound = self.settings.keybindings.iter().any(|(c, n)| *c == chord && n == "View: Toggle Terminal");
        let ctrl_q = k.modifiers == KeyModifiers::CONTROL && matches!(k.code, KeyCode::Char('q' | 'Q'));
        if bound || toggle_key(&k) {
            self.toggle_terminal();
        } else if k.code == KeyCode::F(1) {
            self.open_palette();
        } else if ctrl_q {
            self.request_quit();
        } else if let Some(t) = self.term.as_mut() {
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
    fn a_shell_runs_on_the_pty_and_its_output_is_drawn() {
        let mut t = Term::spawn("/bin/sh", &std::env::temp_dir(), 5, 40).unwrap();
        assert_eq!(fd(), t.master.as_raw_fd());
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
        t.draw(&mut buf, area, Color::White, Color::Black, true);
        let text: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("6 30"));
        t.send(b"exit\r");
        while t.pump().1 {
            assert!(Instant::now() < deadline, "the shell exited, the terminal is gone");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(t);
        assert_eq!(fd(), -1);
    }
}

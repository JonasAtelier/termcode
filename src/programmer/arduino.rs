//! The Programmer for Arduino sketches, through `arduino-cli`: the boards it sees (their
//! ports and FQBNs), one chosen; a sketch compiled and uploaded for it (any board its
//! installed cores know: AVR, ESP32, RP2040…); the Serial Console (shared) on its port.

use super::*;

/// The Arduino features: their names, what each page does.
pub const FEATURES: [(&str, &str); 3] = [
    ("Boards", "The boards arduino-cli sees: their ports and FQBNs; pick yours."),
    ("Sketch", "Compile a sketch for the board and upload it."),
    ("Serial Console", "The board's serial output (its Serial.print), and lines sent back."),
];
pub const SKETCH: usize = 1;
pub const SERIAL: usize = 2;
const INSTALL: &str = "https://arduino.github.io/arduino-cli/latest/installation/";

/// A port arduino-cli lists, with the board it knows there (if any).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Board {
    pub port: String,
    pub protocol: String,
    /// "Arduino Uno"; empty: a port with no board it knows.
    pub name: String,
    pub fqbn: String,
}

#[derive(Default)]
pub struct Arduino {
    pub cli: Option<PathBuf>,
    pub(super) checked: bool,
    pub(super) installing: Option<Instant>,
    pub feature: usize,
    /// None until listed.
    pub boards: Option<Vec<Board>>,
    pub board: Option<usize>,
    /// The sketch (its folder, holding the .ino).
    pub sketch: Option<PathBuf>,
    /// The board's FQBN ("arduino:avr:uno"): the chosen board's, or typed.
    pub fqbn: String,
    pub verify: bool,
}

impl Arduino {
    /// The chosen board.
    pub fn chosen(&self) -> Option<&Board> {
        self.boards.as_ref()?.get(self.board?)
    }
}

/// arduino-cli on PATH, or where its installer puts it.
pub(super) fn detect() -> Option<PathBuf> {
    if cfg!(test) {
        return None; // tests bring their own
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .chain([home.join(".local/bin"), home.join("bin")])
        .map(|d| d.join("arduino-cli"))
        .find(|p| is_exe(p))
}

pub(super) fn ensure_checked(app: &mut App) {
    if !app.prog.ard.checked {
        app.prog.ard.checked = true;
        app.prog.ard.cli = detect();
    }
}

/// `arduino-cli board list --format json`: each port, the first board known there.
pub fn parse_boards(json: &str) -> Vec<Board> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    // 1.x: {"detected_ports": [...]}; before: [...].
    let list = v.get("detected_ports").and_then(|d| d.as_array()).or_else(|| v.as_array()).cloned().unwrap_or_default();
    list.iter()
        .map(|e| {
            let p = &e["port"];
            let b = e["matching_boards"].get(0).or_else(|| e["boards"].get(0));
            let s = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
            Board { port: s(&p["address"]), protocol: s(&p["protocol"]), name: b.map(|b| s(&b["name"])).unwrap_or_default(), fqbn: b.map(|b| s(&b["fqbn"])).unwrap_or_default() }
        })
        .filter(|b| !b.port.is_empty())
        .collect()
}

/// The boards listed (a job's end): the chosen one kept by its port.
pub(super) fn boards_came(a: &mut Arduino, out: &[String]) -> String {
    let was = a.chosen().map(|b| b.port.clone());
    let mut list = parse_boards(&out.join("\n"));
    // Boards first, then bare ports.
    list.sort_by_key(|b| (b.name.is_empty(), b.port.clone()));
    a.board = was.and_then(|p| list.iter().position(|b| b.port == p)).or_else(|| list.iter().position(|b| !b.name.is_empty()));
    if let Some(b) = a.board.and_then(|i| list.get(i)).filter(|b| !b.fqbn.is_empty() && a.fqbn.is_empty()) {
        a.fqbn = b.fqbn.clone();
    }
    let known = list.iter().filter(|b| !b.name.is_empty()).count();
    let ports = list.len();
    a.boards = Some(list);
    match known {
        0 => format!("No Arduino board found ({ports} serial ports)"),
        1 => "Found an Arduino board".into(),
        n => format!("Found {n} Arduino boards"),
    }
}

/// A board row chosen: its FQBN taken, when arduino-cli knows the board.
pub(super) fn choose(a: &mut Arduino, i: usize) {
    a.board = Some(i);
    if let Some(f) = a.chosen().map(|b| b.fqbn.clone()).filter(|f| !f.is_empty()) {
        a.fqbn = f;
    }
}

/// Is `dir` a sketch (an .ino in it)?
fn is_sketch(dir: &Path) -> bool {
    fs::read_dir(dir).into_iter().flatten().flatten().any(|e| e.path().extension().is_some_and(|x| x == "ino"))
}

/// The sketch unless one is chosen: the workspace, or the open .ino's folder.
pub(super) fn ensure_sketch(app: &mut App) {
    if app.prog.ard.sketch.is_some() {
        return;
    }
    let open = app.groups.iter().flat_map(|g| g.tabs.iter()).filter_map(|t| t.key.strip_prefix("file:")).map(|rel| app.root.join(rel)).find(|p| p.extension().is_some_and(|x| x == "ino")).and_then(|p| p.parent().map(Path::to_path_buf));
    app.prog.ard.sketch = Some(app.root.clone()).filter(|r| is_sketch(r)).or(open);
}

/// `arduino-cli` for `op`; None without the tool.
pub(super) fn command(s: &Programmer, op: Op) -> Result<Option<Cmdline>, String> {
    let Some(cli) = s.ard.cli.clone() else { return Ok(None) };
    let a = &s.ard;
    let (args, what): (Vec<String>, String) = match op {
        Op::Boards => (["board", "list", "--format", "json"].map(String::from).to_vec(), "Looking for Arduino boards".into()),
        _ => {
            let dir = a.sketch.clone().ok_or("Choose the sketch first (its folder, with the .ino)")?;
            if a.fqbn.trim().is_empty() {
                return Err("Type the board's FQBN first (e.g. arduino:avr:uno)".into());
            }
            let name = dir.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            let mut v = vec!["compile".to_string()];
            let what = if op == Op::Upload {
                let port = a.chosen().map(|b| b.port.clone()).ok_or("Choose the board's port first (Boards)")?;
                v.extend(["--upload".into(), "-p".into(), port]);
                if a.verify {
                    v.push("--verify".into());
                }
                format!("Uploading {name}")
            } else {
                format!("Compiling {name}")
            };
            v.extend(["-b".into(), a.fqbn.trim().to_string(), dir.display().to_string()]);
            (v, what)
        }
    };
    let shown = shown_cmd("arduino-cli", &args);
    Ok(Some(Cmdline { program: cli, args, what, tmp: None, shown }))
}

/// "Sketch uses 924 bytes (2%) of program storage space…" as "924 bytes (2%) of flash".
pub(super) fn summary(out: &[String]) -> Option<String> {
    let l = out.iter().find(|l| l.starts_with("Sketch uses "))?;
    let used = l.strip_prefix("Sketch uses ")?.split(" of ").next()?;
    Some(format!("{used} of flash"))
}

/// The view shows Arduino: the tool looked for; with it, the page and the boards.
pub(super) fn shown(app: &mut App) {
    ensure_checked(app);
    if app.prog.ard.cli.is_none() {
        return;
    }
    ensure_sketch(app);
    open_page(app);
    if app.prog.ard.boards.is_none() && app.prog.job.is_none() {
        run(app, Op::Boards);
    }
}

pub(super) fn install(app: &mut App) {
    app.prog.log_shown = false;
    // Arduino's own installer, into ~/.local/bin (on PATH here).
    let cmd = format!("sh -c 'curl -fsSL https://raw.githubusercontent.com/arduino/arduino-cli/master/install.sh | BINDIR=\"$HOME/.local/bin\" sh || echo \"See {INSTALL}\"'");
    if app.run_in_new_terminal(&cmd) {
        app.prog.ard.installing = Some(Instant::now());
    }
}

pub(super) fn recheck(app: &mut App) {
    app.prog.ard.cli = detect();
    app.prog.ard.checked = true;
    if app.prog.ard.cli.is_some() {
        app.prog.ard.installing = None;
        shown(app);
    } else {
        app.msg = "arduino-cli is still not found".into();
    }
}

/// F5: the page's main action.
pub(super) fn primary(app: &mut App) {
    match app.prog.ard.feature {
        SERIAL => serial_toggle(app),
        SKETCH => run(app, Op::Upload),
        _ => run(app, Op::Boards),
    }
}

/// Keys in the view: ↑↓ choose a feature, Enter goes to its page, F5 its action.
pub(super) fn key(app: &mut App, k: KeyEvent) -> bool {
    if app.prog.ard.cli.is_none() {
        if k.code == KeyCode::Enter {
            install(app);
            return true;
        }
        return false;
    }
    let a = &mut app.prog.ard;
    match k.code {
        KeyCode::Up => a.feature = a.feature.saturating_sub(1),
        KeyCode::Down => a.feature = (a.feature + 1).min(FEATURES.len() - 1),
        KeyCode::Enter | KeyCode::Char(' ') => app.focus = Focus::Editor,
        KeyCode::F(5) => primary(app),
        _ => return false,
    }
    open_page(app);
    true
}

/// The view under the switch: the board, the features, the tool; or how to install it.
pub(super) fn draw_side(app: &mut App, f: &mut Frame, area: Rect, card_r: Rect, focused: bool) {
    let dim = Style::new().fg(P.dim);
    if app.prog.ard.cli.is_none() {
        let note = if app.prog.ard.installing.is_some() {
            Line::styled("Finish installing in the terminal below: this view looks for arduino-cli every few seconds.", Style::new().fg(P.git_modified))
        } else {
            Line::styled("Install runs Arduino's installer in the terminal below (into ~/.local/bin).", dim)
        };
        let lines = vec![Line::styled("arduino-cli is not installed.", Style::new().bold()), Line::raw(""), Line::raw("termcode lists, builds for and uploads to Arduino boards with Arduino's command line tool, arduino-cli."), Line::raw(""), note];
        let p = Paragraph::new(lines).wrap(Wrap { trim: true }).style(Style::new().fg(P.sidebar_fg));
        let h = p.line_count(area.width) as u16;
        f.render_widget(p, area);
        let y = area.y + h + 1;
        let x = button(app, f, area, (area.x, y), glyph(I_SAVE), "Install", Look::Primary, true, Hit::ArdInstall, P.sidebar_bg);
        button(app, f, area, (x, y), glyph(I_REFRESH), "Check Again", Look::Secondary, true, Hit::ArdRecheck, P.sidebar_bg);
        return;
    }
    let a = &app.prog.ard;
    let (name, port) = match a.chosen() {
        Some(b) => (if b.name.is_empty() { "A serial port".to_string() } else { b.name.clone() }, b.port.clone()),
        None => ("No board chosen".to_string(), String::new()),
    };
    let mut y = area.y;
    text_at(f, area, (area.x, y), vec![Span::styled(fit(&name, area.width as usize), Style::new().fg(P.sidebar_fg).bold())]);
    y += 1;
    if !port.is_empty() {
        text_at(f, area, (area.x, y), vec![Span::styled(fit(&port, area.width as usize), dim)]);
    }
    y += 2;
    let op = app.prog.job.as_ref().map(|j| j.op);
    let rows: Vec<(&str, &str, Option<ratatui::style::Color>)> = vec![
        (icons().chip, FEATURES[0].0, (op == Some(Op::Boards)).then_some(P.git_modified)),
        (glyph(I_PROGRAM), FEATURES[1].0, matches!(op, Some(Op::Compile | Op::Upload)).then_some(P.git_modified)),
        (glyph(I_TERMINAL), FEATURES[2].0, app.prog.serial.is_some().then_some(P.git_added)),
    ];
    y = feature_rows(app, f, area, card_r, focused, y, &rows, app.prog.ard.feature, Hit::ArdFeature) + 1;
    let tool = app.prog.ard.cli.as_deref().map(tilde).unwrap_or_default();
    let mut lines = Vec::new();
    if let Some(p) = &app.prog.serial {
        lines.extend([Line::styled("SERIAL", dim.bold()), Line::styled(format!("● {} at {} baud", tty_name(&p.path), p.baud), Style::new().fg(P.git_added)), Line::raw("")]);
    }
    lines.extend([Line::styled("TOOL", dim.bold()), Line::styled(tool, dim)]);
    if y < area.bottom() {
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), Rect::new(area.x, y, area.width, area.bottom() - y));
    }
}

/// The page: the chosen board, the feature's title, the running job, its cards.
pub(super) fn draw_page(app: &mut App, f: &mut Frame, b: Rect) {
    ensure_checked(app);
    if app.prog.ard.cli.is_none() {
        text_at(f, b, (b.x + 1, b.y + 1), vec![Span::styled("arduino-cli is not installed: the Programmer view in the sidebar installs it.", Style::new().fg(P.dim))]);
        return;
    }
    let mut y = b.y;
    let bar_h = 4.min(b.height);
    board_bar(app, f, Rect::new(b.x, y, b.width, bar_h));
    y += bar_h + 1;
    let i = app.prog.ard.feature.min(FEATURES.len() - 1);
    let (title, about) = FEATURES[i];
    let icon = [icons().chip, glyph(I_PROGRAM), glyph(I_TERMINAL)][i];
    text_at(f, b, (b.x + 1, y), vec![Span::styled(format!("{icon} "), Style::new().fg(P.focus_border)), Span::styled(title.to_string(), Style::new().fg(P.editor_fg).bold()), Span::styled(format!("   {about}"), Style::new().fg(P.dim))]);
    y += 2;
    if y < b.bottom() {
        y += job_card(app, f, Rect::new(b.x, y, b.width, b.bottom() - y));
    }
    if y + 3 > b.bottom() {
        return;
    }
    let rest = Rect::new(b.x, y, b.width, b.bottom() - y);
    match i {
        SERIAL => page_serial(app, f, rest),
        SKETCH => page_sketch(app, f, rest),
        _ => page_boards(app, f, rest),
    }
}

/// The page's top: the chosen board (its name, port, FQBN), Refresh.
fn board_bar(app: &mut App, f: &mut Frame, r: Rect) {
    let inner = card(f, r, &format!("{} Arduino", icons().chip), P.widget_border);
    if inner.height == 0 {
        return;
    }
    let on = app.prog.job.is_none();
    let bw = format!("{} Refresh", glyph(I_REFRESH)).width() as u16 + 2;
    let bx = inner.right().saturating_sub(bw);
    button(app, f, inner, (bx, inner.y), glyph(I_REFRESH), "Refresh", Look::Secondary, on, Hit::Run(Op::Boards), P.editor_bg);
    let room = bx.saturating_sub(inner.x + 1) as usize;
    let a = &app.prog.ard;
    let (title, details) = match a.chosen() {
        Some(bd) if !bd.name.is_empty() => (Span::styled(fit(&bd.name, room), Style::new().fg(P.editor_fg).bold()), format!("{} · {} · {}", bd.port, bd.fqbn, bd.protocol)),
        Some(bd) => (Span::styled(fit("A serial port (no board arduino-cli knows)", room), Style::new().fg(P.editor_fg)), format!("{} · {}", bd.port, bd.protocol)),
        None => (Span::styled(fit("No board chosen", room), Style::new().fg(P.dim)), "Plug a board in, then Refresh (F5 on Boards).".to_string()),
    };
    text_at(f, inner, (inner.x, inner.y), vec![title]);
    if inner.height > 1 {
        text_at(f, inner, (inner.x, inner.y + 1), vec![Span::styled(fit(&details, inner.width as usize), Style::new().fg(P.dim))]);
    }
}

/// The boards and ports, one a row, the chosen one marked; a click chooses.
fn page_boards(app: &mut App, f: &mut Frame, r: Rect) {
    let c = card(f, r, "Boards and ports", P.widget_border);
    if c.height == 0 {
        return;
    }
    let a = &app.prog.ard;
    let Some(list) = a.boards.clone() else {
        text_at(f, c, (c.x, c.y), vec![Span::styled("Refresh (F5) lists the boards.", Style::new().fg(P.dim))]);
        return;
    };
    if list.is_empty() {
        text_at(f, c, (c.x, c.y), vec![Span::styled("No serial port: plug a board in, then Refresh.", Style::new().fg(P.dim))]);
        return;
    }
    let chosen = a.board;
    let pw = list.iter().map(|b| b.port.width()).max().unwrap_or(0);
    for (i, bd) in list.iter().enumerate() {
        let y = c.y + i as u16;
        if y >= c.bottom() {
            break;
        }
        let rr = Rect::new(c.x, y, c.width, 1);
        let hot = app.hover.is_some_and(|q| rr.contains(q));
        let on = chosen == Some(i);
        let ul = if hot { Modifier::UNDERLINED } else { Modifier::empty() };
        let (name, nc) = if bd.name.is_empty() { ("no board arduino-cli knows".to_string(), P.dim) } else { (bd.name.clone(), P.editor_fg) };
        text_at(
            f,
            c,
            (c.x, y),
            vec![
                Span::styled(format!("{} ", if on { "◉" } else { "○" }), Style::new().fg(if on { P.focus_border } else { P.dim })),
                Span::styled(format!("{:<pw$}  ", bd.port), Style::new().fg(P.editor_fg).add_modifier(ul)),
                Span::styled(name, Style::new().fg(nc).add_modifier(ul)),
                Span::styled(if bd.fqbn.is_empty() { String::new() } else { format!("  {}", bd.fqbn) }, Style::new().fg(P.dim)),
            ],
        );
        app.prog.hits.push((rr, Hit::Board(i)));
    }
}

/// The sketch, the board it is for (FQBN, typed in place) and its port; Compile, Upload.
fn page_sketch(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (top, r) = take(r, 6);
    let c = card(f, top, "Sketch", P.widget_border);
    if c.height == 0 {
        return;
    }
    let bx = c.right().saturating_sub(12);
    button(app, f, c, (bx, c.y), glyph(I_FOLDER), "Browse…", Look::Secondary, true, Hit::BrowseSketch, P.editor_bg);
    let a = &app.prog.ard;
    let sketch = a.sketch.clone();
    let (fqbn, port) = (a.fqbn.clone(), a.chosen().map(|b| b.port.clone()));
    label(f, c, (c.x, c.y), "Folder");
    let what = sketch.as_deref().map(tilde).unwrap_or_default();
    field(app, f, c, (c.x + 9, c.y), bx.saturating_sub(c.x + 11).max(10), &what, "No sketch: Browse… picks its .ino", Hit::BrowseSketch);
    label(f, c, (c.x, c.y + 1), "Board");
    let x = field(app, f, c, (c.x + 9, c.y + 1), 30, &fqbn, "FQBN, e.g. arduino:avr:uno", Hit::Ask(Field::Fqbn));
    label(f, c, (x + 1, c.y + 1), "the chosen board's, or type one");
    label(f, c, (c.x, c.y + 3), "Port");
    let (pt, pc) = match &port {
        Some(p) => (p.clone(), P.editor_fg),
        None => ("none: choose the board on Boards".into(), P.dim),
    };
    text_at(f, c, (c.x + 10, c.y + 3), vec![Span::styled(pt, Style::new().fg(pc))]);
    let v = app.prog.ard.verify;
    check(app, f, c, (c.x + 40, c.y + 3), v, "Verify after upload", Hit::ArdVerify);
    if r.height == 0 {
        return;
    }
    let (acts, r) = take(r, 1);
    let ready = on && sketch.is_some() && !fqbn.trim().is_empty();
    let x = button(app, f, acts, (acts.x + 1, acts.y), glyph(I_REFRESH), "Compile", Look::Secondary, ready, Hit::Run(Op::Compile), P.editor_bg);
    let x = button(app, f, acts, (x, acts.y), glyph(I_PROGRAM), "Upload", Look::Primary, ready && port.is_some(), Hit::Run(Op::Upload), P.editor_bg);
    key_hint(f, acts, (x, acts.y), "F5");
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Command", P.widget_border);
    let op = if port.is_some() { Op::Upload } else { Op::Compile };
    if let Ok(Some(cmd)) = command(&app.prog, op) {
        let p = Paragraph::new(Line::from(vec![Span::styled("$ ", Style::new().fg(P.dim)), Span::styled(cmd.shown, Style::new().fg(P.editor_fg))])).wrap(Wrap { trim: false });
        f.render_widget(p, c);
    }
}

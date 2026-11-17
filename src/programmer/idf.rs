//! The Programmer for ESP-IDF projects: `idf.py` (its environment from the framework's
//! export.sh) builds, flashes and erases; set-target picks the chip. The Serial Console
//! (shared) watches the board.

use super::*;

pub const FEATURES: [(&str, &str); 2] = [
    ("Project", "Build an ESP-IDF project and flash it: any chip the framework supports."),
    ("Serial Console", "The board's serial output (its printf, ESP_LOG), and lines sent back."),
];
pub const SERIAL: usize = 1;
/// The chips set-target takes.
pub const TARGETS: [&str; 11] = ["esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61", "esp32h2", "esp32p4", "esp32h21"];
const INSTALL: &str = "https://docs.espressif.com/projects/esp-idf/en/stable/esp32/get-started/linux-macos-setup.html";

#[derive(Default)]
pub struct Idf {
    /// The framework (its export.sh).
    pub root: Option<PathBuf>,
    pub(super) checked: bool,
    pub(super) installing: Option<Instant>,
    pub feature: usize,
    /// The project (its folder: CMakeLists.txt, main/).
    pub project: Option<PathBuf>,
    /// Its name and chip, as its files say.
    pub name: String,
    pub target: Option<String>,
    /// The chip asked for (set-target, once confirmed).
    pub want: String,
}

/// The framework: $IDF_PATH, where Espressif's installer recorded one, or the usual places.
pub(super) fn detect() -> Option<PathBuf> {
    if cfg!(test) {
        return None; // tests bring their own
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let recorded: Vec<PathBuf> = fs::read_to_string(home.join(".espressif/idf-env.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["idfInstalled"].as_object().map(|m| m.values().filter_map(|i| i["path"].as_str().map(PathBuf::from)).collect()))
        .unwrap_or_default();
    std::env::var_os("IDF_PATH").map(PathBuf::from).into_iter().chain(recorded).chain([home.join("esp/esp-idf"), home.join("esp32/esp-idf"), PathBuf::from("/opt/esp-idf")]).find(|d| d.join("export.sh").is_file())
}

pub(super) fn ensure_checked(app: &mut App) {
    if !app.prog.idf.checked {
        app.prog.idf.checked = true;
        app.prog.idf.root = detect();
    }
}

/// Is `dir` an ESP-IDF project (its CMakeLists.txt takes IDF's project.cmake)?
pub fn is_project(dir: &Path) -> bool {
    fs::read_to_string(dir.join("CMakeLists.txt")).is_ok_and(|t| t.contains("project.cmake"))
}

/// The project's name (its project(...)) and chip (sdkconfig's CONFIG_IDF_TARGET).
pub(super) fn read_project(i: &mut Idf) {
    let Some(p) = i.project.clone() else { return };
    let cm = fs::read_to_string(p.join("CMakeLists.txt")).unwrap_or_default();
    i.name = cm.lines().find_map(|l| l.trim().strip_prefix("project(")).map(|r| r.trim_end_matches(')').split_whitespace().next().unwrap_or("").to_string()).unwrap_or_default();
    i.target = fs::read_to_string(p.join("sdkconfig")).ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("CONFIG_IDF_TARGET=").map(|v| v.trim_matches('"').to_string())));
}

/// The project unless one is chosen: the workspace, or the folder above the open file
/// that is one.
pub(super) fn ensure_project(app: &mut App) {
    if app.prog.idf.project.is_some() {
        return;
    }
    let open = app.groups.iter().flat_map(|g| g.tabs.iter()).filter_map(|t| t.key.strip_prefix("file:")).map(|rel| app.root.join(rel));
    let found = std::iter::once(app.root.clone()).chain(open.flat_map(|p| p.ancestors().skip(1).map(Path::to_path_buf).collect::<Vec<_>>())).find(|d| is_project(d));
    if found.is_some() {
        app.prog.idf.project = found;
        read_project(&mut app.prog.idf);
    }
}

/// `idf.py` for `op`: through bash, after the framework's export.sh.
pub(super) fn command(s: &Programmer, op: Op) -> Result<Option<Cmdline>, String> {
    let Some(root) = s.idf.root.clone() else { return Ok(None) };
    let proj = s.idf.project.clone().ok_or("Choose the ESP-IDF project first (its folder)")?;
    let mut args = vec!["-C".to_string(), proj.display().to_string()];
    let port = || flash_port(s).ok_or("Choose the board's port first (Port)");
    let what = match op {
        Op::IdfBuild => {
            args.push("build".into());
            format!("Building {}", s.idf.name_or(&proj))
        }
        Op::IdfFlash => {
            args.extend(["-p".into(), port()?.display().to_string(), "flash".into()]);
            format!("Flashing {}", s.idf.name_or(&proj))
        }
        Op::IdfErase => {
            args.extend(["-p".into(), port()?.display().to_string(), "erase-flash".into()]);
            "Erasing the flash".into()
        }
        _ => {
            args.extend(["set-target".into(), s.idf.want.clone()]);
            format!("Setting the target to {}", s.idf.want)
        }
    };
    let shown = shown_cmd("idf.py", &args);
    let script = ". \"$0/export.sh\" >/dev/null 2>&1 || { echo \"Error: $0/export.sh failed: run its install.sh\"; exit 2; }; exec idf.py \"$@\"";
    let mut bash = vec!["-c".to_string(), script.to_string(), root.display().to_string()];
    bash.extend(args);
    Ok(Some(Cmdline { program: PathBuf::from("bash"), args: bash, what, tmp: None, shown }))
}

impl Idf {
    fn name_or(&self, proj: &Path) -> String {
        if self.name.is_empty() { proj.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()) } else { self.name.clone() }
    }
}

/// The port Flash and Erase use: the console's.
pub(super) fn flash_port(s: &Programmer) -> Option<PathBuf> {
    s.tty.clone().or_else(|| default_tty(s))
}

/// "… binary size 0x2c5d0 bytes. Smallest app partition is 0x100000 bytes. 0xd3a30 bytes
/// (83%) free." as "83% of the app partition free".
pub(super) fn summary(out: &[String]) -> Option<String> {
    let l = out.iter().rev().find(|l| l.contains("Smallest app partition"))?;
    let pct = l.rsplit_once('(')?.1.split_once(')')?.0;
    Some(format!("{pct} of the app partition free"))
}

/// The view shows ESP-IDF: the framework looked for, the project found, the page.
pub(super) fn shown(app: &mut App) {
    ensure_checked(app);
    if app.prog.idf.root.is_none() {
        return;
    }
    ensure_project(app);
    open_page(app);
}

pub(super) fn install(app: &mut App) {
    app.prog.log_shown = false;
    // Espressif's way: the framework from git, then its install.sh (the toolchains).
    let cmd = format!("sh -c 'git clone --depth 1 -b v5.5 --recursive --shallow-submodules https://github.com/espressif/esp-idf.git \"$HOME/esp/esp-idf\" && \"$HOME/esp/esp-idf/install.sh\" all || echo \"See {INSTALL}\"'");
    if app.run_in_new_terminal(&cmd) {
        app.prog.idf.installing = Some(Instant::now());
    }
}

pub(super) fn recheck(app: &mut App) {
    app.prog.idf.root = detect();
    app.prog.idf.checked = true;
    if app.prog.idf.root.is_some() {
        app.prog.idf.installing = None;
        shown(app);
    } else {
        app.msg = "ESP-IDF is still not found".into();
    }
}

/// F5: Flash (the Serial Console: connect or disconnect).
pub(super) fn primary(app: &mut App) {
    match app.prog.idf.feature {
        SERIAL => serial_toggle(app),
        _ => run(app, Op::IdfFlash),
    }
}

pub(super) fn key(app: &mut App, k: KeyEvent) -> bool {
    if app.prog.idf.root.is_none() {
        if k.code == KeyCode::Enter {
            install(app);
            return true;
        }
        return false;
    }
    let i = &mut app.prog.idf;
    match k.code {
        KeyCode::Up => i.feature = i.feature.saturating_sub(1),
        KeyCode::Down => i.feature = (i.feature + 1).min(FEATURES.len() - 1),
        KeyCode::Enter | KeyCode::Char(' ') => app.focus = Focus::Editor,
        KeyCode::F(5) => primary(app),
        _ => return false,
    }
    open_page(app);
    true
}

/// Set Target: the chips, the one now marked.
pub(super) fn pick_target(app: &mut App) {
    let now = app.prog.idf.target.clone();
    let items = TARGETS.iter().map(|t| Item { label: t.to_string(), detail: if now.as_deref() == Some(*t) { "now".into() } else { String::new() }, pick: Pick::Prog(Browse::Target(t.to_string())) }).collect();
    crate::gitcmds::open_picker(app, "Target chip (set-target starts the configuration over)", items, None);
}

pub(super) fn draw_side(app: &mut App, f: &mut Frame, area: Rect, card_r: Rect, focused: bool) {
    let dim = Style::new().fg(P.dim);
    if app.prog.idf.root.is_none() {
        let note = if app.prog.idf.installing.is_some() {
            Line::styled("Finish installing in the terminal below: this view looks for ESP-IDF every few seconds.", Style::new().fg(P.git_modified))
        } else {
            Line::styled("Install fetches ESP-IDF v5.5 into ~/esp/esp-idf and runs its install.sh in the terminal below (a large download).", dim)
        };
        let lines = vec![Line::styled("ESP-IDF is not installed.", Style::new().bold()), Line::raw(""), Line::raw("termcode builds and flashes ESP-IDF projects with the framework's idf.py."), Line::raw(""), note];
        let p = Paragraph::new(lines).wrap(Wrap { trim: true }).style(Style::new().fg(P.sidebar_fg));
        let h = p.line_count(area.width) as u16;
        f.render_widget(p, area);
        let y = area.y + h + 1;
        let x = button(app, f, area, (area.x, y), glyph(I_SAVE), "Install", Look::Primary, true, Hit::IdfInstall, P.sidebar_bg);
        button(app, f, area, (x, y), glyph(I_REFRESH), "Check Again", Look::Secondary, true, Hit::IdfRecheck, P.sidebar_bg);
        return;
    }
    let i = &app.prog.idf;
    let (name, chip) = match &i.project {
        Some(p) => (i.name_or(p), i.target.clone().unwrap_or_else(|| "no target set".into())),
        None => ("No project chosen".to_string(), String::new()),
    };
    let mut y = area.y;
    text_at(f, area, (area.x, y), vec![Span::styled(fit(&name, area.width as usize), Style::new().fg(P.sidebar_fg).bold())]);
    y += 1;
    text_at(f, area, (area.x, y), vec![Span::styled(fit(&chip, area.width as usize), dim)]);
    y += 2;
    let running = app.prog.job.as_ref().is_some_and(|j| j.op.family() == Family::Idf);
    let rows: Vec<(&str, &str, Option<ratatui::style::Color>)> = vec![(glyph(I_PROGRAM), FEATURES[0].0, running.then_some(P.git_modified)), (glyph(I_TERMINAL), FEATURES[1].0, app.prog.serial.is_some().then_some(P.git_added))];
    y = feature_rows(app, f, area, card_r, focused, y, &rows, app.prog.idf.feature, Hit::IdfFeature) + 1;
    let mut lines = Vec::new();
    if let Some(p) = &app.prog.serial {
        lines.extend([Line::styled("SERIAL", dim.bold()), Line::styled(format!("● {} at {} baud", tty_name(&p.path), p.baud), Style::new().fg(P.git_added)), Line::raw("")]);
    }
    lines.extend([Line::styled("FRAMEWORK", dim.bold()), Line::styled(app.prog.idf.root.as_deref().map(tilde).unwrap_or_default(), dim)]);
    if y < area.bottom() {
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), Rect::new(area.x, y, area.width, area.bottom() - y));
    }
}

pub(super) fn draw_page(app: &mut App, f: &mut Frame, b: Rect) {
    ensure_checked(app);
    if app.prog.idf.root.is_none() {
        text_at(f, b, (b.x + 1, b.y + 1), vec![Span::styled("ESP-IDF is not installed: the Programmer view in the sidebar installs it.", Style::new().fg(P.dim))]);
        return;
    }
    let mut y = b.y;
    let bar_h = 4.min(b.height);
    project_bar(app, f, Rect::new(b.x, y, b.width, bar_h));
    y += bar_h + 1;
    let n = app.prog.idf.feature.min(FEATURES.len() - 1);
    let (title, about) = FEATURES[n];
    let icon = if n == SERIAL { glyph(I_TERMINAL) } else { glyph(I_PROGRAM) };
    text_at(f, b, (b.x + 1, y), vec![Span::styled(format!("{icon} "), Style::new().fg(P.focus_border)), Span::styled(title.to_string(), Style::new().fg(P.editor_fg).bold()), Span::styled(format!("   {about}"), Style::new().fg(P.dim))]);
    y += 2;
    if y < b.bottom() {
        y += job_card(app, f, Rect::new(b.x, y, b.width, b.bottom() - y));
    }
    if y + 3 > b.bottom() {
        return;
    }
    let rest = Rect::new(b.x, y, b.width, b.bottom() - y);
    match n {
        SERIAL => page_serial(app, f, rest),
        _ => page_project(app, f, rest),
    }
}

/// The page's top: the project (its name, chip, folder).
fn project_bar(app: &mut App, f: &mut Frame, r: Rect) {
    let inner = card(f, r, &format!("{} ESP-IDF", icons().chip), P.widget_border);
    if inner.height == 0 {
        return;
    }
    let i = &app.prog.idf;
    let (title, details) = match &i.project {
        Some(p) => (Span::styled(fit(&i.name_or(p), inner.width as usize), Style::new().fg(P.editor_fg).bold()), format!("{} · {}", i.target.clone().unwrap_or_else(|| "no target set".into()), tilde(p))),
        None => (Span::styled("No project chosen", Style::new().fg(P.dim)), "Browse… (Project) picks one: a folder with ESP-IDF's CMakeLists.txt.".into()),
    };
    text_at(f, inner, (inner.x, inner.y), vec![title]);
    if inner.height > 1 {
        text_at(f, inner, (inner.x, inner.y + 1), vec![Span::styled(fit(&details, inner.width as usize), Style::new().fg(P.dim))]);
    }
}

fn page_project(app: &mut App, f: &mut Frame, r: Rect) {
    let on = app.prog.job.is_none();
    let (top, r) = take(r, 5);
    let c = card(f, top, "Project", P.widget_border);
    if c.height == 0 {
        return;
    }
    let bx = c.right().saturating_sub(12);
    button(app, f, c, (bx, c.y), glyph(I_FOLDER), "Browse…", Look::Secondary, true, Hit::BrowseProject, P.editor_bg);
    let proj = app.prog.idf.project.clone();
    label(f, c, (c.x, c.y), "Folder");
    let what = proj.as_deref().map(tilde).unwrap_or_default();
    field(app, f, c, (c.x + 9, c.y), bx.saturating_sub(c.x + 11).max(10), &what, "No project: Browse… picks its CMakeLists.txt", Hit::BrowseProject);
    label(f, c, (c.x, c.y + 1), "Target");
    let target = app.prog.idf.target.clone().unwrap_or_default();
    let x = field(app, f, c, (c.x + 9, c.y + 1), 14, &format!("{target}  {}", icons().chev_down), "not set", Hit::IdfTargetPick);
    label(f, c, (x + 1, c.y + 1), "set-target starts the configuration (sdkconfig) over");
    label(f, c, (c.x, c.y + 2), "Port");
    let port = flash_port(&app.prog);
    let pw = c.right().saturating_sub(c.x + 11).clamp(10, 60);
    let pt = port.as_ref().map_or(String::new(), |p| {
        let name = crate::serial::ports().into_iter().find(|(q, _)| q == p).map_or_else(|| tty_name(p), |(_, w)| w);
        format!("{name}  {}", icons().chev_down)
    });
    field(app, f, c, (c.x + 9, c.y + 2), pw, &pt, "No serial port found: plug the board in", Hit::Tty);
    if r.height == 0 {
        return;
    }
    let (acts, r) = take(r, 1);
    let ready = on && proj.is_some();
    let x = button(app, f, acts, (acts.x + 1, acts.y), glyph(I_REFRESH), "Build", Look::Secondary, ready, Hit::Run(Op::IdfBuild), P.editor_bg);
    let x = button(app, f, acts, (x, acts.y), glyph(I_PROGRAM), "Flash", Look::Primary, ready && port.is_some(), Hit::Run(Op::IdfFlash), P.editor_bg);
    key_hint(f, acts, (x, acts.y), "F5");
    button(app, f, acts, (x + 4, acts.y), glyph(I_ERASE), "Erase Flash", Look::Danger, ready && port.is_some(), Hit::Confirm(Op::IdfErase), P.editor_bg);
    if r.height < 3 {
        return;
    }
    let c = card(f, r, "Command", P.widget_border);
    if let Ok(Some(cmd)) = command(&app.prog, Op::IdfFlash) {
        let p = Paragraph::new(Line::from(vec![Span::styled("$ ", Style::new().fg(P.dim)), Span::styled(cmd.shown, Style::new().fg(P.editor_fg))])).wrap(Wrap { trim: false });
        f.render_widget(p, c);
    }
}

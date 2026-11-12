//! Delayed function documentation, using the same bounded lookup as F12.
use crate::editor::{self, Buffer, Pos};
use crate::nav::{self, Kind, Lang, Pending, Query};
use crate::panel::{App, Doc, Drag};
use crate::{navui, theme::{self, P}, ui};
use ratatui::crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

const DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, PartialEq)]
struct Target {
    group: usize,
    key: String,
    version: u64,
    pos: Pos,
    name: String,
    anchor: Position,
}

pub struct Card {
    signature: Buffer,
    lines: Vec<String>,
    source: Option<(String, u64)>,
    area: Rect,
    scroll: usize,
    max_scroll: usize,
}

#[derive(Default)]
pub struct State {
    target: Option<Target>,
    pub due: Option<Instant>,
    pub pending: Option<Pending>,
    in_repo: bool,
    pub card: Option<Card>,
}

pub fn dismiss(app: &mut App) -> bool {
    let visible = app.nav.hover.card.is_some();
    app.nav.hover = State::default(); // dropping Pending cancels and reaps its rg
    visible
}

fn blocked(app: &App) -> bool {
    app.settings_dlg.is_some() || app.prompt.is_some() || app.quick.is_some() || app.find.is_some()
        || app.gitui.dlg.is_some() || app.gitui.cmds.picker.is_some() || app.gitui.card.is_some()
        || app.peek.is_some() || app.nav.peek.is_some() || app.nav.req_running() || !matches!(app.drag, Drag::None)
}

fn target_at(app: &App, p: Position) -> Option<Target> {
    if blocked(app) { return None; }
    let (group, pos, range, name) = navui::symbol_under(app, p)?;
    let grp = &app.groups[group];
    let tab = grp.tabs.get(grp.active)?;
    let Some(Doc::Text(b)) = app.docs.get(&tab.key) else { return None };
    let line = &b.lines[pos.line];
    if !code_at(line, range.start, nav::lang_for(&tab.key)) { return None; }
    // ponytail: call-shaped identifiers; semantic overload resolution needs an LSP.
    if !line[range.end..].trim_start().starts_with('(')
        || matches!(name.as_str(), "if" | "for" | "while" | "switch" | "sizeof" | "typeof" | "match" | "catch") {
        return None;
    }
    let x = editor::display_col(line, range.start).saturating_sub(tab.view.hscroll);
    Some(Target { group, key: tab.key.clone(), version: b.version(), pos: Pos::new(pos.line, range.start), name,
        anchor: Position::new(app.ui.groups[group].text.x + x as u16, p.y) })
}

// Only scan this line's prefix; never reparse a file on every mouse motion.
fn code_at(line: &str, at: usize, lang: Lang) -> bool {
    let b = &line.as_bytes()[..at];
    let (mut i, mut quote, mut block) = (0, None, false);
    while i < b.len() {
        let (c, next) = (b[i], b.get(i + 1).copied());
        if let Some(q) = quote {
            if c == b'\\' { i += 2; continue; }
            if c == q { quote = None; }
        } else if block {
            if c == b'*' && next == Some(b'/') { block = false; i += 2; continue; }
        } else {
            if c == b'/' && next == Some(b'/')
                || c == b'#' && matches!(lang, Lang::Python | Lang::Shell) { return false; }
            if c == b'/' && next == Some(b'*') { block = true; i += 2; continue; }
            if c == b'\'' || c == b'"' { quote = Some(c); }
        }
        i += 1;
    }
    quote.is_none() && !block && !(line.trim_start().starts_with('*') && !line[..at].contains("*/"))
}

fn valid(app: &App) -> bool {
    let Some(t) = &app.nav.hover.target else { return false };
    target_at(app, t.anchor).as_ref() == Some(t)
        && app.nav.hover.card.as_ref().and_then(|c| c.source.as_ref()).is_none_or(|(key, version)| {
            matches!(app.docs.get(key), Some(Doc::Text(b)) if b.version() == *version)
        })
}

/// Some: the pointer is over the documentation and the underlying editor gets no event.
pub fn on_mouse(app: &mut App, m: MouseEvent) -> Option<bool> {
    let p = Position::new(m.column, m.row);
    if blocked(app) || m.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        dismiss(app);
        return None;
    }
    if let Some(c) = app.nav.hover.card.as_mut().filter(|c| c.area.contains(p)) {
        match m.kind {
            MouseEventKind::Moved => { app.hover = Some(p); return Some(false); }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let old = c.scroll;
                c.scroll = c.scroll.saturating_add_signed(if m.kind == MouseEventKind::ScrollUp { -3 } else { 3 }).min(c.max_scroll);
                return Some(c.scroll != old);
            }
            _ => return Some(dismiss(app)),
        }
    }
    if m.kind != MouseEventKind::Moved {
        dismiss(app);
        return None;
    }
    let target = target_at(app, p);
    if target != app.nav.hover.target {
        dismiss(app);
        app.nav.hover.due = target.as_ref().map(|_| Instant::now() + DELAY);
        app.nav.hover.target = target;
    }
    None
}

fn start(app: &mut App, in_repo: bool) -> Option<()> {
    let t = app.nav.hover.target.as_ref()?;
    let path = t.key.strip_prefix("file:")?;
    let Some(Doc::Text(b)) = app.docs.get(&t.key) else { return None };
    let root = if in_repo { app.git_root.clone()? } else { app.root.clone() };
    let from_file = if in_repo { app.repo_path(path)? } else { path.to_string() };
    let q = Query::Definitions { from_file, from_line: t.pos.line + 1, buffer: b.lines.join("\n") };
    let pending = nav::spawn(root, t.name.clone(), nav::lang_for(path), q, app.live_rg.clone());
    app.nav.hover.pending = Some(pending);
    app.nav.hover.in_repo = in_repo;
    Some(())
}

pub fn poll(app: &mut App) -> bool {
    if app.nav.hover.target.is_none() { return false; }
    if !valid(app) { return dismiss(app); }
    if app.nav.hover.due.is_some_and(|t| Instant::now() >= t) {
        app.nav.hover.due = None;
        start(app, false);
    }
    let res = match app.nav.hover.pending.as_ref().map(|p| p.rx.try_recv()) {
        Some(Ok(r)) => r,
        Some(Err(TryRecvError::Disconnected)) => Err("lookup ended".into()),
        _ => return false,
    };
    app.nav.hover.pending = None;
    let Ok(items) = res else { return false };
    let mut changed = false;
    for loc in items.iter().filter(|l| matches!(l.kind, Kind::Function | Kind::Decl)).take(8) {
        let path = if app.nav.hover.in_repo { app.doc_path(&loc.path) } else { loc.path.clone() };
        let key = format!("file:{path}");
        let disk;
        let (lines, source) = match app.docs.get(&key) {
            Some(Doc::Text(b)) => (&b.lines, Some((key.clone(), b.version()))),
            _ => { disk = nav::read_lines(&app.root.join(&path)); (&disk, None) }
        };
        // An open buffer can have unsaved comments inserted above the definition.
        let at = loc.line.saturating_sub(1);
        let at = if lines.get(at).is_some_and(|l| l.trim() == loc.preview.trim()) { at }
            else { let Some(at) = lines.iter().position(|l| l.trim() == loc.preview.trim()) else { continue }; at };
        let docs = documentation(lines, at, nav::lang_for(&path));
        let documented = !docs.is_empty();
        if documented || app.nav.hover.card.is_none() {
            let text = signature(lines, at, nav::lang_for(&path));
            let signature = Buffer::from_lines(&path, vec![text], "function hover");
            app.nav.hover.card = Some(Card { signature, lines: docs, source,
                area: Rect::default(), scroll: 0, max_scroll: 0 });
            changed = true;
            if documented { return true; }
        }
    }
    if !app.nav.hover.in_repo && app.git_root.as_ref().is_some_and(|g| *g != app.root) {
        start(app, true);
    }
    changed
}

/// Adjacent C/kernel-doc, Rust/JSDoc, Go comments and Python docstrings. No body scan.
fn documentation(lines: &[String], line: usize, lang: Lang) -> Vec<String> {
    if line >= lines.len() { return Vec::new(); }
    if lang == Lang::Python {
        let header_end = (line..lines.len().min(line + 16)).find(|&i| lines[i].trim_end().ends_with(':'));
        if let Some(start) = header_end.and_then(|end| (end + 1..lines.len().min(end + 5)).find(|&i| !lines[i].trim().is_empty())) {
            let first = lines[start].trim_start().trim_start_matches(['r', 'u']);
            if let Some(q) = ["\"\"\"", "'''"].into_iter().find(|q| first.starts_with(q)) {
                let mut out = Vec::new();
                let indent = &lines[start][..lines[start].len() - lines[start].trim_start().len()];
                for (k, l) in lines[start..].iter().take(80).enumerate() {
                    let l = if k == 0 { &first[q.len()..] } else { l.strip_prefix(indent).unwrap_or(l.trim_start()) };
                    if let Some((body, _)) = l.split_once(q) { out.push(body.to_string()); return trim_docs(out); }
                    out.push(l.to_string());
                }
            }
        }
    }
    let mut start = line;
    let mut blank = false;
    // A C return type or an attribute can sit between the comment and function name.
    for _ in 0..12 {
        let Some(prev) = start.checked_sub(1).and_then(|i| lines.get(i)) else { break };
        let l = prev.trim();
        if l.ends_with("*/") {
            let end = start;
            while start > 0 && end - start < 80 {
                start -= 1;
                if let Some((_, tail)) = lines[start].split_once("/*") {
                    let mut out = Vec::new();
                    for (k, l) in lines[start..end].iter().enumerate() {
                        let l = if k == 0 { tail.trim_start_matches('*') } else { l.trim_start().strip_prefix('*').unwrap_or(l.trim_start()) };
                        // Remove the closer before its leading star is stripped.
                        let l = if k > 0 && lines[start + k].trim() == "*/" { "" } else { l.split_once("*/").map_or(l, |x| x.0) };
                        out.push(l.strip_prefix(' ').unwrap_or(l).to_string());
                    }
                    return trim_docs(out);
                }
            }
            return Vec::new();
        }
        let line_comment = |l: &str| l.starts_with("//") && !l.starts_with("//!")
            || matches!(lang, Lang::Python | Lang::Shell) && l.starts_with('#');
        if line_comment(l) {
            let end = start;
            while start > 0 && end - start < 80 && line_comment(lines[start - 1].trim_start()) { start -= 1; }
            return trim_docs(lines[start..end].iter().map(|l| {
                l.trim_start().trim_start_matches(['/', '#']).strip_prefix(' ').unwrap_or(l.trim_start().trim_start_matches(['/', '#'])).to_string()
            }).collect());
        }
        if l.is_empty() {
            if blank { return Vec::new(); }
            blank = true;
            start -= 1;
            continue;
        }
        if l.starts_with("#[") || l.starts_with('@')
            || lang == Lang::C && l.chars().all(|c| c.is_alphanumeric() || c.is_whitespace() || "_*&:".contains(c)) {
            start -= 1;
        } else { break; }
    }
    Vec::new()
}

fn trim_docs(mut lines: Vec<String>) -> Vec<String> {
    for line in &mut lines { line.truncate(line.trim_end().len()); }
    while lines.last().is_some_and(|l| l.trim().is_empty()) { lines.pop(); }
    let start = lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(lines.len());
    lines.drain(..start);
    lines
}

/// Join a source signature, including a separate C return type, without its body.
fn signature(lines: &[String], at: usize, lang: Lang) -> String {
    let mut start = at;
    if lang == Lang::C {
        while start > 0 && at - start < 6 {
            let prev = lines[start - 1].trim();
            if prev.is_empty() || !prev.chars().all(|c| c.is_alphanumeric() || c.is_whitespace() || "_*&:".contains(c)) { break; }
            start -= 1;
        }
    }
    let (mut out, mut depth, mut opened, mut quote, mut escaped) = (String::new(), 0usize, false, None, false);
    for line in lines.iter().skip(start).take(12) {
        for c in line.trim().chars() {
            if let Some(q) = quote {
                if escaped { escaped = false; }
                else if c == '\\' { escaped = true; }
                else if c == q { quote = None; }
            } else {
                if c == '"' || c == '\'' && lang != Lang::Rust { quote = Some(c); }
                if c == '(' { opened = true; depth += 1; }
                if c == ')' { depth = depth.saturating_sub(1); }
                if opened && depth == 0 && (matches!(c, '{' | ';') || c == ':' && lang == Lang::Python) {
                    return clean(out.trim());
                }
            }
            out.push(c);
            if out.len() >= 4096 { return clean(out.trim()); }
        }
        if opened && depth == 0 { break; }
        out.push(' ');
    }
    clean(out.trim())
}

fn clean(text: &str) -> String {
    let (mut out, mut col) = (String::new(), 0);
    for c in text.chars() {
        if c == '\t' { out.push_str(&" ".repeat(editor::tab() - col % editor::tab())); }
        else if !c.is_control() { out.push(c); }
        if !c.is_control() || c == '\t' { col = editor::advance(col, c); }
    }
    out
}

struct Colors { bg: Color, fg: Color, border: Color, code: Style, link: Style }
impl Colors {
    fn new(app: &App) -> Colors {
        let empty = serde_json::Map::new();
        let map = app.settings.theme.json["colors"].as_object().unwrap_or(&empty);
        let bg = theme::lookup(map, "editorHoverWidget.background").map_or(P.widget_bg, |c| c.over(P.editor_bg));
        let get = |key, fallback| theme::lookup(map, key).map_or(fallback, |c| c.over(bg));
        let fg = get("editorHoverWidget.foreground", P.widget_fg);
        Colors { bg, fg, border: get("editorHoverWidget.border", P.widget_border),
            code: Style::new().fg(get("textPreformat.foreground", fg)).bg(get("textPreformat.background", P.editor_bg)),
            link: Style::new().fg(get("textLink.foreground", P.focus_border)).add_modifier(Modifier::UNDERLINED) }
    }
}

/// The Markdown subset useful in function docs; source text never becomes HTML or commands.
fn inline(text: &str, c: &Colors) -> Vec<Span<'static>> {
    let mut rest = text;
    let mut out = Vec::new();
    while !rest.is_empty() {
        let mut matched = false;
        for (mark, style) in [("`", c.code), ("**", Style::new().bold()), ("*", Style::new().italic())] {
            if let Some(tail) = rest.strip_prefix(mark) {
                if let Some((body, after)) = tail.split_once(mark).filter(|(body, _)| !body.is_empty()) {
                    out.push(Span::styled(body.to_string(), style)); rest = after; matched = true; break;
                }
            }
        }
        if matched { continue; }
        if let Some(tail) = rest.strip_prefix('[') {
            if let Some((label, tail)) = tail.split_once("](") {
                if let Some((_, after)) = tail.split_once(')') {
                    out.push(Span::styled(label.to_string(), c.link)); rest = after; continue;
                }
            }
        }
        let n = rest.char_indices().skip(1).find(|(_, ch)| matches!(ch, '`' | '*' | '[')).map_or(rest.len(), |(i, _)| i);
        out.push(Span::raw(rest[..n].to_string())); rest = &rest[n..];
    }
    out
}

fn doc_lines(lines: &[String], c: &Colors) -> Vec<Line<'static>> {
    let mut code = false;
    lines.iter().filter_map(|line| {
        let text = clean(line);
        if text.trim_start().starts_with("```") { code = !code; return None; }
        if code || text.starts_with("    ") { return Some(Line::styled(text, c.code)); }
        for prefix in ["@returns ", "@return ", "@returns:", "@return:", "Returns:", "Return:"] {
            if let Some(rest) = text.strip_prefix(prefix) {
                let mut spans = vec![Span::styled("Returns — ", Style::new().italic())];
                spans.extend(inline(rest.trim(), c)); return Some(Line::from(spans));
            }
        }
        if let Some(rest) = text.strip_prefix("* ") {
            let mut spans = vec![Span::raw("- ")];
            spans.extend(inline(rest, c)); return Some(Line::from(spans));
        }
        let tag = if let Some(rest) = text.strip_prefix("@param ") {
            let (name, desc) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            Some(("@param", name.trim_end_matches(':'), desc.trim_start_matches([' ', '-', ':'])))
        } else if let Some(rest) = text.strip_prefix('@') {
            rest.split_once(':').filter(|(name, _)| nav::is_ident(name)).map(|(name, desc)| ("@param", name, desc.trim()))
        } else { None };
        if let Some((tag, name, desc)) = tag {
            let mut spans = vec![Span::styled(format!("{tag} "), Style::new().italic()), Span::styled(name.to_string(), c.code.bold()), Span::raw(" — ")];
            spans.extend(inline(desc, c)); return Some(Line::from(spans));
        }
        if let Some(heading) = text.strip_prefix('#').map(|s| s.trim_start_matches('#')).and_then(|s| s.strip_prefix(' ')) {
            return Some(Line::from(inline(heading, c)).style(Style::new().bold()));
        }
        Some(Line::from(inline(&text, c)))
    }).collect()
}

pub fn draw(app: &mut App, f: &mut Frame) {
    if !valid(app) { dismiss(app); return; }
    let c = Colors::new(app);
    let Some(t) = app.nav.hover.target.as_ref() else { return };
    let area = app.ui.groups[t.group].area.intersection(f.area());
    let Some(card) = app.nav.hover.card.as_mut() else { return };
    card.area = Rect::default();
    if area.width < 16 || area.height < 3 { return; }
    let docs = doc_lines(&card.lines, &c);
    let sig = &card.signature.lines[0];
    let w = (docs.iter().map(Line::width).chain([sig.width()]).max().unwrap_or(20) + 4).clamp(16, 84).min(area.width as usize) as u16;
    card.signature.highlight_upto(1);
    let sig = &card.signature.lines[0];
    let code = ui::render_line(sig, card.signature.segs(0), &[], None, 0, sig.width().max(1), Style::new().fg(c.fg).bg(c.bg));
    let mut lines = vec![code];
    if !docs.is_empty() {
        lines.push(Line::styled(theme::icons().border[4].repeat((w - 4) as usize), Style::new().fg(c.border)));
        lines.extend(docs);
    }
    let paragraph = Paragraph::new(lines).style(Style::new().fg(c.fg).bg(c.bg)).wrap(Wrap { trim: false });
    let total = paragraph.line_count(w - 4);
    let want = (total + 2).clamp(3, 16) as u16;
    let above = t.anchor.y.saturating_sub(area.y);
    let below = area.bottom().saturating_sub(t.anchor.y + 1);
    let top = above >= want || below < want && above >= below;
    let h = want.min(if top { above } else { below });
    if h < 3 { return; }
    let x = t.anchor.x.min(area.right() - w).max(area.x);
    let y = if top { t.anchor.y - h } else { t.anchor.y + 1 };
    let pop = Rect::new(x, y, w, h);
    card.area = pop;
    let block = ui::popup_block(c.bg, c.fg, (1, 0)).border_style(Style::new().fg(c.border).bg(c.bg));
    let body = block.inner(pop);
    f.render_widget(Clear, pop);
    f.render_widget(block, pop);
    card.max_scroll = total.saturating_sub(body.height as usize).min(u16::MAX as usize);
    card.scroll = card.scroll.min(card.max_scroll);
    f.render_widget(paragraph.scroll((card.scroll as u16, 0)), body);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str, docs: &str) -> (App, Terminal<ratatui::backend::TestBackend>, std::path::PathBuf, Position) {
        let root = std::env::temp_dir().join(format!("termcode-hover-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("f.c"), format!("{docs}static int foo(void) {{ return 0; }}\nint use(void) {{ return foo(); }}\n")).unwrap();
        let mut app = App::new(root.clone(), None);
        app.open_file(0, "f.c", false);
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(110, 28)).unwrap();
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        crate::panel::wait_idle(&mut app);
        let (b, tab) = app.ed().unwrap();
        let call = b.lines.len() - 1;
        let col = b.lines[call].find("foo").unwrap();
        tab.view.scroll = call;
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let gh = &app.ui.groups[0];
        let p = Position::new(gh.text.x + col as u16, gh.text.y);
        (app, term, root, p)
    }

    fn motion(app: &mut App, p: Position, modifiers: KeyModifiers) {
        navui::on_mouse(app, MouseEvent { kind: MouseEventKind::Moved, column: p.x, row: p.y, modifiers });
    }

    fn finish_hover(app: &mut App) {
        app.nav.hover.due = Some(Instant::now());
        poll(app);
        let end = Instant::now() + Duration::from_secs(3);
        while app.nav.hover.pending.is_some() {
            assert!(Instant::now() < end, "hover lookup never completed");
            poll(app);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn hover_cancels_stale_work_and_keeps_ctrl_navigation() {
        let (mut app, mut term, root, p) = fixture("cancel", "// Function docs.\n");
        motion(&mut app, p, KeyModifiers::NONE);
        let due = app.nav.hover.due;
        motion(&mut app, Position::new(p.x + 1, p.y), KeyModifiers::NONE);
        assert_eq!(app.nav.hover.due, due, "motion within a name keeps the dwell timer");
        motion(&mut app, p, KeyModifiers::CONTROL);
        assert!(app.nav.hover.target.is_none() && app.nav.link.is_some());
        motion(&mut app, p, KeyModifiers::NONE);
        app.nav.hover.due = Some(Instant::now());
        poll(&mut app);
        motion(&mut app, Position::new(0, 0), KeyModifiers::NONE);
        std::thread::sleep(Duration::from_millis(20));
        poll(&mut app);
        assert!(app.nav.hover.pending.is_none() && app.nav.hover.card.is_none());
        motion(&mut app, p, KeyModifiers::NONE);
        finish_hover(&mut app);
        assert!(app.nav.hover.card.is_some());
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let (b, tab) = app.ed().unwrap();
        b.insert(&mut tab.view, "changed ");
        poll(&mut app);
        assert!(app.nav.hover.target.is_none(), "an edit invalidates the result");
        crate::panel::wait_idle(&mut app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn long_comments_scroll_inside_a_bounded_hover() {
        let docs = (0..45).map(|i| format!("// Documentation line {i:02}.\n")).collect::<String>();
        let (mut app, mut term, root, p) = fixture("scroll", &docs);
        motion(&mut app, p, KeyModifiers::NONE);
        finish_hover(&mut app);
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let c = app.nav.hover.card.as_ref().unwrap();
        assert!(c.max_scroll > 0 && term.backend().buffer().area.contains(c.area.as_position()));
        let inside = Position::new(c.area.x + 1, c.area.y + 2);
        motion(&mut app, inside, KeyModifiers::NONE);
        poll(&mut app);
        assert!(app.nav.hover.card.is_some(), "moving into the popup keeps it open");
        let before = app.ed().unwrap().1.view.scroll;
        navui::on_mouse(&mut app, MouseEvent { kind: MouseEventKind::ScrollDown, column: inside.x, row: inside.y, modifiers: KeyModifiers::NONE });
        assert_eq!(app.nav.hover.card.as_ref().unwrap().scroll, 3);
        assert_eq!(app.ed().unwrap().1.view.scroll, before, "the wheel does not scroll code underneath");
        for (w, h) in [(30, 5), (40, 10), (80, 20)] {
            let mut t = Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            t.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        }
        dismiss(&mut app);
        crate::panel::wait_idle(&mut app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn undocumented_functions_show_only_the_signature_without_idle_polling() {
        let (mut app, mut term, root, p) = fixture("no-docs", "");
        motion(&mut app, p, KeyModifiers::NONE);
        finish_hover(&mut app);
        assert!(app.nav.hover.card.is_some(), "a resolved function still has a signature without documentation");
        assert!(app.nav.hover.card.as_ref().unwrap().lines.is_empty());
        assert!(app.nav.hover.due.is_none() && !app.nav.busy());
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let area = app.nav.hover.card.as_ref().unwrap().area;
        assert_eq!(area.height, 3, "signature plus border, with no empty documentation section");
        let text = (area.x..area.right()).map(|x| term.backend().buffer()[(x, area.y + 1)].symbol()).collect::<String>();
        assert!(text.contains("static int foo(void)"), "{text}");
        motion(&mut app, p, KeyModifiers::NONE);
        assert!(app.nav.hover.due.is_none(), "a completed result is kept until the pointer leaves");
        dismiss(&mut app);
        crate::panel::wait_idle(&mut app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn function_names_inside_comments_do_not_start_a_hover_lookup() {
        let (mut app, mut term, root, _) = fixture("comment-name", "// foo() is mentioned here.\n");
        app.ed().unwrap().1.view.scroll = 0;
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let gh = &app.ui.groups[0];
        let p = Position::new(gh.text.x + 3, gh.text.y);
        motion(&mut app, p, KeyModifiers::NONE);
        assert!(app.nav.hover.target.is_none());
        crate::panel::wait_idle(&mut app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inline_comments_end_before_a_real_call_and_strings_are_not_code() {
        for (line, lang, want) in [
            ("/* 中文 */ foo()", Lang::C, true),
            ("/* http://example */ foo()", Lang::C, true),
            ("// foo()", Lang::C, false),
            ("/* foo()", Lang::C, false),
            (" * foo()", Lang::C, false),
            ("printf(\"foo()\")", Lang::C, false),
            ("char *s = \"x\"; foo()", Lang::C, true),
            ("# foo()", Lang::Python, false),
        ] {
            assert_eq!(code_at(line, line.find("foo").unwrap(), lang), want, "{line}");
        }
    }

    #[test]
    fn vscode_hover_shows_a_coloured_signature_then_formatted_docs() {
        let (mut app, mut term, root, p) = fixture("vscode-display", "// **Description** using `foo`.\n// @param value: the input\n// Return: the result\n");
        let colors = app.settings.theme.json["colors"].as_object_mut().unwrap();
        colors.insert("editorHoverWidget.background".into(), "#203040".into());
        colors.insert("editorHoverWidget.foreground".into(), "#d0d1d2".into());
        colors.insert("editorHoverWidget.border".into(), "#506070".into());
        motion(&mut app, p, KeyModifiers::NONE);
        finish_hover(&mut app);
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let area = app.nav.hover.card.as_ref().unwrap().area;
        let buffer = term.backend().buffer();
        let rows = (area.y..area.bottom()).map(|y| (area.x..area.right()).map(|x| buffer[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>();
        let signature = rows.iter().position(|r| r.contains("static int foo(void)")).expect("a full signature replaces the path title");
        let docs = rows.iter().position(|r| r.contains("Description")).unwrap();
        assert!(signature < docs && !rows.join("\n").contains("f.c:"));
        assert!(!rows.join("\n").contains("**") && !rows.join("\n").contains('`'));
        assert!(rows.iter().any(|r| r.contains("@param value")));
        assert_eq!(buffer[(area.x, area.y)].fg, Color::Rgb(0x50, 0x60, 0x70));
        let row_y = area.y + signature as u16;
        let int_x = area.x + rows[signature].find("int").unwrap() as u16;
        assert_ne!(buffer[(int_x, row_y)].fg, Color::Rgb(0xd0, 0xd1, 0xd2), "signature has token colours");
        let description_x = area.x + rows[docs].find("Description").unwrap() as u16;
        let cell = &buffer[(description_x, area.y + docs as u16)];
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert_eq!(cell.bg, Color::Rgb(0x20, 0x30, 0x40));
        assert!(!area.contains(p), "hover never covers the function being inspected");
        dismiss(&mut app);
        crate::panel::wait_idle(&mut app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn signatures_include_return_type_and_all_arguments_without_the_body() {
        for (lang, text, at, expected) in [
            (Lang::C, "/** docs */\nstatic unsigned long\nfoo(struct port *p,\n    int flags)\n{\n return 0;\n}", 2, "static unsigned long foo(struct port *p, int flags)"),
            (Lang::C, "int foo(int x) { return x; }", 0, "int foo(int x)"),
            (Lang::C, "int foo(const char *s = \"a) b\");", 0, "int foo(const char *s = \"a) b\")"),
            (Lang::Rust, "pub fn foo<'a>(text: &'a str) -> usize { text.len() }", 0, "pub fn foo<'a>(text: &'a str) -> usize"),
            (Lang::Python, "def foo(x: int,\n    name: str) -> str:\n    pass", 0, "def foo(x: int, name: str) -> str"),
        ] {
            let lines = text.lines().map(String::from).collect::<Vec<_>>();
            assert_eq!(signature(&lines, at, lang), expected);
        }
    }

    #[test]
    fn markdown_formats_return_tags_and_fenced_examples() {
        let (app, _term, root, _) = fixture("markdown", "// Docs.\n");
        let c = Colors::new(&app);
        let source = ["@return: a value", "* **Bold** with `code`", "```c", "  int x = 1;", "```", "[Guide](https://example.test)"];
        let lines = doc_lines(&source.map(String::from), &c);
        let plain = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect::<Vec<_>>();
        assert_eq!(plain, ["Returns — a value", "- Bold with code", "  int x = 1;", "Guide"]);
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(60, 8)).unwrap();
        term.draw(|f| f.render_widget(Paragraph::new(lines).style(Style::new().fg(c.fg).bg(c.bg)), f.area())).unwrap();
        assert!(term.backend().buffer()[(2, 1)].modifier.contains(Modifier::BOLD));
        assert_eq!(term.backend().buffer()[(2, 2)].bg, c.code.bg.unwrap_or(c.bg));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hover_prefers_above_and_falls_below_without_covering_the_name() {
        let (mut app, mut term, root, _) = fixture("position", "// Brief docs.\n");
        let (b, tab) = app.ed().unwrap();
        let call = b.lines.len() - 1;
        tab.view.cursor = Pos::new(call, 0);
        b.insert(&mut tab.view, &"\n".repeat(9));
        let call = b.lines.len() - 1;
        let col = b.lines[call].find("foo").unwrap();
        tab.view.scroll = 0;
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let p = Position::new(app.ui.groups[0].text.x + col as u16, app.line_y(0, call).unwrap());
        motion(&mut app, p, KeyModifiers::NONE);
        finish_hover(&mut app);
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        assert!(app.nav.hover.card.as_ref().unwrap().area.bottom() <= p.y);
        dismiss(&mut app);
        app.ed().unwrap().1.view.scroll = call;
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        let p = Position::new(app.ui.groups[0].text.x + col as u16, app.line_y(0, call).unwrap());
        motion(&mut app, p, KeyModifiers::NONE);
        finish_hover(&mut app);
        term.draw(|f| crate::ui::draw(&mut app, f)).unwrap();
        assert!(app.nav.hover.card.as_ref().unwrap().area.y > p.y);
        dismiss(&mut app);
        crate::panel::wait_idle(&mut app);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn documentation_stays_attached_to_its_function() {
        let cases = [
            (Lang::C, "/**\n * Change master.\n * @dp: 中文 port\n */\nstatic int\nchange(void)", 5, vec!["Change master.", "@dp: 中文 port"]),
            (Lang::C, "/* A one-line note. */\nint foo(void)", 1, vec!["A one-line note."]),
            (Lang::Rust, "/// Rust docs.\n/// More detail.\n#[inline]\npub fn foo()", 3, vec!["Rust docs.", "More detail."]),
            (Lang::Go, "// Foo does work.\nfunc Foo()", 1, vec!["Foo does work."]),
            (Lang::Js, "/** JSDoc. */\nfunction foo()", 1, vec!["JSDoc."]),
            (Lang::Python, "def foo():\n    \"\"\"A docstring.\n    More detail.\n    \"\"\"\n    pass", 0, vec!["A docstring.", "More detail."]),
            (Lang::Python, "def foo():\n\n    \"\"\"After a blank.\"\"\"\n    pass", 0, vec!["After a blank."]),
            (Lang::C, "/* Unrelated variable. */\nint value;\nint foo(void)", 2, vec![]),
            (Lang::C, "/* File licence. */\n\n\nint foo(void)", 3, vec![]),
            (Lang::Rust, "//! Module docs.\nfn foo()", 1, vec![]),
            (Lang::Python, "def foo():\n    work()\n    \"\"\"Not its docstring.\"\"\"", 0, vec![]),
        ];
        for (lang, input, at, expected) in cases {
            let lines = input.lines().map(String::from).collect::<Vec<_>>();
            assert_eq!(documentation(&lines, at, lang), expected, "{input}");
        }
    }

    #[test]
    fn documentation_wraps_unicode_and_drops_terminal_controls() {
        let lines = ["中文 text\x1b[31m", "\tindented"];
        let cleaned = lines.iter().map(|line| clean(line)).collect::<Vec<_>>();
        assert!(cleaned.iter().all(|line| !line.contains(['\x1b', '\t'])));
        assert_eq!(cleaned[0], "中文 text[31m");
        let paragraph = Paragraph::new(cleaned.join("\n")).wrap(Wrap { trim: false });
        assert!(paragraph.line_count(6) >= 3);
    }
}

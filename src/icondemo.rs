//! `herdr-code --icon-demo` (hidden): the activity bar and an Explorer sample with the
//! three `herdr-code.icons.size` options side by side, drawn by the panel's own code, so
//! the user can pick in their own terminal and font. Any key quits; the terminal is
//! restored on every exit path (as the panel does: a panic hook, then after the loop).

use crate::blocks::{self, BlockStyle, IconSize};
use crate::icons::{file_icon, folder_icon, FileIcons};
use crate::theme::{icons, P};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::prelude::*;

const OPTIONS: [(IconSize, &str, &str); 3] = [
    (IconSize::Small, "A  small", "one cell (before)"),
    (IconSize::Wide, "B  wide", "2-cell slot (default)"),
    (IconSize::Large, "C  large", "3x2 codicons"),
];

pub fn run() -> Result<(), String> {
    crate::panel::setup().map_err(|e| e.to_string())?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::panel::restore();
        hook(info);
    }));
    let res = std::panic::catch_unwind(|| -> std::io::Result<()> {
        let mut t = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
        loop {
            t.draw(draw)?;
            // Any key quits; a resize (or anything else) redraws.
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    return Ok(());
                }
            }
        }
    });
    crate::panel::restore();
    match res {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(r) => r.map_err(|e| e.to_string()),
    }
}

/// The demo screen: a heading, the three options side by side, how to pick one.
pub fn draw(f: &mut Frame) {
    let area = f.area();
    f.buffer_mut().set_style(area, Style::new().bg(blocks::gutter()).fg(P.editor_fg));
    if area.width < 60 || area.height < 14 {
        f.render_widget(Line::from("herdr-code --icon-demo needs at least 60x14 cells. Any key quits."), area);
        return;
    }
    let heading = "Icon sizes in your terminal and font: pick one for \"herdr-code.icons.size\"";
    f.render_widget(Line::styled(format!(" {heading}"), Style::new().fg(P.sidebar_title).bold()), Rect::new(area.x, area.y, area.width, 1));
    let w = (area.width - 2) / 3;
    for (k, (size, name, note)) in OPTIONS.iter().enumerate() {
        let x = area.x + 1 + k as u16 * w;
        let col = Rect::new(x, area.y + 2, w.saturating_sub(1), area.height - 4);
        f.render_widget(
            Line::from(vec![Span::styled(*name, Style::new().fg(P.activity_fg).bold()), Span::styled(format!("  {note}"), Style::new().fg(P.dim))]),
            Rect::new(col.x, col.y, col.width, 1),
        );
        option(f, Rect::new(col.x, col.y + 1, col.width, col.height - 1), *size);
    }
    let foot = "settings.json: \"herdr-code.icons.size\": \"small\" | \"wide\" | \"large\"   ·   any key quits";
    f.render_widget(Line::styled(format!(" {foot}"), Style::new().fg(P.dim)), Rect::new(area.x, area.bottom() - 1, area.width, 1));
}

/// One option: an activity bar card beside an Explorer card, as the panel draws them.
fn option(f: &mut Frame, r: Rect, size: IconSize) {
    let ic = icons();
    let bar = blocks::card(BlockStyle::Cards, f, Rect::new(r.x, r.y, 7, r.height), P.activity_bg, false);
    for (i, (icon, name)) in [(ic.files, "files"), (ic.search, "search"), (ic.scm, "source-control")].into_iter().enumerate() {
        crate::ui::activity_item(f, bar, i, size, icon, name, i == 0);
    }
    let tree = blocks::card(BlockStyle::Cards, f, Rect::new(r.x + 8, r.y, r.width.saturating_sub(8), r.height), P.sidebar_bg, true);
    let fi = FileIcons::default();
    let gap = " ".repeat(blocks::icon_gap(size, 2) as usize);
    let rows: [(u16, &str, Option<bool>); 7] = [
        (0, "drivers", Some(true)),
        (1, "net", Some(false)),
        (1, "mxl862xx.c", None),
        (1, "Makefile", None),
        (0, "include", Some(false)),
        (0, "README.md", None),
        (0, "Kconfig", None),
    ];
    for (k, (depth, name, dir)) in rows.into_iter().enumerate() {
        let y = tree.y + 1 + k as u16;
        if y >= tree.bottom() {
            break;
        }
        let (chev, (glyph, color)) = match dir {
            Some(open) => (if open { ic.chev_down } else { ic.chev_right }, folder_icon(name, open, &fi)),
            None => (" ", file_icon(name, &fi)),
        };
        let line = Line::from(vec![
            Span::raw(format!("  {}", "  ".repeat(depth as usize))),
            Span::styled(chev, Style::new().fg(P.icon_fg)),
            Span::raw(" "),
            Span::styled(glyph, Style::new().fg(color)),
            Span::raw(gap.clone()),
            Span::styled(name, Style::new().fg(P.sidebar_fg)),
        ]);
        f.render_widget(line, Rect::new(tree.x, y, tree.width, 1));
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn the_demo_shows_the_three_sizes() {
        let mut t = Terminal::new(TestBackend::new(120, 24)).unwrap();
        t.draw(super::draw).unwrap();
        let b = t.backend().buffer();
        let screen: String = (0..24).map(|y| (0..120).map(|x| b[(x, y)].symbol()).collect::<String>() + "\n").collect();
        for label in ["A  small", "B  wide", "C  large", "herdr-code.icons.size"] {
            assert!(screen.contains(label), "{label}");
        }
        let large = crate::blocks::large_icon("files").unwrap();
        assert!(screen.contains(large[0].as_str()) && screen.contains(large[1].as_str()), "the 3x2 files icon in option C");
        // Too small: a note instead of a cut-off drawing.
        let mut small = Terminal::new(TestBackend::new(40, 8)).unwrap();
        small.draw(super::draw).unwrap();
    }
}

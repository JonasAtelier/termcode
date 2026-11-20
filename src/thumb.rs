//! Picture previews on hover: an image file pointed at in the Explorer, or an image's path
//! pointed at in the editor, shows in a small card: its real pixels where the terminal has
//! Kitty graphics (Ghostty, kitty), half-blocks elsewhere.

use crate::panel::{App, Doc};
use crate::theme::P;
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthStr;

/// The largest picture a card shows, in cells; a small one is shown up to 4 times bigger.
const MAX: (u16, u16) = (32, 12);

/// The path-like word around byte `at` of `line` (a dot ending a sentence left out), when
/// it names a picture.
pub fn image_word(line: &str, at: usize) -> Option<Range<usize>> {
    let ok = |c: char| c.is_alphanumeric() || "._-/~@+".contains(c);
    if at > line.len() || !line.is_char_boundary(at) {
        return None;
    }
    let start = line[..at].char_indices().rev().take_while(|(_, c)| ok(*c)).last().map_or(at, |(i, _)| i);
    let end = at + line[at..].char_indices().take_while(|(_, c)| ok(*c)).last().map_or(0, |(i, c)| i + c.len_utf8());
    let end = start + line[start..end].trim_end_matches('.').len();
    (end > start && crate::media::kind(Path::new(&line[start..end])) == Some(crate::media::Kind::Image)).then_some(start..end)
}

/// Where `word` is: from the home folder (`~/`), absolute, beside the file it is written
/// in, or in the workspace. Only a file that is there.
fn resolve(root: &Path, word: &str, beside: Option<&Path>) -> Option<PathBuf> {
    let w = Path::new(word);
    let home = word.strip_prefix("~/").and_then(|r| Some(PathBuf::from(std::env::var_os("HOME")?).join(r)));
    let near = beside.and_then(Path::parent).map(|d| d.join(w));
    home.into_iter().chain(w.is_absolute().then(|| w.to_path_buf())).chain(near).chain([root.join(w)]).find(|p| p.is_file())
}

/// The picture's size in cells: its pixels fitted into `MAX`, its shape kept.
fn fit((pw, ph): (u32, u32), (cw, ch): (u16, u16)) -> (u16, u16) {
    let (cw, ch) = (f64::from(cw.max(1)), f64::from(ch.max(1)));
    let scale = (f64::from(MAX.0) * cw / f64::from(pw.max(1))).min(f64::from(MAX.1) * ch / f64::from(ph.max(1))).min(4.0);
    (((f64::from(pw) * scale / cw).round() as u16).clamp(1, MAX.0), ((f64::from(ph) * scale / ch).round() as u16).clamp(1, MAX.1))
}

/// The card for the picture at `path`: right of `anchor` (the Explorer's row) when
/// `beside`, else under it (an editor's word) or over it when there is no room.
pub fn card(app: &mut App, f: &mut Frame, path: &Path, anchor: Rect, beside: bool) {
    let Ok(px) = image::image_dimensions(path) else { return };
    let cell = crate::media::cell(app);
    let (cols, rows) = fit(px, cell);
    let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let size = format!(" {}×{} ", px.0, px.1);
    let area = f.area();
    let w = (cols.max(size.width() as u16 + 2) + 2).min(area.width);
    let h = (rows + 2).min(area.height);
    let (x, y) = if beside {
        (anchor.right().min(area.right().saturating_sub(w)), anchor.y.min(area.bottom().saturating_sub(h)))
    } else {
        let y = if anchor.bottom() + h <= area.bottom() { anchor.bottom() } else { anchor.y.saturating_sub(h) };
        (anchor.x.min(area.right().saturating_sub(w)), y)
    };
    let r = Rect::new(x, y, w, h);
    f.render_widget(Clear, r);
    let block = crate::ui::popup_block(P.widget_bg, P.widget_fg, (0, 0))
        .title(Span::styled(format!(" {} ", crate::ui::fit(&name, w.saturating_sub(4) as usize)), Style::new().fg(P.widget_fg).bold()))
        .title_bottom(Line::styled(size, Style::new().fg(P.dim)).right_aligned());
    let inner = block.inner(r);
    f.render_widget(block, r);
    let pic = Rect::new(inner.x + inner.width.saturating_sub(cols) / 2, inner.y, cols.min(inner.width), rows.min(inner.height));
    let mut hk = std::collections::hash_map::DefaultHasher::new();
    (path, std::fs::metadata(path).and_then(|m| m.modified()).ok(), cols, rows, cell, P.editor_bg).hash(&mut hk);
    if let Some((id, seq)) = crate::media::kept(app, hk.finish(), (cols, rows), |wh| crate::mdpreview::pixels(path, wh)) {
        let buf = f.buffer_mut();
        for y in 0..pic.height {
            for x in 0..pic.width {
                let (sym, fg) = crate::settingsgraphics::placeholder(id, y, x);
                buf[(pic.x + x, pic.y + y)].set_symbol(&sym).set_fg(fg);
            }
        }
        if !seq.is_empty() {
            let c = &mut buf[(pic.x, pic.y)];
            let sym = format!("{seq}{}", c.symbol());
            c.set_symbol(&sym);
        }
        crate::settingsgraphics::mark_drawn(pic);
    } else if let Some(lines) = crate::mdpreview::half_blocks(path, cols, rows) {
        f.render_widget(Paragraph::new(lines), pic);
    }
}

/// The card for a picture's path under the pointer in an editor.
pub fn draw_editor(app: &mut App, f: &mut Frame) {
    let Some(p) = app.hover else { return };
    if crate::hover::blocked(app) || app.nav.hover.card.is_some() {
        return;
    }
    let Some((g, pos)) = crate::navui::pos_under(app, p) else { return };
    let Some(Doc::Text(b)) = app.groups.get(g).and_then(|grp| grp.tabs.get(grp.active)).and_then(|t| app.docs.get(&t.key)) else { return };
    let Some(line) = b.lines.get(pos.line) else { return };
    let Some(word) = image_word(line, pos.col) else { return };
    let Some(path) = resolve(&app.root, &line[word], b.path.as_deref()) else { return };
    card(app, f, &path, Rect::new(p.x, p.y, 1, 1), false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pictures_path_is_found_around_the_pointer_and_fitted_to_its_shape() {
        let line = r#"<img src="docs/shot-1.png"> and logo.svg, see ~/pics/a.JPG."#;
        let at = |s: &str| line.find(s).unwrap();
        assert_eq!(image_word(line, at("shot")).map(|r| &line[r]), Some("docs/shot-1.png"));
        assert_eq!(image_word(line, at("a.JPG") + 2).map(|r| &line[r]), Some("~/pics/a.JPG"), "the sentence's dot left out");
        assert_eq!(image_word(line, at("logo")), None, "not a picture termcode shows");
        assert_eq!(image_word(line, at("and")), None);
        // 8×16 cells: a wide screenshot fills the width, a tall one the height, an icon grows 4×.
        assert_eq!(fit((1920, 1080), (8, 16)), (32, 9));
        assert_eq!(fit((400, 1600), (8, 16)), (6, 12));
        assert_eq!(fit((16, 16), (8, 16)), (8, 4));
    }

    #[test]
    fn a_path_is_looked_for_beside_its_file_then_in_the_workspace() {
        let root = std::env::temp_dir().join(format!("termcode-thumb-{}", std::process::id()));
        std::fs::create_dir_all(root.join("doc/img")).unwrap();
        std::fs::write(root.join("doc/img/a.png"), b"x").unwrap();
        std::fs::write(root.join("b.png"), b"x").unwrap();
        let md = root.join("doc/README.md");
        assert_eq!(resolve(&root, "img/a.png", Some(&md)), Some(root.join("doc/img/a.png")));
        assert_eq!(resolve(&root, "b.png", Some(&md)), Some(root.join("b.png")));
        assert_eq!(resolve(&root, "none.png", Some(&md)), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

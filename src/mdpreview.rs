//! Markdown: Open Preview. The source (an open buffer with its unsaved edits, else the
//! file) rendered as terminal lines: headings, paragraphs and list items reflowed to the
//! pane with hanging indents, quotes, fenced code highlighted like the editor, aligned
//! tables, rules. Text only: HTML is never interpreted, links are never followed.
//! ui.rs keeps the lines until the source, the width or the colours change.

use crate::editor::Buffer;
use crate::theme::{self, P};
use ratatui::prelude::*;
use syntect::highlighting::Highlighter;
use syntect::parsing::Scope;
use unicode_width::UnicodeWidthStr;

struct Look {
    text: Style,
    heading: Style,
    bullet: Style,
    quote: Style,
    code: Style,
    block: Style,
    link: Style,
    rule: Style,
}

/// The editor's Markdown token colours, so the preview matches the theme.
fn look(link: Color) -> Look {
    let syn = theme::syntax();
    let h = Highlighter::new(&syn.theme);
    let fg = |scope: &str| {
        let c = h.style_for_stack(&[Scope::new(scope).unwrap()]).foreground;
        Color::Rgb(c.r, c.g, c.b)
    };
    Look {
        text: Style::new().fg(P.editor_fg),
        heading: Style::new().fg(fg("markup.heading.markdown")).bold(),
        bullet: Style::new().fg(fg("punctuation.definition.list_item.markdown")),
        quote: Style::new().fg(fg("punctuation.definition.blockquote.markdown")),
        code: Style::new().fg(fg("markup.raw.inline.markdown")),
        block: Style::new().fg(P.editor_fg).bg(P.widget_bg),
        link: Style::new().fg(link).underlined(),
        rule: Style::new().fg(P.line_nr),
    }
}

/// A block being gathered: its first-row prefix, the indent of its other rows, its text.
struct Para {
    lead: Vec<Span<'static>>,
    hang: usize,
    text: String,
    base: Style,
    /// List item indent in the source: deeper lines continue it.
    indent: Option<usize>,
}

pub fn render(src: &[String], width: usize, link: Color) -> Vec<Line<'static>> {
    let lk = look(link);
    let width = width.max(20);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Option<Para> = None;
    let flush = |cur: &mut Option<Para>, out: &mut Vec<Line<'static>>| {
        if let Some(p) = cur.take() {
            out.extend(wrap(p.lead, inline(&p.text, p.base, &lk), p.hang, width));
        }
    };
    let mut i = 0;
    while i < src.len() {
        let line = src[i].replace('\t', "    ");
        let t = line.trim_start();
        let indent = line.len() - t.len();
        i += 1;
        if t.is_empty() {
            flush(&mut cur, &mut out);
            if out.last().is_some_and(|l| l.width() > 0) {
                out.push(Line::default());
            }
            continue;
        }
        if t.starts_with("<!--") {
            // An HTML comment, to its end.
            let mut l = t.to_string();
            while !l.contains("-->") && i < src.len() {
                l = src[i].clone();
                i += 1;
            }
            continue;
        }
        if let Some(fence) = ["```", "~~~"].into_iter().find(|f| t.starts_with(f)) {
            flush(&mut cur, &mut out);
            let lang = t[3..].split_whitespace().next().unwrap_or("").to_string();
            let mut code = Vec::new();
            while i < src.len() && !src[i].trim_start().starts_with(fence) {
                code.push(src[i].clone());
                i += 1;
            }
            i += 1; // the closing fence
            out.extend(code_block(&lang, code, width, &lk));
            continue;
        }
        let hashes = t.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) && t[hashes..].chars().next().is_none_or(|c| c == ' ') {
            flush(&mut cur, &mut out);
            let text = t[hashes..].trim().trim_end_matches('#').trim_end();
            let rows = wrap(Vec::new(), inline(text, lk.heading, &lk), 0, width);
            let w = rows.iter().map(Line::width).max().unwrap_or(0);
            out.extend(rows);
            if hashes <= 2 {
                let glyph = if hashes == 1 { "━" } else { "─" };
                out.push(Line::styled(glyph.repeat(if hashes == 1 { width } else { w.max(1) }), lk.rule));
            }
            continue;
        }
        if t.len() >= 3 && t.chars().all(|c| matches!(c, '-' | ' ')) && t.contains("---")
            || t.chars().all(|c| matches!(c, '*' | ' ')) && t.matches('*').count() >= 3
            || t.chars().all(|c| matches!(c, '_' | ' ')) && t.matches('_').count() >= 3
        {
            flush(&mut cur, &mut out);
            out.push(Line::styled("─".repeat(width), lk.rule));
            continue;
        }
        if t.starts_with('|') {
            flush(&mut cur, &mut out);
            let mut rows = vec![t.to_string()];
            while i < src.len() && src[i].trim_start().starts_with('|') {
                rows.push(src[i].trim().to_string());
                i += 1;
            }
            out.extend(table(&rows, &lk));
            continue;
        }
        if let Some(q) = t.strip_prefix('>') {
            let q = q.strip_prefix(' ').unwrap_or(q);
            match cur.as_mut().filter(|p| p.base == lk.quote.italic()) {
                Some(p) => push_text(&mut p.text, q),
                None => {
                    flush(&mut cur, &mut out);
                    // Every row keeps the bar: `wrap` repeats a quote's lead.
                    cur = Some(Para { lead: vec![Span::styled("▎ ", lk.quote)], hang: 2, text: q.to_string(), base: lk.quote.italic(), indent: None });
                }
            }
            continue;
        }
        if let Some((marker, rest)) = list_item(t) {
            flush(&mut cur, &mut out);
            let depth = indent / 2;
            let (glyph, rest) = match rest.strip_prefix("[ ] ").map(|r| ("☐", r)).or_else(|| rest.strip_prefix("[x] ").or_else(|| rest.strip_prefix("[X] ")).map(|r| ("☑", r))) {
                Some((g, r)) => (g.to_string(), r),
                None if marker.ends_with(['.', ')']) => (marker.to_string(), rest),
                None => (["•", "◦", "▪"][depth % 3].to_string(), rest),
            };
            let pad = "  ".repeat(depth + 1);
            let hang = pad.width() + glyph.width() + 1;
            cur = Some(Para {
                lead: vec![Span::raw(pad), Span::styled(format!("{glyph} "), lk.bullet)],
                hang,
                text: rest.to_string(),
                base: lk.text,
                indent: Some(indent),
            });
            continue;
        }
        if indent >= 4 && cur.is_none() {
            // An indented code block.
            let mut code = vec![line[4.min(line.len())..].to_string()];
            while i < src.len() && (src[i].starts_with("    ") || src[i].starts_with('\t')) {
                code.push(src[i].replace('\t', "    ")[4..].to_string());
                i += 1;
            }
            out.extend(code_block("", code, width, &lk));
            continue;
        }
        match cur.as_mut() {
            // Continuation: a paragraph's next line, or deeper than its list item.
            Some(p) if p.indent.is_none_or(|n| indent > n) => push_text(&mut p.text, t),
            _ => {
                flush(&mut cur, &mut out);
                cur = Some(Para { lead: Vec::new(), hang: 0, text: t.to_string(), base: lk.text, indent: None });
            }
        }
        // Two trailing spaces or a backslash end the row here.
        if line.ends_with("  ") || line.ends_with('\\') {
            if let Some(p) = cur.as_mut() {
                p.text = p.text.trim_end_matches('\\').trim_end().to_string();
            }
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    while out.last().is_some_and(|l| l.width() == 0) {
        out.pop();
    }
    out
}

fn push_text(text: &mut String, more: &str) {
    if !text.is_empty() {
        text.push(' ');
    }
    text.push_str(more.trim());
}

/// `- x`, `* x`, `+ x`, `1. x`, `1) x`: the marker and the text.
fn list_item(t: &str) -> Option<(&str, &str)> {
    if let Some(rest) = t.strip_prefix(['-', '*', '+']).and_then(|r| r.strip_prefix(' ')) {
        return Some((&t[..1], rest));
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    let after = t.get(digits..digits + 2)?;
    (digits > 0 && digits <= 9 && (after == ". " || after == ") ")).then(|| (&t[..digits + 1], &t[digits + 2..]))
}

/// Inline Markdown: code, bold, italic, strikethrough, links and images (as their text).
fn inline(text: &str, base: Style, lk: &Look) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut plain = String::new();
    while let Some(c) = rest.chars().next() {
        let flush = |plain: &mut String, out: &mut Vec<Span<'static>>| {
            if !plain.is_empty() {
                out.push(Span::styled(std::mem::take(plain), base));
            }
        };
        if c == '\\' && rest.len() > 1 {
            let n = rest[1..].chars().next().map_or(1, char::len_utf8);
            plain.push_str(&rest[1..1 + n]);
            rest = &rest[1 + n..];
            continue;
        }
        if let Some(tail) = rest.strip_prefix('`') {
            if let Some((body, after)) = tail.split_once('`').filter(|(b, _)| !b.is_empty()) {
                flush(&mut plain, &mut out);
                out.push(Span::styled(body.to_string(), base.patch(lk.code)));
                rest = after;
                continue;
            }
        }
        let emphasis = [("**", Style::new().bold()), ("__", Style::new().bold()), ("~~", Style::new().crossed_out()), ("*", Style::new().italic())];
        if let Some((st, body, after)) = emphasis.iter().find_map(|&(m, st)| {
            let (body, after) = rest.strip_prefix(m)?.split_once(m)?;
            (!body.is_empty() && !body.starts_with(' ')).then_some((st, body, after))
        }) {
            flush(&mut plain, &mut out);
            out.extend(inline(body, base.patch(st), lk));
            rest = after;
            continue;
        }
        let image = rest.starts_with("![");
        if let Some(tail) = rest.strip_prefix('!').unwrap_or(rest).strip_prefix('[') {
            if let Some((label, tail)) = tail.split_once("](") {
                if let Some((_, after)) = tail.split_once(')') {
                    flush(&mut plain, &mut out);
                    if image {
                        out.push(Span::styled(format!("[image: {label}]"), base.patch(lk.rule)));
                    } else {
                        out.extend(inline(label, base.patch(lk.link), lk));
                    }
                    rest = after;
                    continue;
                }
            }
        }
        if let Some((url, after)) = rest.strip_prefix('<').and_then(|t| t.split_once('>')).filter(|(u, _)| u.starts_with("http")) {
            flush(&mut plain, &mut out);
            out.push(Span::styled(url.to_string(), base.patch(lk.link)));
            rest = after;
            continue;
        }
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !plain.is_empty() {
        out.push(Span::styled(plain, base));
    }
    out
}

/// `lead` then `body`, word-wrapped to `width`; rows after the first start with `hang`
/// blanks. A word longer than a row is cut.
fn wrap(lead: Vec<Span<'static>>, body: Vec<Span<'static>>, hang: usize, width: usize) -> Vec<Line<'static>> {
    let width = width.max(hang + 8);
    let indent = |lead: &[Span<'static>]| {
        // A quote's bar continues down its rows.
        match lead.first() {
            Some(s) if s.content.starts_with('▎') => vec![s.clone()],
            _ => vec![Span::raw(" ".repeat(hang))],
        }
    };
    let next = indent(&lead);
    let mut rows = vec![lead];
    let mut x: usize = rows[0].iter().map(Span::width).sum();
    for span in body {
        for word in span.content.split_inclusive(' ') {
            let mut word = word.to_string();
            if x + word.trim_end().width() > width && x > hang && word.trim_end().width() <= width - hang {
                rows.push(next.clone());
                x = hang;
                word = word.trim_start().to_string();
            }
            loop {
                let room = width.saturating_sub(x);
                if word.trim_end().width() <= room {
                    break;
                }
                let mut cut = 0;
                let mut w = 0;
                for (i, c) in word.char_indices() {
                    let cw = c.to_string().width();
                    if w + cw > room {
                        break;
                    }
                    w += cw;
                    cut = i + c.len_utf8();
                }
                if cut == 0 && x > hang {
                    rows.push(next.clone());
                    x = hang;
                    continue;
                }
                let cut = if cut == 0 { word.chars().next().map_or(word.len(), char::len_utf8) } else { cut };
                rows.last_mut().unwrap().push(Span::styled(word[..cut].to_string(), span.style));
                rows.push(next.clone());
                x = hang;
                word = word[cut..].to_string();
            }
            if !word.is_empty() {
                x += word.width();
                rows.last_mut().unwrap().push(Span::styled(word, span.style));
            }
        }
    }
    rows.into_iter().map(Line::from).collect()
}

/// A fence's info word as the extension the editor's syntaxes know.
fn ext(lang: &str) -> &str {
    match lang.to_ascii_lowercase().as_str() {
        "rust" => "rs",
        "python" | "python3" => "py",
        "javascript" | "node" => "js",
        "typescript" => "ts",
        "shell" | "console" | "shell-session" | "zsh" | "sh" | "bash" => "sh",
        "golang" => "go",
        "ruby" => "rb",
        "yml" => "yaml",
        "c++" => "cpp",
        "jsonc" | "json5" => "json",
        _ => lang,
    }
}

/// Fenced or indented code on the code background, highlighted as its language.
fn code_block(lang: &str, code: Vec<String>, width: usize, lk: &Look) -> Vec<Line<'static>> {
    let n = code.len();
    let mut b = Buffer::from_lines(&format!("code.{}", ext(lang)), code, "preview");
    b.highlight_upto(n);
    let pad = |l: &mut Line<'static>| {
        let w = l.width();
        if w < width {
            l.spans.push(Span::styled(" ".repeat(width - w), lk.block));
        }
    };
    let mut out = vec![Line::default()];
    for i in 0..n {
        let mut l = crate::ui::render_line(&b.lines[i], b.segs(i), &[], None, 0, width.saturating_sub(4), lk.block);
        l.spans.insert(0, Span::styled("  ", lk.block));
        pad(&mut l);
        out.push(l);
    }
    pad(&mut out[0]);
    let mut last = Line::default();
    pad(&mut last);
    out.push(last);
    out
}

/// A pipe table, columns aligned, the header bold over a rule.
fn table(rows: &[String], lk: &Look) -> Vec<Line<'static>> {
    let cells = |r: &str| -> Vec<String> {
        let r = r.trim();
        let r = r.strip_prefix('|').unwrap_or(r);
        let r = r.strip_suffix('|').unwrap_or(r);
        r.split('|').map(|c| c.trim().to_string()).collect()
    };
    let mut grid: Vec<Vec<String>> = rows.iter().map(|r| cells(r)).collect();
    let rule = |r: &[String]| r.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')));
    let header = grid.len() > 1 && rule(&grid[1]);
    if header {
        grid.remove(1);
    }
    let spans: Vec<Vec<Vec<Span<'static>>>> = grid
        .iter()
        .enumerate()
        .map(|(r, row)| row.iter().map(|c| inline(c, if header && r == 0 { lk.text.bold() } else { lk.text }, lk)).collect())
        .collect();
    let cols = spans.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols).map(|c| spans.iter().filter_map(|r| r.get(c)).map(|s| s.iter().map(Span::width).sum::<usize>()).max().unwrap_or(0)).collect();
    let mut out = Vec::new();
    for (r, row) in spans.into_iter().enumerate() {
        let mut line = vec![Span::raw("  ")];
        for (c, cell) in row.into_iter().enumerate() {
            if c > 0 {
                line.push(Span::styled(" │ ", lk.rule));
            }
            let w: usize = cell.iter().map(Span::width).sum();
            line.extend(cell);
            line.push(Span::raw(" ".repeat(widths[c] - w)));
        }
        out.push(Line::from(line));
        if header && r == 0 {
            let parts: Vec<String> = widths.iter().map(|&w| "─".repeat(w)).collect();
            out.push(Line::from(vec![Span::raw("  "), Span::styled(parts.join("─┼─"), lk.rule)]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(src: &str, width: usize) -> Vec<String> {
        let lines: Vec<String> = src.lines().map(str::to_string).collect();
        render(&lines, width, Color::Blue).iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>().trim_end().to_string()).collect()
    }

    #[test]
    fn paragraphs_reflow_and_lists_hang_under_their_text() {
        let out = text("# Title\n\nOne two\nthree four five six seven.\n\n- an item that wraps onto\n  a second source line\n  1. nested\n", 24);
        assert_eq!(
            out,
            [
                "Title",
                "━━━━━━━━━━━━━━━━━━━━━━━━",
                "",
                "One two three four five",
                "six seven.",
                "",
                "  • an item that wraps",
                "    onto a second source",
                "    line",
                "    1. nested",
            ]
        );
    }

    #[test]
    fn inline_marks_become_styles_not_text() {
        let lk = look(Color::Blue);
        let spans = inline("a `code` **bold** [link](http://x) ![pic](p.png) \\*", lk.text, &lk);
        let s: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(s, "a code bold link [image: pic] *");
        assert!(spans.iter().any(|s| s.content == "bold" && s.style.add_modifier.contains(Modifier::BOLD)));
        assert!(spans.iter().any(|s| s.content == "link" && s.style.add_modifier.contains(Modifier::UNDERLINED)));
    }

    #[test]
    fn code_tables_quotes_rules_and_comments() {
        let out = text("```sh\necho hi\n```\n| Key | Action |\n|---|---|\n| F1 | palette |\n\n> quoted\n> more\n\n---\n<!-- hidden -->\nend\n", 30);
        assert_eq!(
            out,
            ["", "  echo hi", "", "  Key │ Action", "  ────┼────────", "  F1  │ palette", "", "▎ quoted more", "", "──────────────────────────────", "end"]
        );
    }
}

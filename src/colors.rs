//! Colour depth. termcode paints in 24-bit colour; a terminal that knows only 256 or 16
//! colours misreads those codes (GNU screen turns the editor yellow). So after each frame
//! is drawn, its colours are fitted to what the terminal shows: the nearest of xterm's
//! 256 (its colour cube and grey ramp, never the 16 the user's theme may have changed),
//! or the nearest of the 16 basic ones.

use ratatui::buffer::Buffer;
use ratatui::style::Color;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Depth {
    True,
    C256,
    C16,
}

/// `termcode.colors` (0: auto).
static SET: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
thread_local! {
    static TEST: std::cell::Cell<Option<Depth>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub fn test_depth(d: Option<Depth>) {
    TEST.with(|c| c.set(d));
}

pub fn set(d: Option<Depth>) {
    SET.store(d.map_or(0, |d| d as u8 + 1), Ordering::Relaxed);
}

/// TERMCODE_COLORS (`truecolor`, `256`, `16`) wins over the setting, the setting over
/// what the environment says.
pub fn depth() -> Depth {
    #[cfg(test)]
    return TEST.with(|c| c.get()).unwrap_or(Depth::True);
    #[cfg(not(test))]
    {
        static ENV: OnceLock<(Option<Depth>, Depth)> = OnceLock::new();
        let (forced, auto) = *ENV.get_or_init(|| {
            let var = |k: &str| std::env::var(k).ok();
            (parse(var("TERMCODE_COLORS").as_deref().unwrap_or("")), detect(&var))
        });
        forced.unwrap_or(match SET.load(Ordering::Relaxed) {
            1 => Depth::True,
            2 => Depth::C256,
            3 => Depth::C16,
            _ => auto,
        })
    }
}

/// A `termcode.colors` / TERMCODE_COLORS value; anything else is auto.
pub fn parse(v: &str) -> Option<Depth> {
    match v {
        "truecolor" | "24bit" => Some(Depth::True),
        "256" => Some(Depth::C256),
        "16" => Some(Depth::C16),
        _ => None,
    }
}

/// What the terminal shows, from its environment. GNU screen first: it keeps the outer
/// terminal's COLORTERM but shows at most 256 colours. tmux and herdr draw the panes
/// themselves and convert for the terminal outside, so they take 24-bit colour.
pub fn detect(var: &dyn Fn(&str) -> Option<String>) -> Depth {
    let term = var("TERM").unwrap_or_default();
    let set = |k: &str| var(k).is_some_and(|v| !v.is_empty());
    let fallback = if term.contains("256") { Depth::C256 } else { Depth::C16 };
    if set("STY") && !term.starts_with("tmux") {
        return fallback;
    }
    let truecolor = matches!(var("COLORTERM").as_deref(), Some("truecolor" | "24bit"))
        || term.ends_with("-direct")
        || set("TMUX")
        || set("HERDR_ENV")
        || ["kitty", "ghostty", "alacritty", "foot", "wezterm", "contour", "rio"].iter().any(|t| term.contains(t))
        || matches!(var("TERM_PROGRAM").as_deref(), Some("ghostty" | "iTerm.app" | "WezTerm" | "vscode" | "Hyper" | "Tabby" | "rio"));
    if truecolor {
        Depth::True
    } else {
        fallback
    }
}

/// Fits a drawn frame's colours to the terminal's depth.
pub fn fit(buf: &mut Buffer) {
    let d = depth();
    if d == Depth::True {
        return;
    }
    for cell in buf.content.iter_mut() {
        cell.fg = fit_color(cell.fg, d);
        cell.bg = fit_color(cell.bg, d);
        cell.underline_color = fit_color(cell.underline_color, d);
    }
}

fn fit_color(c: Color, d: Depth) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    match d {
        Depth::True => c,
        Depth::C256 => Color::Indexed(to256(r, g, b)),
        Depth::C16 => to16(r, g, b),
    }
}

fn dist((r, g, b): (u8, u8, u8), (r2, g2, b2): (u8, u8, u8)) -> i32 {
    let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
    d(r, r2) + d(g, g2) + d(b, b2)
}

/// The nearest of xterm's colour cube (16-231) and grey ramp (232-255).
pub fn to256(r: u8, g: u8, b: u8) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let step = |v: u8| (0..6).min_by_key(|&i| (STEPS[i] as i32 - v as i32).abs()).unwrap();
    let (ri, gi, bi) = (step(r), step(g), step(b));
    let cube = (STEPS[ri], STEPS[gi], STEPS[bi]);
    let avg = (r as i32 + g as i32 + b as i32) / 3;
    let n = ((avg - 8 + 5) / 10).clamp(0, 23);
    let grey = (8 + 10 * n) as u8;
    if dist((r, g, b), (grey, grey, grey)) < dist((r, g, b), cube) {
        232 + n as u8
    } else {
        16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8
    }
}

/// The nearest of the 16 basic colours (xterm's defaults).
pub fn to16(r: u8, g: u8, b: u8) -> Color {
    const BASIC: [((u8, u8, u8), Color); 16] = [
        ((0, 0, 0), Color::Black),
        ((205, 0, 0), Color::Red),
        ((0, 205, 0), Color::Green),
        ((205, 205, 0), Color::Yellow),
        ((0, 0, 238), Color::Blue),
        ((205, 0, 205), Color::Magenta),
        ((0, 205, 205), Color::Cyan),
        ((229, 229, 229), Color::Gray),
        ((127, 127, 127), Color::DarkGray),
        ((255, 0, 0), Color::LightRed),
        ((0, 255, 0), Color::LightGreen),
        ((255, 255, 0), Color::LightYellow),
        ((92, 92, 255), Color::LightBlue),
        ((255, 0, 255), Color::LightMagenta),
        ((0, 255, 255), Color::LightCyan),
        ((255, 255, 255), Color::White),
    ];
    BASIC.iter().min_by_key(|(rgb, _)| dist((r, g, b), *rgb)).unwrap().1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminal_environment_decides_the_depth() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        for (pairs, want) in [
            (&[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")][..], Depth::True),
            (&[("TERM", "xterm-ghostty")], Depth::True),
            (&[("TERM", "xterm-kitty")], Depth::True),
            (&[("TERM", "tmux-256color"), ("TMUX", "/tmp/s,1,0")], Depth::True),
            (&[("TERM", "xterm-256color"), ("HERDR_ENV", "1")], Depth::True),
            (&[("TERM", "xterm-256color"), ("TERM_PROGRAM", "iTerm.app")], Depth::True),
            (&[("TERM", "xterm-256color"), ("TERM_PROGRAM", "Apple_Terminal")], Depth::C256),
            (&[("TERM", "xterm-256color")], Depth::C256),
            (&[("TERM", "screen.xterm-256color"), ("STY", "1.pts"), ("COLORTERM", "truecolor")], Depth::C256),
            (&[("TERM", "screen"), ("STY", "1.pts")], Depth::C16),
            (&[("TERM", "linux")], Depth::C16),
        ] {
            assert_eq!(detect(&env(pairs)), want, "{pairs:?}");
        }
        assert_eq!((parse("256"), parse("16"), parse("truecolor"), parse("auto")), (Some(Depth::C256), Some(Depth::C16), Some(Depth::True), None));
    }

    #[test]
    fn colours_fit_the_nearest_of_256_or_16() {
        assert_eq!(to256(0, 0, 0), 16);
        assert_eq!(to256(255, 255, 255), 231);
        assert_eq!(to256(30, 30, 30), 234, "the dark editor background: a grey");
        assert_eq!(to256(0, 127, 212), 32, "focus blue: the cube");
        assert_eq!((to16(30, 30, 30), to16(212, 212, 212), to16(241, 76, 76)), (Color::Black, Color::Gray, Color::LightRed));
        let mut buf = Buffer::empty(ratatui::layout::Rect::new(0, 0, 2, 1));
        buf[(0, 0)].set_fg(Color::Rgb(30, 30, 30)).set_bg(Color::Indexed(3));
        test_depth(Some(Depth::C256));
        fit(&mut buf);
        test_depth(None);
        assert_eq!((buf[(0, 0)].fg, buf[(0, 0)].bg), (Color::Indexed(234), Color::Indexed(3)), "indexed colours stay");
    }
}

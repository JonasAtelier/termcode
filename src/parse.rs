//! Pure parsers and small helpers, kept free of I/O so they can be unit tested.

/// One entry of `git status --porcelain=v1 -z`.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusEntry {
    pub x: char,
    pub y: char,
    pub path: String,
    /// Source path of a rename/copy.
    pub orig: Option<String>,
}

/// Parse `git status --porcelain=v1 -z`. With `-z` paths are never quoted, and a
/// rename/copy is `XY <to>\0<from>\0` (the source follows as its own field).
pub fn parse_porcelain_z(out: &[u8]) -> Vec<StatusEntry> {
    let mut fields = out
        .split(|&b| b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned());
    let mut entries = Vec::new();
    while let Some(f) = fields.next() {
        // "XY p" is at least 4 bytes; the empty field after the last NUL is skipped.
        if f.len() < 4 {
            continue;
        }
        let mut cs = f.chars();
        let (x, y) = (cs.next().unwrap_or(' '), cs.next().unwrap_or(' '));
        let orig = if matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C') {
            fields.next()
        } else {
            None
        };
        entries.push(StatusEntry { x, y, path: f[3..].to_string(), orig });
    }
    entries
}

/// One ripgrep match.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub path: String,
    pub line: usize,
    pub col: usize,
    pub text: String,
}

/// Parse one `rg --null --line-number --column --no-heading` line:
/// `path\0line:col:text`. `--null` keeps paths containing `:` unambiguous.
pub fn parse_rg_line(s: &str) -> Option<Hit> {
    let (path, rest) = s.split_once('\0')?;
    let mut it = rest.splitn(3, ':');
    let line = it.next()?.parse().ok()?;
    let col = it.next()?.parse().ok()?;
    Some(Hit {
        path: path.to_string(),
        line,
        col,
        text: it.next().unwrap_or("").trim_end_matches('\r').to_string(),
    })
}

/// File name stem for a (scope, path) pair: launch locks, hot-exit backups.
/// FNV-1a: stable across builds, unlike `DefaultHasher`.
pub fn lock_key(session: &str, root: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session.bytes().chain([0]).chain(root.bytes()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Quick-open rank, lower is better, VS Code-like tiers: exact file name, file
/// name prefix, substring of the file name, substring of the path, then a
/// subsequence. Ties go to the shorter path.
pub fn match_score(candidate: &str, query: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(candidate.len());
    }
    let c = candidate.to_lowercase();
    let q = query.to_lowercase();
    let name = c.rsplit('/').next().unwrap_or(&c);
    let tier = if name == q {
        0
    } else if name.starts_with(&q) {
        1
    } else if name.contains(&q) {
        2
    } else if c.contains(&q) {
        3
    } else {
        let mut it = c.chars();
        if !q.chars().all(|qc| it.any(|cc| cc == qc)) {
            return None;
        }
        4
    };
    Some(tier * 100_000 + c.len().min(99_999))
}

/// Standard base64 read back (an extension's picture); None at anything else. Padding
/// and line breaks are fine.
pub fn unbase64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let digits: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).take_while(|&c| c != b'=').map(val).collect::<Option<_>>()?;
    if digits.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for c in digits.chunks(4) {
        let n = c.iter().enumerate().fold(0u32, |n, (i, &d)| n | u32::from(d) << (18 - 6 * i));
        out.extend_from_slice(&n.to_be_bytes()[1..c.len()]);
    }
    Some(out)
}

/// Standard base64, for the OSC 52 clipboard escape.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = u32::from(c[0]) << 16
            | u32::from(*c.get(1).unwrap_or(&0)) << 8
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            s.push(if i <= c.len() {
                T[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    s
}

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A key as keybindings name it: the key and Ctrl/Alt/Shift, a letter in lower case
/// (Shift+P arrives as `P` from some terminals and as Shift+`p` from others).
pub type Chord = (KeyCode, KeyModifiers);

pub fn chord(k: &KeyEvent) -> Chord {
    let mods = k.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Char(c) if c.is_ascii_uppercase() => (KeyCode::Char(c.to_ascii_lowercase()), mods | KeyModifiers::SHIFT),
        code => (code, mods),
    }
}

/// `"ctrl+alt+p"`, `"shift+f12"`, `"alt+left"`: a keybinding's key.
pub fn parse_chord(s: &str) -> Result<Chord, String> {
    let mut mods = KeyModifiers::NONE;
    let lower = s.trim().to_ascii_lowercase();
    // "ctrl++" is Ctrl and the plus key.
    let (head, key) = match lower.strip_suffix("++") {
        Some(h) => (h, "+"),
        None => lower.rsplit_once('+').unwrap_or(("", lower.as_str())),
    };
    for m in head.split('+').filter(|m| !m.is_empty()) {
        mods |= match m {
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "alt" | "meta" | "option" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => return Err(format!("{s:?}: unknown modifier {m:?} (ctrl, alt, shift)")),
        };
    }
    let code = match key {
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        f if f.starts_with('f') && f[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => KeyCode::F(f[1..].parse().unwrap()),
        c if c.chars().count() == 1 => KeyCode::Char(c.chars().next().unwrap()),
        _ => return Err(format!("{s:?}: unknown key {key:?}")),
    };
    Ok(chord(&KeyEvent::new(code, mods)))
}

/// "Ctrl+Alt+P": a chord as the palette shows it.
pub fn chord_label((code, mods): Chord) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (m, name) in [(KeyModifiers::CONTROL, "Ctrl"), (KeyModifiers::ALT, "Alt"), (KeyModifiers::SHIFT, "Shift")] {
        if mods.contains(m) {
            parts.push(name.into());
        }
    }
    parts.push(match code {
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::PageUp => "PgUp".into(),
        KeyCode::PageDown => "PgDn".into(),
        KeyCode::Left => "←".into(),
        KeyCode::Right => "→".into(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        other => format!("{other:?}"),
    });
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keybinding_keys_parse_and_match_what_terminals_send() {
        let c = KeyModifiers::CONTROL;
        assert_eq!(parse_chord("ctrl+alt+p"), Ok((KeyCode::Char('p'), c | KeyModifiers::ALT)));
        assert_eq!(parse_chord("Ctrl+Shift+P"), Ok((KeyCode::Char('p'), c | KeyModifiers::SHIFT)));
        assert_eq!(parse_chord("shift+f12"), Ok((KeyCode::F(12), KeyModifiers::SHIFT)));
        assert_eq!(parse_chord("alt+left"), Ok((KeyCode::Left, KeyModifiers::ALT)));
        assert_eq!(parse_chord("ctrl++"), Ok((KeyCode::Char('+'), c)));
        assert_eq!(parse_chord("f5"), Ok((KeyCode::F(5), KeyModifiers::NONE)));
        assert!(parse_chord("hyper+x").unwrap_err().contains("unknown modifier"));
        assert!(parse_chord("ctrl+banana").unwrap_err().contains("unknown key"));
        // Shift+P as an upper-case P (legacy) or as Shift and p (kitty keys): one chord.
        let legacy = chord(&KeyEvent::new(KeyCode::Char('P'), c));
        let kitty = chord(&KeyEvent::new(KeyCode::Char('p'), c | KeyModifiers::SHIFT));
        assert_eq!((legacy, kitty), (parse_chord("ctrl+shift+p").unwrap(), parse_chord("ctrl+shift+p").unwrap()));
        assert_eq!(chord_label(parse_chord("ctrl+alt+p").unwrap()), "Ctrl+Alt+P");
    }
    use std::process::Command;

    #[test]
    fn porcelain_z_handles_rename_cjk_and_spaces() {
        let out = "R  new name.rs\0old name.rs\0 M 中文 檔案.txt\0?? dir/a b\0A  added.rs\0MM both.rs\0".as_bytes();
        let e = parse_porcelain_z(out);
        assert_eq!(e.len(), 5);
        assert_eq!(
            e[0],
            StatusEntry { x: 'R', y: ' ', path: "new name.rs".into(), orig: Some("old name.rs".into()) }
        );
        assert_eq!((e[1].x, e[1].y, e[1].path.as_str()), (' ', 'M', "中文 檔案.txt"));
        assert_eq!((e[2].x, e[2].path.as_str()), ('?', "dir/a b"));
        assert_eq!(e[3].orig, None);
        assert_eq!((e[4].x, e[4].y), ('M', 'M'));
        assert!(parse_porcelain_z(b"").is_empty());
    }

    #[test]
    fn rg_line_parses_and_keeps_colons() {
        let h = parse_rg_line("src/a:b.rs\x0012:5:let x = \"a:b\";").unwrap();
        assert_eq!(h, Hit { path: "src/a:b.rs".into(), line: 12, col: 5, text: "let x = \"a:b\";".into() });
        assert_eq!(parse_rg_line("中文.txt\x003:1:").unwrap().text, "");
        assert!(parse_rg_line("no-null:1:2:x").is_none());
        assert!(parse_rg_line("p\x00x:1:t").is_none());
    }

    #[test]
    fn lock_key_is_stable_and_distinct() {
        assert_eq!(lock_key("default", "/r"), lock_key("default", "/r"));
        assert_ne!(lock_key("default", "/r"), lock_key("work", "/r"));
        assert_ne!(lock_key("default", "/r"), lock_key("default", "/r2"));
        // The separator keeps ("ab","c") and ("a","bc") apart.
        assert_ne!(lock_key("ab", "c"), lock_key("a", "bc"));
        assert_eq!(lock_key("default", "/r").len(), 16);
    }

    #[test]
    fn lock_is_exclusive_until_dropped() {
        let p = std::env::temp_dir().join(format!("termcode-lock-{}/k.json", std::process::id()));
        let first = crate::try_lock(&p).unwrap();
        assert!(first.is_some());
        // A second open file description cannot take it, even in the same process.
        assert!(crate::try_lock(&p).unwrap().is_none());
        drop(first);
        // Retry briefly: a child forked by a parallel test shares the lock's open
        // file description from fork until its exec (O_CLOEXEC closes it then).
        let again = (0..100).any(|_| {
            let ok = crate::try_lock(&p).unwrap().is_some();
            if !ok {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            ok
        });
        assert!(again, "lock not released after drop");
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn repo_root_from_subdir_and_outside() {
        let tmp = std::env::temp_dir().join(format!("termcode-root-{}", std::process::id()));
        let sub = tmp.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        let ok = Command::new("git").args(["init", "-q"]).arg(&tmp).status().unwrap();
        assert!(ok.success());
        assert_eq!(crate::repo_root(&sub), Some(tmp.canonicalize().unwrap()));
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(crate::repo_root(&std::env::temp_dir()), None);
    }

    #[test]
    fn quick_open_and_base64() {
        assert!(match_score("src/main.rs", "main").unwrap() < match_score("src/main.rs", "smr").unwrap());
        let exact = match_score("drivers/net/dsa/mxl862xx/mxl862xx.c", "mxl862xx.c").unwrap();
        let other = match_score("net/dsa/tag_mxl862xx.c", "mxl862xx.c").unwrap();
        assert!(exact < other, "exact file name ranks first");
        assert!(match_score("a/mxl862xx_host.c", "mxl862xx").unwrap() < other);
        assert_eq!(match_score("src/main.rs", "xyz"), None);
        assert_eq!(base64(b"hi"), "aGk=");
        assert_eq!(base64(b"herdr"), "aGVyZHI=");
        assert_eq!(base64("中".as_bytes()), "5Lit");
    }
}

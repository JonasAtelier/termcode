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

/// One `git log --format=%H%x1f%h%x1f%an%x1f%ar%x1f%s` line.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

pub fn parse_log_line(s: &str) -> Option<Commit> {
    let f: Vec<&str> = s.splitn(5, '\x1f').collect();
    let [hash, short, author, date, subject] = f[..] else { return None };
    Some(Commit {
        hash: hash.into(),
        short: short.into(),
        author: author.into(),
        date: date.into(),
        subject: subject.into(),
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

#[cfg(test)]
mod tests {
    use super::*;
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
    fn log_line_parses() {
        let c = parse_log_line("abc123\x1fabc\x1fJonas\x1f2 days ago\x1ffix: a\x1fb").unwrap();
        assert_eq!((c.short.as_str(), c.author.as_str(), c.subject.as_str()), ("abc", "Jonas", "fix: a\x1fb"));
        assert!(parse_log_line("short\x1fline").is_none());
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
        let p = std::env::temp_dir().join(format!("herdr-code-lock-{}/k.json", std::process::id()));
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
        let tmp = std::env::temp_dir().join(format!("herdr-code-root-{}", std::process::id()));
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

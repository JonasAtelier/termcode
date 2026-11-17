//! A patch (`git diff`, `git format-patch`, `diff -u`) shown as the diff view shows
//! changes: each file's hunks side by side under a bar with its name, a hunk's `@@` line
//! between hunks, the files' own line numbers, the commit message above. The file's tab
//! turns into this and back (`patch:` keys, as `preview:` for Markdown).

use crate::diff::{Kind, Row};
use crate::panel::{App, Doc, DiffDoc};

/// A row of a patch's view beyond the diff's own lines.
#[derive(Clone, Debug, PartialEq)]
pub enum Head {
    /// A file: its name (with what happened to it).
    File(String),
    /// A hunk's `@@ -a,b +c,d @@ …` line.
    Hunk(String),
    /// The message and stats before the first file; a note in a file (binary).
    Text(String),
}

/// A patch's view: the rows that are heads, and each row's line numbers on both sides.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PatchRows {
    pub heads: std::collections::HashMap<usize, Head>,
    pub nums: Vec<[Option<usize>; 2]>,
    pub files: usize,
}

pub fn is_patch(rel: &str) -> bool {
    let ext = rel.rsplit_once('.').map_or("", |(_, e)| e);
    matches!(ext.to_ascii_lowercase().as_str(), "patch" | "diff")
}

/// `text` as rows (a head's row is blank on both sides, so each side's lines stay in
/// order for highlighting), its heads and numbers, and the files' paths.
pub fn parse(text: &str) -> (Vec<Row>, PatchRows, Vec<String>) {
    let mut rows = Vec::new();
    let mut p = PatchRows::default();
    let mut paths: Vec<String> = Vec::new();
    let (mut li, mut ri) = (0usize, 0usize);
    let head = |rows: &mut Vec<Row>, p: &mut PatchRows, li: &mut usize, ri: &mut usize, h: Head| {
        p.heads.insert(rows.len(), h);
        p.nums.push([None, None]);
        rows.push(Row { left: Some((*li, String::new())), right: Some((*ri, String::new())), kind: Kind::Same });
        *li += 1;
        *ri += 1;
    };
    // The run of `-` and `+` lines being gathered, paired as the diff view pairs them.
    let mut dels: Vec<(usize, String)> = Vec::new();
    let mut inss: Vec<(usize, String)> = Vec::new();
    let flush = |rows: &mut Vec<Row>, p: &mut PatchRows, li: &mut usize, ri: &mut usize, dels: &mut Vec<(usize, String)>, inss: &mut Vec<(usize, String)>| {
        let n = dels.len().max(inss.len());
        let (mut d, mut s) = (dels.drain(..), inss.drain(..));
        for _ in 0..n {
            let (a, b) = (d.next(), s.next());
            let kind = match (&a, &b) {
                (Some(_), Some(_)) => Kind::Change,
                (Some(_), None) => Kind::Del,
                _ => Kind::Ins,
            };
            p.nums.push([a.as_ref().map(|x| x.0), b.as_ref().map(|x| x.0)]);
            let left = a.map(|(_, t)| {
                *li += 1;
                (*li - 1, t)
            });
            let right = b.map(|(_, t)| {
                *ri += 1;
                (*ri - 1, t)
            });
            rows.push(Row { left, right, kind });
        }
    };
    // In a hunk: the old and new lines still to come, and the next numbers.
    let mut left_n = (0usize, 0usize);
    let mut right_n = (0usize, 0usize);
    let mut file: Option<(String, String, Vec<&str>)> = None;
    let mut started = false;
    let name = |old: &str, new: &str, notes: &[&str]| {
        let strip = |s: &str| s.strip_prefix("a/").or_else(|| s.strip_prefix("b/")).unwrap_or(s).to_string();
        let (o, n) = (strip(old), strip(new));
        if new == "/dev/null" {
            format!("{o} (deleted)")
        } else if old == "/dev/null" || notes.iter().any(|l| l.starts_with("new file")) {
            format!("{n} (new)")
        } else if o != n {
            format!("{o} → {n}")
        } else {
            n
        }
    };
    let mut lines = text.lines().peekable();
    while let Some(l) = lines.next() {
        if left_n.1 > 0 || right_n.1 > 0 {
            match l.chars().next() {
                Some(' ') | None => {
                    flush(&mut rows, &mut p, &mut li, &mut ri, &mut dels, &mut inss);
                    let t = l.get(1..).unwrap_or("").to_string();
                    p.nums.push([Some(left_n.0), Some(right_n.0)]);
                    rows.push(Row { left: Some((li, t.clone())), right: Some((ri, t)), kind: Kind::Same });
                    (li, ri) = (li + 1, ri + 1);
                    left_n = (left_n.0 + 1, left_n.1.saturating_sub(1));
                    right_n = (right_n.0 + 1, right_n.1.saturating_sub(1));
                }
                Some('-') => {
                    if !inss.is_empty() {
                        flush(&mut rows, &mut p, &mut li, &mut ri, &mut dels, &mut inss);
                    }
                    dels.push((left_n.0, l[1..].to_string()));
                    left_n = (left_n.0 + 1, left_n.1.saturating_sub(1));
                }
                Some('+') => {
                    inss.push((right_n.0, l[1..].to_string()));
                    right_n = (right_n.0 + 1, right_n.1.saturating_sub(1));
                }
                // "\ No newline at end of file"
                Some('\\') => {}
                _ => {
                    left_n.1 = 0;
                    right_n.1 = 0;
                }
            }
            if left_n.1 == 0 && right_n.1 == 0 {
                flush(&mut rows, &mut p, &mut li, &mut ri, &mut dels, &mut inss);
            }
            continue;
        }
        if let Some(rest) = l.strip_prefix("diff --git ") {
            // Its name comes from the ---/+++ lines (or, without hunks, from here).
            let (a, b) = rest.split_once(" b/").map_or((rest, rest), |(a, b)| (a, b));
            let b = format!("b/{b}");
            // The file before, named on its diff --git line only (a rename, a mode).
            if let Some((old, new, notes)) = file.take() {
                head(&mut rows, &mut p, &mut li, &mut ri, Head::File(name(&old, &new, &notes)));
                paths.push(new.trim_start_matches("b/").to_string());
                p.files += 1;
            }
            file = Some((a.to_string(), b, Vec::new()));
            started = true;
            continue;
        }
        if let (Some(old), Some(new)) = (l.strip_prefix("--- "), lines.peek().and_then(|n| n.strip_prefix("+++ "))) {
            let clean = |s: &str| s.split('\t').next().unwrap_or(s).trim().to_string();
            let (old, new) = (clean(old), clean(new));
            lines.next();
            let notes = file.take().map(|f| f.2).unwrap_or_default();
            let path = if new == "/dev/null" { old.clone() } else { new.clone() };
            head(&mut rows, &mut p, &mut li, &mut ri, Head::File(name(&old, &new, &notes)));
            paths.push(path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).unwrap_or(&path).to_string());
            p.files += 1;
            started = true;
            continue;
        }
        if let Some(rest) = l.strip_prefix("@@ -") {
            // A file named on its diff --git line only (no ---/+++): its bar now.
            if let Some((old, new, notes)) = file.take() {
                head(&mut rows, &mut p, &mut li, &mut ri, Head::File(name(&old, &new, &notes)));
                paths.push(new.trim_start_matches("b/").to_string());
                p.files += 1;
            }
            let range = |s: &str| -> (usize, usize) {
                let (a, b) = s.split_once(',').unwrap_or((s, "1"));
                (a.parse().unwrap_or(1), b.parse().unwrap_or(1))
            };
            let mut parts = rest.split(' ');
            let old = range(parts.next().unwrap_or(""));
            let new = range(parts.next().unwrap_or("").trim_start_matches('+'));
            left_n = (old.0.max(1), old.1);
            right_n = (new.0.max(1), new.1);
            head(&mut rows, &mut p, &mut li, &mut ri, Head::Hunk(l.to_string()));
            continue;
        }
        match file.as_mut() {
            // index, mode, similarity, rename and binary lines of a file.
            Some(f) if started => {
                if l.starts_with("Binary files") {
                    let (old, new, notes) = file.take().unwrap_or_default();
                    head(&mut rows, &mut p, &mut li, &mut ri, Head::File(name(&old, &new, &notes)));
                    head(&mut rows, &mut p, &mut li, &mut ri, Head::Text("Binary file, not shown".into()));
                    paths.push(new.trim_start_matches("b/").to_string());
                    p.files += 1;
                } else {
                    f.2.push(l);
                }
            }
            // Before the first file: the commit's header, message and stats.
            _ if !started => head(&mut rows, &mut p, &mut li, &mut ri, Head::Text(l.to_string())),
            _ => {}
        }
    }
    flush(&mut rows, &mut p, &mut li, &mut ri, &mut dels, &mut inss);
    if let Some((old, new, notes)) = file.take() {
        head(&mut rows, &mut p, &mut li, &mut ri, Head::File(name(&old, &new, &notes)));
        paths.push(new.trim_start_matches("b/").to_string());
        p.files += 1;
    }
    (rows, p, paths)
}

/// The diff of the patch `rel` (its open buffer's unsaved text, else the file), made
/// again when that changes.
pub fn ensure(app: &mut App, key: &str, rel: &str) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match app.docs.get(&format!("file:{rel}")) {
        Some(Doc::Text(b)) => b.identity().hash(&mut h),
        _ => std::fs::metadata(app.root.join(rel)).ok().map(|m| (m.len(), m.modified().ok())).hash(&mut h),
    }
    let stamp = h.finish();
    if app.patch_stamps.get(key) == Some(&stamp) && app.docs.contains_key(key) {
        return;
    }
    let text = match app.docs.get(&format!("file:{rel}")) {
        Some(Doc::Text(b)) => b.lines.join("\n"),
        _ => std::fs::read_to_string(app.root.join(rel)).unwrap_or_default(),
    };
    let (rows, p, paths) = parse(&text);
    // Highlighted as its files' language when they share one.
    let ext = |p: &str| p.rsplit_once('.').map(|(_, e)| e.to_string());
    let path = match paths.first() {
        Some(first) if paths.iter().all(|p| ext(p) == ext(first)) => first.clone(),
        _ => "patch.txt".into(),
    };
    let note = (p.files == 0).then(|| format!("{rel}: no file's changes in it (not a patch?)"));
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let file = crate::git::ScmFile { section: crate::git::Section::Changes, path, orig: None, letter: 'M' };
    let doc = DiffDoc { title: format!("Diff {base}"), rows, note, file, id: Default::default(), patch: Some(Box::new(p)) };
    app.docs.insert(key.to_string(), Doc::Diff(doc));
    app.patch_stamps.insert(key.to_string(), stamp);
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "From 1234 Mon Sep 17 00:00:00 2001
Subject: [PATCH] Say hello

---
 src/a.rs | 3 ++-
 1 file changed

diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,3 +10,4 @@ fn main() {
 keep
-old
+new
+added
 tail
--
2.43.0
";

    #[test]
    fn a_patch_reads_as_files_hunks_and_paired_lines() {
        let (rows, p, paths) = parse(PATCH);
        assert_eq!(paths, ["src/a.rs"]);
        let shown: Vec<String> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| match p.heads.get(&i) {
                Some(Head::File(n)) => format!("FILE {n}"),
                Some(Head::Hunk(h)) => format!("HUNK {h}"),
                Some(Head::Text(t)) => format!("TEXT {t}"),
                None => format!("{:?} {:?} | {:?} {:?}", p.nums[i][0], r.left.as_ref().map(|l| l.1.as_str()), p.nums[i][1], r.right.as_ref().map(|r| r.1.as_str())),
            })
            .collect();
        assert_eq!(
            shown,
            [
                "TEXT From 1234 Mon Sep 17 00:00:00 2001",
                "TEXT Subject: [PATCH] Say hello",
                "TEXT ",
                "TEXT ---",
                "TEXT  src/a.rs | 3 ++-",
                "TEXT  1 file changed",
                "TEXT ",
                "FILE src/a.rs",
                "HUNK @@ -10,3 +10,4 @@ fn main() {",
                "Some(10) Some(\"keep\") | Some(10) Some(\"keep\")",
                "Some(11) Some(\"old\") | Some(11) Some(\"new\")",
                "None None | Some(12) Some(\"added\")",
                "Some(12) Some(\"tail\") | Some(13) Some(\"tail\")",
            ],
            "the signature after the last hunk is not a deletion"
        );
        assert_eq!((rows[10].kind, rows[11].kind), (Kind::Change, Kind::Ins));
        // Each side's lines in order, heads included (as the highlighting reads them).
        let left: Vec<usize> = rows.iter().filter_map(|r| r.left.as_ref().map(|l| l.0)).collect();
        assert!(left.windows(2).all(|w| w[1] == w[0] + 1));
    }

    #[test]
    fn new_deleted_renamed_and_binary_files_are_named_so() {
        let (_, p, paths) = parse("diff --git a/n.txt b/n.txt\nnew file mode 100644\n--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+hi\ndiff --git a/d.txt b/d.txt\ndeleted file mode 100644\n--- a/d.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\ndiff --git a/o.txt b/r.txt\nsimilarity index 100%\nrename from o.txt\nrename to r.txt\ndiff --git a/i.png b/i.png\nBinary files a/i.png and b/i.png differ\n");
        let mut names: Vec<(usize, String)> = p.heads.iter().filter_map(|(i, h)| match h { Head::File(n) => Some((*i, n.clone())), _ => None }).collect();
        names.sort();
        assert_eq!(names.into_iter().map(|n| n.1).collect::<Vec<_>>(), ["n.txt (new)", "d.txt (deleted)", "o.txt → r.txt", "i.png"]);
        assert_eq!((p.files, paths.len()), (4, 4));
        assert!(p.heads.values().any(|h| *h == Head::Text("Binary file, not shown".into())));
    }
}

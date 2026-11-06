//! Go to Definition, Peek Definition and Find All References without an index: each
//! query scans the (maybe unsaved) buffer in memory, then runs one on-demand `rg`.
//!
//! Conventions, as in `parse::Hit` and `App::open_file_at`: paths are relative to the
//! repo root, `line` is 1-based and `col` is the 1-based *byte* column; `symbol_at`
//! takes a 0-based byte offset (`editor::Pos::col`). rg is never signalled: a cancelled
//! query stops reading, the pipes close, rg ends at its next write (EPIPE) or when done,
//! and a background thread reaps it. At most `MAX_LIVE_RG` rg run at once per `live`
//! counter, which the caller owns (the panel passes `App::live_rg`, shared with Search).

use crate::parse::{self, Hit};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

const MAX_DEFS: usize = 200;
const MAX_REFS: usize = 1000;
const MAX_HISTORY: usize = 50;
/// Candidate lines read from one definitions rg, kept or not (a `NULL` on the kernel).
const MAX_RAW_HITS: usize = 5000;
/// Bytes of a file read for the lines around a C candidate.
const MAX_CONTEXT_BYTES: u64 = 1 << 20;
/// Lines looked back for the `enum … {` around an enumerator.
const MAX_ENUM_LINES: usize = 5000;
/// rg processes not yet reaped (running or abandoned) allowed at once; a newer search
/// or lookup waits for one to end.
pub(crate) const MAX_LIVE_RG: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    C, // and C++
    Rust,
    Python,
    Go,
    Shell,
    Js, // and TS
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A local variable or parameter in the buffer, the nearest one above the cursor.
    Local,
    /// A C `label:` in the buffer, the nearest one to the cursor.
    Label,
    Macro,
    Type,
    Function,
    /// Variable, constant, enum constant.
    Value,
    /// Prototype, `extern`: ranks after every definition.
    Decl,
    /// A plain whole-word match (references, and definitions in `Lang::Other`).
    Word,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub path: String,
    pub line: usize,
    pub col: usize,
    /// The whole line.
    pub preview: String,
    pub kind: Kind,
    /// Lower is better; results come sorted by it.
    pub score: usize,
}

pub fn lang_for(path: &str) -> Lang {
    match Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("") {
        "c" | "h" | "C" | "H" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "inl" => Lang::C,
        "rs" => Lang::Rust,
        "py" | "pyi" => Lang::Python,
        "go" => Lang::Go,
        "sh" | "bash" | "zsh" | "ksh" => Lang::Shell,
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts" => Lang::Js,
        _ => Lang::Other,
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*`: the only symbols ever handed to rg.
pub fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    s.len() <= 255
        && cs.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The identifier at byte offset `col` of `line` (an `editor::Pos::col`), and its byte
/// range. A cursor just past a word still picks it, like VS Code. Identifiers are ASCII,
/// so a UTF-8 byte of a CJK char is never part of one and every range is a char boundary.
pub fn symbol_at(line: &str, col: usize) -> Option<(String, Range<usize>)> {
    let b = line.as_bytes();
    let w = |i: usize| b.get(i).is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_');
    let at = if w(col) { col } else if col > 0 && w(col - 1) { col - 1 } else { return None };
    let start = (0..at).rev().find(|&i| !w(i)).map_or(0, |i| i + 1);
    let end = (at..b.len()).find(|&i| !w(i)).unwrap_or(b.len());
    let s = &line[start..end];
    is_ident(s).then(|| (s.to_string(), start..end))
}

fn types(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::C => &["-t", "c", "-t", "cpp"],
        Lang::Rust => &["-t", "rust"],
        Lang::Python => &["-t", "py"],
        Lang::Go => &["-t", "go"],
        Lang::Shell => &["-t", "sh"],
        Lang::Js => &["-t", "js", "-t", "ts"],
        Lang::Other => &[],
    }
}

/// rg prefilter per language (`SYM` = the escaped symbol). `classify` has the final say.
fn patterns(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::C => &[
            r"^\s*#\s*define\s+SYM\b",
            r"^\s*(?:typedef\s+|static\s+)?(?:struct|union|enum|class)\s+SYM\s*(?::[^;]*)?(?:\{.*)?$",
            r"^\s*\}(?:[\w\s]*\s)?SYM\s*;",
            r"^\s*typedef\s.*\bSYM\s*;",
            r"^\s*typedef\s.*\(\s*\*\s*SYM\s*\)",
            r"^(?:[A-Za-z_][\w\s*&:]*[\s*&:])?SYM\s*\(",
            r"^\s*SYM\s*(?:=[^=;][^;]*)?,?\s*(?:/[*/].*)?$",
            r"^[A-Za-z_][\w\s*]*[\s*]SYM\s*(?:\[[^\]]*\]\s*)*[=;,]",
            r"^\s*(?:static\s+|extern\s+)?(?:DEFINE|DECLARE)_[A-Z0-9_]+\(\s*SYM\s*[,)]",
            r"^\s*(?:static\s+|extern\s+)?(?:DEFINE|DECLARE)_PER_CPU\w*\([^;]*,\s*SYM\s*\)",
        ],
        Lang::Rust => &[r"\b(?:fn|struct|enum|trait|type|const|static|mod|union)\s+(?:mut\s+)?SYM\b", r"\bmacro_rules!\s*SYM\b"],
        Lang::Python => &[r"^\s*(?:async\s+)?(?:def|class)\s+SYM\b", r"^SYM\s*(?::[^=]*)?=[^=]"],
        Lang::Go => &[r"^func\s+(?:\([^)]*\)\s*)?SYM\s*[(\[]", r"^\s*(?:type|var|const)\s+SYM\b"],
        Lang::Shell => &[
            r"^\s*(?:function\s+)?SYM\s*\(\s*\)",
            r"^\s*function\s+SYM\b",
            r"^\s*(?:(?:export|local|readonly|declare)\s+)?SYM=",
        ],
        Lang::Js => &[
            r"\bfunction\b\s*\*?\s*SYM\b",
            r"\b(?:class|interface|enum)\s+SYM\b",
            r"\b(?:const|let|var)\s+SYM\b",
            r"\btype\s+SYM\s*[=<]",
        ],
        Lang::Other => &[r"\bSYM\b"],
    }
}

fn regex_escape(s: &str) -> String {
    s.chars().fold(String::new(), |mut o, c| {
        if c.is_ascii_punctuation() {
            o.push('\\');
        }
        o.push(c);
        o
    })
}

/// Words (`[A-Za-z0-9_]+`) and single other chars, with byte offsets; spaces dropped.
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let mut v = Vec::new();
    let mut word = None;
    for (i, c) in line.char_indices().chain([(line.len(), ' ')]) {
        let w = c.is_ascii_alphanumeric() || c == '_';
        if let (Some(s), false) = (word, w) {
            v.push((s, &line[s..i]));
            word = None;
        }
        if w {
            word.get_or_insert(i);
        } else if !c.is_whitespace() {
            v.push((i, &line[i..i + c.len_utf8()]));
        }
    }
    v
}

fn is_word(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
}

fn words_or(t: &[(usize, &str)], extra: &[&str]) -> bool {
    t.iter().all(|x| is_word(x.1) || extra.contains(&x.1))
}

fn balanced(l: &str) -> bool {
    l.matches('(').count() == l.matches(')').count()
}

/// What a C verdict still needs from the surrounding lines (see `settle`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Need {
    Nothing,
    BraceNext,
    FuncEnd,
    InEnum,
}

/// Whether `line` defines `sym`: its kind, the byte offset of `sym`, and what is left
/// to check around it. Mirrors `patterns`.
fn classify(lang: Lang, sym: &str, line: &str) -> Option<(Kind, usize, Need)> {
    use Kind::*;
    use Need::Nothing as N;
    let t = tokens(line);
    let col0 = !line.starts_with(char::is_whitespace);
    let tok = |j: usize| t.get(j).map_or("", |x| x.1);
    for i in (0..t.len()).filter(|&i| t[i].1 == sym) {
        let at = t[i].0;
        let back = |k: usize| i.checked_sub(k).map_or("", tok);
        let (p, p2, n, n2) = (back(1), back(2), tok(i + 1), tok(i + 2));
        let hit = match lang {
            Lang::C => {
                // `DEFINE_MUTEX(S)`, `DEFINE_PER_CPU(T, S)`: definitions; `DECLARE_…`: declarations.
                let m = usize::from(matches!(tok(0), "static" | "extern"));
                let (mac, per_cpu) = (tok(m), tok(m).contains("_PER_CPU"));
                let kernel = (mac.starts_with("DEFINE_") || mac.starts_with("DECLARE_")) && tok(m + 1) == "(";
                let arg = if per_cpu { p == "," && n == ")" } else { i == m + 2 && matches!(n, "," | ")") };
                if kernel && arg {
                    Some((if mac.starts_with("DEFINE_") { Value } else { Decl }, N))
                } else if i == 2 && p == "define" && p2 == "#" {
                    Some((Macro, N))
                } else if matches!(p, "struct" | "union" | "enum" | "class") {
                    // `struct S;` is a forward declaration; `struct S {` or `{` next defines it.
                    let lead = i == 1 || i == 2 && matches!(tok(0), "typedef" | "static");
                    match n {
                        "{" | ":" if lead => Some((Type, N)),
                        "" if lead => Some((Type, Need::BraceNext)),
                        _ => None,
                    }
                } else if tok(0) == "}" && n == ";" && words_or(&t[1..i], &[])
                    || tok(0) == "typedef" && (n == ";" || p == "*" && p2 == "(" && n == ")")
                {
                    Some((Type, N)) // `} S;` ends a typedef; `typedef … S;`, `typedef … (*S)(…)`
                } else if col0 && n == "(" && (i == 0 || is_word(tok(0))) && words_or(&t[..i], &["*", "&", ":"]) {
                    // A definition reaches `{` before any `;`; a prototype the other way round.
                    match line[at..].find([';', '{']).map(|k| line.as_bytes()[at + k]) {
                        Some(b'{') => Some((Function, N)),
                        Some(_) if i > 0 => Some((Decl, N)),
                        Some(_) => None, // `MACRO(x);`
                        None => Some((Function, Need::FuncEnd)),
                    }
                } else if i == 0 && (n.is_empty() || n == "," || n == "=" && n2 != "=") && !line.contains(';') && balanced(line) {
                    Some((Value, Need::InEnum))
                } else if col0 && i > 0 && matches!(n, "=" | ";" | "," | "[") && words_or(&t[..i], &["*"]) {
                    Some((if tok(0) == "extern" { Decl } else { Value }, N))
                } else {
                    None
                }
            }
            Lang::Rust => {
                if p == "!" && p2 == "macro_rules" {
                    Some((Macro, N))
                } else if p == "mut" && p2 == "static" {
                    Some((Value, N))
                } else {
                    let k = match p {
                        "fn" => Some(Function),
                        "struct" | "enum" | "trait" | "type" | "union" | "mod" => Some(Type),
                        "const" | "static" if p2 != "*" => Some(Value), // not `*const T`
                        _ => None,
                    };
                    k.map(|k| (k, N))
                }
            }
            Lang::Python => match p {
                "def" => Some((Function, N)),
                "class" => Some((Type, N)),
                _ if col0 && i == 0 && (n == "=" && n2 != "=" || n == ":" && line.contains('=')) => Some((Value, N)),
                _ => None,
            },
            Lang::Go => {
                if tok(0) == "func" && matches!(n, "(" | "[") && (i == 1 || p == ")") {
                    Some((Function, N))
                } else if i == 1 && p == "type" {
                    Some((Type, N))
                } else if i == 1 && matches!(p, "var" | "const") {
                    Some((Value, N))
                } else {
                    None
                }
            }
            Lang::Shell => {
                let lead = i == 0 || i == 1 && matches!(p, "function" | "export" | "local" | "readonly" | "declare");
                if i == 1 && p == "function" || lead && n == "(" && n2 == ")" {
                    Some((Function, N))
                } else if lead && line[at + sym.len()..].starts_with('=') {
                    Some((Value, N))
                } else {
                    None
                }
            }
            Lang::Js => match p {
                "function" => Some((Function, N)),
                "*" if p2 == "function" => Some((Function, N)),
                "class" | "interface" | "enum" => Some((Type, N)),
                "type" if matches!(n, "=" | "<") => Some((Type, N)),
                "const" | "let" | "var" => Some((Value, N)),
                _ => None,
            },
            Lang::Other => Some((Word, N)),
        };
        if let Some((k, need)) = hit {
            return Some((k, at, need));
        }
    }
    None
}

/// Finish a `classify` verdict for `lines[n]` from the lines around it.
/// Past the lines read (a file over `MAX_CONTEXT_BYTES`) the candidate is kept as is.
fn settle<S: AsRef<str>>(kind: Kind, at: usize, need: Need, lines: &[S], n: usize) -> Option<Kind> {
    let after = || lines.iter().skip(n + 1).take(20).flat_map(|l| l.as_ref().chars());
    match need {
        Need::Nothing => Some(kind),
        _ if n >= lines.len() => Some(kind),
        Need::BraceNext => (after().find(|c| !c.is_whitespace()) == Some('{')).then_some(kind),
        // A head continued on later lines: `{` first defines it, `;` first declares it.
        Need::FuncEnd => match after().find(|c| matches!(c, ';' | '{')) {
            Some(';') if at == 0 => None,
            Some(';') => Some(Kind::Decl),
            _ => Some(kind),
        },
        // `S,` / `S = 1` is an enumerator only inside `enum … {`, else likely a call argument:
        // look back to the `{`, giving up at a statement, an open call or a column-0 `}`.
        Need::InEnum => {
            let is_enum = |l: &str| tokens(l).iter().any(|t| t.1 == "enum");
            let mut up = lines[..n].iter().rev().take(MAX_ENUM_LINES).map(|l| l.as_ref());
            while let Some(l) = up.next() {
                if l.contains('{') {
                    return (is_enum(l) || l.trim() == "{" && up.next().is_some_and(is_enum)).then_some(kind);
                }
                if l.starts_with('}') || l.contains(';') || !balanced(l) {
                    return None;
                }
            }
            None
        }
    }
}

const NOT_A_TYPE: &[&str] = &[
    "return", "else", "case", "goto", "sizeof", "typeof", "struct", "union", "enum", "class", "new", "delete", "throw",
    "yield", "await", "in", "not", "and", "or", "is", "if", "while", "do", "import", "from", "as", "use", "define",
    "include", "ifdef", "ifndef", "undef", "elif", "echo", "print",
];

/// Byte offset of `sym` if `line` declares it as a local or a parameter.
// ponytail: token heuristics, no scopes; a tree-sitter parse if this misfires too often.
fn local_at(lang: Lang, sym: &str, line: &str) -> Option<usize> {
    if line.trim_start().starts_with(['*', '/', '#']) {
        return None; // comment or preprocessor
    }
    let t = tokens(line);
    let tok = |j: usize| t.get(j).map_or("", |x| x.1);
    (0..t.len()).filter(|&i| t[i].1 == sym).map(|i| (i, tok(i + 1), tok(i + 2))).find(|&(i, n, n2)| {
        // Go `x :=`, Python `x = …` at any indent, `for x in`.
        if n == ":" && n2 == "=" || lang == Lang::Python && i == 0 && n == "=" && n2 != "=" || i > 0 && tok(i - 1) == "for" && n == "in" {
            return true;
        }
        // A parameter: inside the parentheses after `fn`/`def`/`func`/`function`/`lambda`.
        if let Some(k) = t[..i].iter().position(|x| matches!(x.1, "fn" | "def" | "func" | "function" | "lambda")) {
            let depth: i32 = t[k..i].iter().map(|x| i32::from(x.1 == "(") - i32::from(x.1 == ")")).sum();
            let go = lang == Lang::Go && (is_word(n) || matches!(n, "*" | "["));
            if depth > 0 && (matches!(n, "," | ")" | "=") || n == ":" && n2 != ":" || go) {
                return true;
            }
        }
        // A typed declaration: `T x`, `T *x`, `let mut x`.
        let mut j = i;
        while j > 0 && matches!(t[j - 1].1, "*" | "&") {
            j -= 1;
        }
        let p = if j > 0 { t[j - 1].1 } else { return false };
        // `a * b`, spaced on both sides, multiplies.
        let spaced = j < i && line[..t[j].0].ends_with(' ') && line[t[i - 1].0 + 1..].starts_with(' ');
        is_word(p)
            && !NOT_A_TYPE.contains(&p)
            && !spaced
            && !(p == "mut" && j >= 2 && t[j - 2].1 == "&")
            && matches!(n, "=" | ";" | "," | ")" | "[" | ":")
            && !(n == "=" && n2 == "=")
            && !(n == ":" && n2 == ":")
    })
    .map(|(i, ..)| t[i].0)
}

/// Byte offset of `sym` if `line` is the C label `sym:`.
fn label_at(sym: &str, line: &str) -> Option<usize> {
    let rest = line.trim_start().strip_prefix(sym)?;
    (rest.starts_with(':') && !rest.starts_with("::")).then(|| line.len() - line.trim_start().len())
}

/// The first `MAX_CONTEXT_BYTES` of a file, as lines.
fn read_lines(path: &Path) -> Vec<String> {
    let mut v = Vec::new();
    let _ = File::open(path).and_then(|f| f.take(MAX_CONTEXT_BYTES).read_to_end(&mut v));
    String::from_utf8_lossy(&v).lines().map(String::from).collect()
}

fn loc(path: &str, line: usize, at: usize, text: &str, kind: Kind) -> Location {
    Location { path: path.into(), line, col: at + 1, preview: text.trim_end().into(), kind, score: 0 }
}

/// Cancel flag and live-rg counter of one query.
#[derive(Clone, Copy)]
struct Ctl<'a> {
    stop: &'a AtomicBool,
    live: &'a Arc<AtomicUsize>,
}

/// Wait for a child we gave up on, off the UI thread (no zombie, no signal).
/// Its pipes are closed first: a child blocked on a full pipe then gets EPIPE
/// and exits instead of keeping the reaper waiting forever. `live` counts it out.
pub(crate) fn reap(mut c: Child, live: Option<Arc<AtomicUsize>>) -> std::thread::JoinHandle<()> {
    drop(c.stdin.take());
    drop(c.stdout.take());
    drop(c.stderr.take());
    std::thread::spawn(move || {
        let _ = c.wait();
        if let Some(n) = live {
            n.fetch_sub(1, Ordering::SeqCst);
        }
    })
}

/// Run rg in `root` on `args` and hand each hit to `f` until it returns false, rg is
/// done or `stop` is set; then rg is reaped in the background. Waits first while
/// `live` is at `MAX_LIVE_RG`, and gives up if `stop` is set meanwhile.
fn rg(root: &Path, args: &[&str], Ctl { stop, live }: Ctl, mut f: impl FnMut(Hit) -> bool) -> Result<(), String> {
    while live.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < MAX_LIVE_RG).then_some(n + 1)).is_err() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if stop.load(Ordering::Relaxed) {
        live.fetch_sub(1, Ordering::SeqCst);
        return Ok(());
    }
    let c = Command::new("rg")
        .args(["--no-config", "--null", "--line-number", "--column", "--no-heading", "--color", "never"])
        .args(["--max-columns", "300", "--max-columns-preview"])
        .args(args)
        .args(["--", "."])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut c = match c {
        Ok(c) => c,
        Err(e) => {
            live.fetch_sub(1, Ordering::SeqCst);
            return Err(format!("rg: {e}"));
        }
    };
    if let Some(out) = c.stdout.take() {
        for line in BufReader::new(out).split(b'\n').map_while(Result::ok) {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Some(mut h) = parse::parse_rg_line(&String::from_utf8_lossy(&line)) else { continue };
            if let Some(p) = h.path.strip_prefix("./") {
                h.path = p.to_string();
            }
            if !f(h) {
                break;
            }
        }
    } // the reader is dropped here: rg gets EPIPE at its next write
    reap(c, Some(live.clone()));
    Ok(())
}

/// Definitions of `symbol`, best first. `from_file` (root-relative) is the file being
/// edited and `buffer` its current, maybe unsaved, text: the buffer replaces that file's
/// copy on disk. A local or parameter declared at or above `from_line` (1-based) is
/// returned alone, as is a C file's own `static` or `#define`, without running rg. So is
/// a C `label:`, nearest to the cursor: first when the cursor line is its `goto`.
/// Blocking; the panel uses `spawn`.
#[cfg(test)]
pub fn definitions(root: &Path, symbol: &str, lang: Lang, from_file: &str, from_line: usize, buffer: &str, live: &Arc<AtomicUsize>) -> Result<Vec<Location>, String> {
    defs_with(root, symbol, lang, (from_file, from_line, buffer), Ctl { stop: &AtomicBool::new(false), live })
}

fn defs_with(root: &Path, sym: &str, lang: Lang, (from_file, from_line, buffer): (&str, usize, &str), ctl: Ctl) -> Result<Vec<Location>, String> {
    if !is_ident(sym) {
        return Err(format!("not an identifier: {sym:?}"));
    }
    let lines: Vec<&str> = buffer.lines().collect();
    let (mut out, mut local, mut label) = (Vec::new(), None, None::<Location>);
    for (n, line) in lines.iter().enumerate().filter(|(_, l)| l.contains(sym)) {
        if lang == Lang::C {
            if let Some(at) = label_at(sym, line) {
                if label.as_ref().is_none_or(|l| (n + 1).abs_diff(from_line) < l.line.abs_diff(from_line)) {
                    label = Some(loc(from_file, n + 1, at, line, Kind::Label));
                }
                continue;
            }
        }
        match classify(lang, sym, line).and_then(|(k, at, need)| Some((settle(k, at, need, &lines, n)?, at))) {
            Some((kind, at)) => out.push(loc(from_file, n + 1, at, line, kind)),
            None if n < from_line && lang != Lang::Other => {
                local = local_at(lang, sym, line).map(|at| loc(from_file, n + 1, at, line, Kind::Local)).or(local);
            }
            None => {}
        }
    }
    let goto_here = from_line.checked_sub(1).and_then(|i| lines.get(i)).is_some_and(|l| {
        let t = tokens(l);
        t.windows(2).any(|w| w[0].1 == "goto" && w[1].1 == sym)
    });
    if let Some(l) = if goto_here { label.or(local) } else { local.or(label) } {
        return Ok(vec![l]);
    }
    let own = |l: &Location| l.preview.starts_with('#') || l.preview.starts_with("static");
    if !(lang == Lang::C && out.iter().any(own)) {
        let pat = patterns(lang).iter().map(|p| format!("(?:{})", p.replace("SYM", &regex_escape(sym)))).collect::<Vec<_>>().join("|");
        let mut args = vec!["--max-count", "50"];
        args.extend(types(lang));
        args.extend(["-e", &pat]);
        let (mut file, mut found, mut raw): ((String, Vec<String>), usize, usize) = Default::default();
        rg(root, &args, ctl, |h| {
            raw += 1;
            if h.path == from_file {
                return raw < MAX_RAW_HITS;
            }
            if let Some((kind, at, need)) = classify(lang, sym, &h.text) {
                // rg prints a file's hits together: one bounded read per file.
                if need != Need::Nothing && file.0 != h.path {
                    file = (h.path.clone(), read_lines(&root.join(&h.path)));
                }
                if let Some(kind) = settle(kind, at, need, &file.1, h.line.saturating_sub(1)) {
                    out.push(loc(&h.path, h.line, at, &h.text, kind));
                    found += 1;
                }
            }
            found < MAX_DEFS && raw < MAX_RAW_HITS
        })?;
    }
    if ctl.stop.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    rank(&mut out, from_file);
    out.truncate(MAX_DEFS);
    Ok(out)
}

/// Locals, then definitions, then declarations; within each: same file, same
/// directory, header/source pair (foo.c ↔ foo.h), then shallower paths.
fn rank(v: &mut Vec<Location>, from: &str) {
    let from_p = Path::new(from);
    for l in v.iter_mut() {
        let p = Path::new(&l.path);
        let near = if l.path == from {
            0
        } else if p.parent() == from_p.parent() {
            1
        } else if p.file_stem() == from_p.file_stem() && p.extension() != from_p.extension() {
            2
        } else {
            3
        };
        let tier = match l.kind {
            Kind::Local | Kind::Label => 0,
            Kind::Decl => 2,
            _ => 1,
        };
        l.score = tier * 1_000_000 + near * 100_000 + l.path.matches('/').count().min(99_999);
    }
    v.sort_by(|a, b| (a.score, &a.path, a.line).cmp(&(b.score, &b.path, b.line)));
    v.dedup_by(|a, b| a.path == b.path && a.line == b.line);
}

/// Whole-word occurrences (`rg -w -F`), sorted by path then line; the first 1000 found.
/// Blocking; the panel uses `spawn`.
#[cfg(test)]
pub fn references(root: &Path, symbol: &str, lang: Lang, live: &Arc<AtomicUsize>) -> Result<Vec<Location>, String> {
    refs_with(root, symbol, lang, Ctl { stop: &AtomicBool::new(false), live })
}

fn refs_with(root: &Path, sym: &str, lang: Lang, ctl: Ctl) -> Result<Vec<Location>, String> {
    if !is_ident(sym) {
        return Err(format!("not an identifier: {sym:?}"));
    }
    let mut args = vec!["-w", "-F"];
    args.extend(types(lang));
    args.extend(["-e", sym]);
    let mut out = Vec::new();
    rg(root, &args, ctl, |h| {
        out.push(Location { path: h.path, line: h.line, col: h.col, preview: h.text, kind: Kind::Word, score: 0 });
        out.len() < MAX_REFS
    })?;
    if ctl.stop.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    Ok(out)
}

pub enum Query {
    Definitions { from_file: String, from_line: usize, buffer: String },
    References,
}

/// A query on a worker thread. Poll `rx` from the UI loop (one message, the result);
/// `cancel` or drop it to stop: no result is sent, rg exits on the closed pipe and the
/// worker reaps it, then ends.
pub struct Pending {
    pub rx: mpsc::Receiver<Result<Vec<Location>, String>>,
    stop: Arc<AtomicBool>,
    #[cfg_attr(not(test), allow(dead_code))] // tests check it ends; the panel lets it detach
    worker: std::thread::JoinHandle<()>,
}

impl Pending {
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// `live` is the caller's rg counter: Search and nav share one, so one cap covers both.
pub fn spawn(root: PathBuf, symbol: String, lang: Lang, query: Query, live: Arc<AtomicUsize>) -> Pending {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let s = stop.clone();
    let worker = std::thread::spawn(move || {
        let ctl = Ctl { stop: &s, live: &live };
        let r = match query {
            Query::Definitions { from_file, from_line, buffer } => defs_with(&root, &symbol, lang, (&from_file, from_line, &buffer), ctl),
            Query::References => refs_with(&root, &symbol, lang, ctl),
        };
        if !s.load(Ordering::Relaxed) {
            let _ = tx.send(r);
        }
    });
    Pending { rx, stop, worker }
}

/// (path, line, col), the `Location` convention.
pub type Mark = (String, usize, usize);

/// Go Back / Go Forward, VS Code style: a new jump clears Forward; 50 entries kept.
#[derive(Debug, Default)]
pub struct NavHistory {
    back: Vec<Mark>,
    fwd: Vec<Mark>,
}

impl NavHistory {
    /// Call before jumping away from `from`.
    pub fn push(&mut self, from: Mark) {
        self.fwd.clear();
        if self.back.last() != Some(&from) {
            self.back.push(from);
        }
        if self.back.len() > MAX_HISTORY {
            self.back.remove(0);
        }
    }

    /// Where Go Back goes from `cur`, which becomes the Forward target.
    pub fn back(&mut self, cur: Mark) -> Option<Mark> {
        let m = self.back.pop()?;
        self.fwd.push(cur);
        Some(m)
    }

    pub fn forward(&mut self, cur: Mark) -> Option<Mark> {
        let m = self.fwd.pop()?;
        self.back.push(cur);
        Some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn live() -> Arc<AtomicUsize> {
        Arc::default()
    }

    fn fixture(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("herdr-code-nav-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for (p, text) in files {
            let f = d.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, text).unwrap();
        }
        d
    }

    fn top(v: &[Location]) -> Vec<(&str, usize, Kind)> {
        v.iter().map(|l| (l.path.as_str(), l.line, l.kind)).collect()
    }

    fn until(done: impl Fn() -> bool) -> bool {
        let t = Instant::now();
        while !done() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        done()
    }

    const DSA_H: &str = "#ifndef DSA_H\n#define DSA_H\nstruct dsa_port;\n#define DSA_MAX_PORTS 12\nstruct dsa_port {\n\tint index;\n};\n\
        typedef struct {\n\tint a;\n} dsa_cfg_t;\nstruct brace_next\n{\n\tint b;\n};\nenum dsa_tag {\n\tDSA_TAG_NONE = 0,\n\tDSA_TAG_MXL,\n};\n\
        int dsa_port_change_master(struct dsa_port *dp,\n\t\t\t   int x);\n#endif\n";
    const PORT_C: &str = "#include \"dsa.h\"\n\nint\ndsa_port_change_master(struct dsa_port *dp,\n\t\t       int x)\n{\n\
        \treturn dsa_call(x,\n\t\t\tDSA_TAG_MXL,\n\t\t\t0);\n}\n";
    const USER_C: &str = "#include \"dsa.h\"\nstatic int helper(struct dsa_port *dp)\n{\n\treturn dsa_port_change_master(dp, DSA_MAX_PORTS);\n}\n";

    fn c_tree(tag: &str) -> PathBuf {
        fixture(tag, &[("c/dsa.h", DSA_H), ("c/port.c", PORT_C), ("c/user.c", USER_C)])
    }

    #[test]
    fn symbol_at_takes_byte_offsets_with_cjk_and_tabs() {
        // Bytes: tab 0, 中 1..4, 文 4..7, space 7, foo_bar 8..15, ( 15, x0 16..18, ) 18.
        let l = "\t中文 foo_bar(x0)";
        assert_eq!(symbol_at(l, 10), Some(("foo_bar".into(), 8..15)));
        assert_eq!(symbol_at(l, 8), Some(("foo_bar".into(), 8..15)));
        assert_eq!(symbol_at(l, 15), Some(("foo_bar".into(), 8..15)), "just past the word");
        assert_eq!(symbol_at(l, 16), Some(("x0".into(), 16..18)));
        assert_eq!(symbol_at(l, 1), None, "CJK is not an identifier");
        assert_eq!(symbol_at(l, 2), None, "inside a CJK char");
        assert_eq!(symbol_at(l, 0), None, "tab");
        assert_eq!(symbol_at(l, 99), None, "past the line");
        assert_eq!(symbol_at("a = 0x1f;", 5), None, "number");
        assert_eq!(symbol_at("", 0), None);
        assert_eq!((lang_for("a/b.c"), lang_for("x.hpp"), lang_for("m.rs")), (Lang::C, Lang::C, Lang::Rust));
        assert_eq!((lang_for("p.py"), lang_for("g.go"), lang_for("s.sh"), lang_for("t.tsx")), (Lang::Python, Lang::Go, Lang::Shell, Lang::Js));
        assert_eq!(lang_for("Makefile"), Lang::Other);
    }

    #[test]
    fn non_identifiers_are_rejected_before_rg() {
        let root = std::env::temp_dir();
        for bad in [".*", "", "a b", "9x", "a|b", "foo$", "x;rm", "名前"] {
            assert!(definitions(&root, bad, Lang::C, "", 0, "", &live()).is_err(), "{bad:?}");
            assert!(references(&root, bad, Lang::Other, &live()).is_err(), "{bad:?}");
        }
        assert_eq!(regex_escape("a.b*"), r"a\.b\*");
    }

    #[test]
    fn c_definitions() {
        let root = c_tree("c");
        let d = |s: &str| definitions(&root, s, Lang::C, "c/user.c", 4, USER_C, &live()).unwrap();
        // Forward declaration (line 3) excluded.
        assert_eq!(top(&d("dsa_port")), [("c/dsa.h", 5, Kind::Type)]);
        assert_eq!(top(&d("dsa_cfg_t")), [("c/dsa.h", 10, Kind::Type)]);
        assert_eq!(top(&d("brace_next")), [("c/dsa.h", 11, Kind::Type)]);
        assert_eq!(top(&d("DSA_MAX_PORTS")), [("c/dsa.h", 4, Kind::Macro)]);
        // The call argument `DSA_TAG_MXL,` in port.c is not an enumerator.
        assert_eq!(top(&d("DSA_TAG_MXL")), [("c/dsa.h", 17, Kind::Value)]);
        // Kernel-style two-line definition first, the multi-line prototype after it.
        let v = d("dsa_port_change_master");
        assert_eq!(top(&v), [("c/port.c", 4, Kind::Function), ("c/dsa.h", 19, Kind::Decl)]);
        assert_eq!(v[0].col, 1);
        assert!(v[0].score < v[1].score);
        // A parameter above the cursor wins outright.
        assert_eq!(top(&d("dp")), [("c/user.c", 2, Kind::Local)]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn kernel_macros_labels_and_long_enums() {
        let big = format!("enum big {{\n{}\tLATE_ONE,\n}};\n", (0..300).map(|i| format!("\tE_{i},\n")).collect::<String>());
        let root = fixture(
            "kmac",
            &[
                ("k/lock.c", "static DEFINE_MUTEX(foo_lock);\nDEFINE_SPINLOCK(bar_lock);\nDEFINE_PER_CPU(int, pcpu_cnt);\n"),
                ("k/lock.h", "DECLARE_PER_CPU(int, pcpu_cnt);\nextern struct mutex foo_lock;\n"),
                ("k/big.h", &big),
            ],
        );
        let d = |s: &str| top(&definitions(&root, s, Lang::C, "k/user.c", 1, "", &live()).unwrap()).into_iter().map(|(p, n, k)| (p.to_string(), n, k)).collect::<Vec<_>>();
        let v = |x: &[(&str, usize, Kind)]| x.iter().map(|&(p, n, k)| (p.to_string(), n, k)).collect::<Vec<_>>();
        assert_eq!(d("foo_lock"), v(&[("k/lock.c", 1, Kind::Value), ("k/lock.h", 2, Kind::Decl)]));
        assert_eq!(d("bar_lock"), v(&[("k/lock.c", 2, Kind::Value)]));
        assert_eq!(d("pcpu_cnt"), v(&[("k/lock.c", 3, Kind::Value), ("k/lock.h", 1, Kind::Decl)]));
        assert_eq!(d("LATE_ONE"), v(&[("k/big.h", 302, Kind::Value)]), "301 lines below its enum line");
        // A label and a local of the same name: the label on its `goto` line, the local elsewhere.
        let buf = "int f(int x)\n{\n\tint out = 0;\n\tif (x)\n\t\tgoto out;\n\treturn 0;\nout:\n\treturn out;\n}\n";
        let at = |line: usize| top(&definitions(&root, "out", Lang::C, "k/f.c", line, buf, &live()).unwrap()).into_iter().map(|(_, n, k)| (n, k)).collect::<Vec<_>>();
        assert_eq!(at(5), [(7, Kind::Label)]);
        assert_eq!(at(8), [(3, Kind::Local)]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn context_reads_are_capped_and_unread_context_keeps_the_candidate() {
        let root = fixture("ctx", &[("huge.c", &"abcdefghi\n".repeat(200_000))]);
        let n = read_lines(&root.join("huge.c")).len();
        assert!((100_000..110_000).contains(&n), "{n} lines from a 2 MB file");
        assert_eq!(settle(Kind::Function, 1, Need::FuncEnd, &[] as &[&str], 5), Some(Kind::Function));
        assert_eq!(settle(Kind::Value, 0, Need::InEnum, &["x(a,", "\tFOO,"], 1), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn other_language_definitions() {
        let rs = "pub fn rust_fn(x: u32) -> u32 {\n    x\n}\npub struct RustStruct {\n    a: u8,\n}\nmacro_rules! rust_macro {\n    () => {};\n}\n";
        let py = "class PyClass:\n    def py_method(self, arg):\n        return arg\n";
        let root = fixture(
            "langs",
            &[
                ("rs/lib.rs", rs),
                ("rs/use.rs", "fn main() { rust_fn(1); rust_macro!(); let s: RustStruct; }\n"),
                ("py/mod.py", py),
                ("go/main.go", "package main\n\nfunc (s *Server) Serve(addr string) error {\n\treturn nil\n}\n"),
                ("sh/run.sh", "do_build() {\n  :\n}\nfunction do_test {\n  do_build\n}\n"),
            ],
        );
        let d = |s: &str, l: Lang| top(&definitions(&root, s, l, "", 0, "", &live()).unwrap()).into_iter().map(|(p, n, k)| (p.to_string(), n, k)).collect::<Vec<_>>();
        let one = |p: &str, n: usize, k: Kind| vec![(p.to_string(), n, k)];
        assert_eq!(d("rust_fn", Lang::Rust), one("rs/lib.rs", 1, Kind::Function));
        assert_eq!(d("RustStruct", Lang::Rust), one("rs/lib.rs", 4, Kind::Type));
        assert_eq!(d("rust_macro", Lang::Rust), one("rs/lib.rs", 7, Kind::Macro));
        assert_eq!(d("PyClass", Lang::Python), one("py/mod.py", 1, Kind::Type));
        assert_eq!(d("py_method", Lang::Python), one("py/mod.py", 2, Kind::Function));
        assert_eq!(d("Serve", Lang::Go), one("go/main.go", 3, Kind::Function));
        assert_eq!(d("do_build", Lang::Shell), one("sh/run.sh", 1, Kind::Function));
        assert_eq!(d("do_test", Lang::Shell), one("sh/run.sh", 4, Kind::Function));
        // Parameters from the buffer.
        let local = |s: &str, l: Lang, f: &str, line: usize, buf: &str| top(&definitions(&root, s, l, f, line, buf, &live()).unwrap())[0].1;
        assert_eq!(local("x", Lang::Rust, "rs/lib.rs", 2, rs), 1);
        assert_eq!(local("arg", Lang::Python, "py/mod.py", 3, py), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn ranking_buffer_dir_pair_shallow_then_declarations() {
        let root = fixture(
            "rank",
            &[
                ("r/a/foo.c", "int rank_me = 0;\n"), // stale on disk: the buffer wins
                ("r/a/other.c", "int rank_me = 2;\n"),
                ("r/a/decl.h", "extern int rank_me;\n"),
                ("r/b/foo.h", "int rank_me = 3;\n"),
                ("r/c/d/e.c", "int rank_me = 5;\n"),
                ("r/z.c", "int rank_me = 4;\n"),
            ],
        );
        let v = definitions(&root, "rank_me", Lang::C, "r/a/foo.c", 1, "\n\nint rank_me = 1;\n", &live()).unwrap();
        let paths: Vec<_> = v.iter().map(|l| (l.path.as_str(), l.line)).collect();
        assert_eq!(paths, [("r/a/foo.c", 3), ("r/a/other.c", 1), ("r/b/foo.h", 1), ("r/z.c", 1), ("r/c/d/e.c", 1), ("r/a/decl.h", 1)]);
        assert_eq!(v.last().unwrap().kind, Kind::Decl);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn references_are_whole_words_sorted_and_capped() {
        let root = c_tree("refs");
        let v = references(&root, "dsa_port", Lang::C, &live()).unwrap();
        let want = [("c/dsa.h", 3, Kind::Word), ("c/dsa.h", 5, Kind::Word), ("c/dsa.h", 19, Kind::Word), ("c/port.c", 4, Kind::Word), ("c/user.c", 2, Kind::Word)];
        assert_eq!(top(&v), want);
        fs::remove_dir_all(&root).unwrap();
        // ~150 KiB of rg output, over a pipe buffer: rg must end on the closed pipe.
        let root = fixture("cap", &[("big.c", &"cap_word x;\n".repeat(5000))]);
        let v = references(&root, "cap_word", Lang::C, &live()).unwrap();
        assert_eq!(v.len(), MAX_REFS);
        assert!(v.windows(2).all(|w| w[0].line < w[1].line));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cancel_closes_the_pipe_reaps_rg_and_ends_the_worker() {
        let root = fixture("cancel", &[("big.c", &"cap_word x;\n".repeat(5000))]);
        // Stop after the first hit while rg still has ~150 KiB to write: the reaper's
        // wait() returns (and counts it out) only because the pipe was closed.
        let (stop, count, mut n) = (AtomicBool::new(false), live(), 0);
        rg(&root, &["-F", "-e", "cap_word"], Ctl { stop: &stop, live: &count }, |_| {
            n += 1;
            stop.store(true, Ordering::Relaxed);
            true
        })
        .unwrap();
        assert_eq!(n, 1);
        assert!(until(|| count.load(Ordering::SeqCst) == 0), "rg was not reaped after the reader stopped");
        // Background query: cancelled sends nothing and its worker ends.
        let p = spawn(root.clone(), "cap_word".into(), Lang::C, Query::References, live());
        p.cancel();
        assert!(until(|| p.worker.is_finished()), "worker thread leaked");
        assert!(p.rx.try_recv().is_err());
        // Not cancelled: the ranked result arrives.
        let p = spawn(root.clone(), "cap_word".into(), Lang::C, Query::References, live());
        assert_eq!(p.rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap().len(), MAX_REFS);
        assert!(until(|| p.worker.is_finished()));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn queries_wait_for_a_free_rg_slot() {
        let root = fixture("queue", &[("a.c", "q_word\n")]);
        let live = Arc::new(AtomicUsize::new(MAX_LIVE_RG)); // two abandoned rg still running
        let run = |stop: Arc<AtomicBool>| {
            let (r, l) = (root.clone(), live.clone());
            std::thread::spawn(move || {
                let mut n = 0;
                rg(&r, &["-F", "-e", "q_word"], Ctl { stop: &stop, live: &l }, |_| {
                    n += 1;
                    true
                })
                .unwrap();
                n
            })
        };
        let h = run(Arc::new(AtomicBool::new(false)));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!h.is_finished(), "rg started without a free slot");
        live.fetch_sub(1, Ordering::SeqCst); // one of them ended
        assert!(until(|| h.is_finished()));
        assert_eq!(h.join().unwrap(), 1);
        assert!(until(|| live.load(Ordering::SeqCst) == MAX_LIVE_RG - 1), "finished rg counted out");
        // Cancelled while waiting: gives up without starting rg or taking a slot.
        live.fetch_add(1, Ordering::SeqCst);
        let stop = Arc::new(AtomicBool::new(false));
        let h = run(stop.clone());
        std::thread::sleep(Duration::from_millis(50));
        stop.store(true, Ordering::Relaxed);
        assert!(until(|| h.is_finished()));
        assert_eq!((h.join().unwrap(), live.load(Ordering::SeqCst)), (0, MAX_LIVE_RG));
        fs::remove_dir_all(&root).unwrap();
    }

    /// nav.rs never signals a process: kill, killpg, raise, sigqueue and pthread_kill
    /// appear nowhere in its code (comments aside), as identifiers, methods or commands.
    #[test]
    fn nav_sends_no_signals() {
        let text = include_str!("nav.rs");
        let forbidden = [["ki", "ll"].concat(), ["ki", "llpg"].concat(), ["rai", "se"].concat(), ["sig", "queue"].concat(), ["pthread_k", "ill"].concat()];
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("").as_bytes();
            for f in &forbidden {
                let hit = (0..code.len().saturating_sub(f.len() - 1)).any(|i| {
                    code[i..].starts_with(f.as_bytes()) && (i == 0 || !word(code[i - 1])) && code.get(i + f.len()).is_none_or(|b| !word(*b))
                });
                assert!(!hit, "nav.rs:{}: {f:?} in {line:?}", n + 1);
            }
        }
    }

    #[test]
    fn history_back_forward_like_vscode() {
        let m = |l: usize| ("f.c".to_string(), l, 1);
        let mut h = NavHistory::default();
        h.push(m(1)); // jump 1 -> 2
        h.push(m(2)); // jump 2 -> 3
        assert_eq!(h.back(m(3)), Some(m(2)));
        assert_eq!(h.back(m(2)), Some(m(1)));
        assert_eq!(h.back(m(1)), None);
        assert_eq!(h.forward(m(1)), Some(m(2)));
        h.push(m(2)); // a new jump 2 -> 9 drops Forward (3)
        assert_eq!(h.forward(m(9)), None);
        assert_eq!(h.back(m(9)), Some(m(2)));
        let mut h = NavHistory::default();
        h.push(m(5));
        h.push(m(5));
        assert_eq!(h.back.len(), 1, "repeated jumps from one spot are one entry");
        for i in 0..60 {
            h.push(m(i));
        }
        assert_eq!(h.back.len(), MAX_HISTORY);
        let mut last = None;
        while let Some(x) = h.back(m(99)) {
            last = Some(x);
        }
        assert_eq!(last, Some(m(10)), "the oldest 10 fell off");
    }

    /// `NAV_TREE=/path/to/linux cargo test kernel_timing -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a kernel tree in NAV_TREE"]
    fn kernel_timing() {
        let root = PathBuf::from(std::env::var("NAV_TREE").unwrap());
        let from = "net/dsa/slave.c";
        let buf = fs::read_to_string(root.join(from)).unwrap();
        // NULL: thousands of candidates, bounded by MAX_RAW_HITS and the 1 MB context reads.
        for sym in ["dsa_port_change_master", "dsa_port", "dsa2_mutex", "NULL"] {
            let t = Instant::now();
            let v = definitions(&root, sym, Lang::C, from, 1, &buf, &live()).unwrap();
            println!("{sym}: {:?}, {} results", t.elapsed(), v.len());
            for l in v.iter().take(3) {
                println!("  {}:{}:{} {:?} {}", l.path, l.line, l.col, l.kind, l.preview.trim());
            }
        }
    }
}

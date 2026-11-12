//! Document symbols for Go to Symbol in Editor (`@` in Quick Open) and the breadcrumbs:
//! the buffer's definition-shaped lines, matched per language family.
//! ponytail: line patterns, no parser; an LSP's documentSymbol if they misfire too often.

use crate::nav::{self, Lang};

#[derive(Clone, Debug, PartialEq)]
pub struct Symbol {
    /// 0-based line.
    pub line: usize,
    pub name: String,
    /// VS Code's symbol kind as a word: "function", "struct", "heading", …
    pub kind: &'static str,
    /// Markdown heading level (1-6); 0 for code, whose nesting comes from the folds.
    pub level: usize,
}

/// Every kind `symbols` gives, in the order of the icon set's and palette's symbol icons.
pub const KINDS: [&str; 10] = ["function", "struct", "class", "enum", "interface", "module", "type", "constant", "variable", "heading"];

/// `kind`'s place in `KINDS` (an unknown one draws as a variable).
pub fn kind_index(kind: &str) -> usize {
    KINDS.iter().position(|k| *k == kind).unwrap_or(8)
}

/// Lines a definition head may span before its `{` (or Python's `:`): rustfmt puts each
/// parameter on a line of its own.
const HEAD_LINES: usize = 24;

pub fn symbols(path: &str, lines: &[String]) -> Vec<Symbol> {
    if is_markdown(path) {
        return headings(lines);
    }
    let lang = nav::lang_for(path);
    let sym = |line, (kind, name): (&'static str, String)| Symbol { line, name, kind, level: 0 };
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            Some(sym(i, match lang {
                Lang::Rust => rust(l)?,
                Lang::C => c(l, lines, i)?,
                Lang::Python => python(l)?,
                Lang::Go => go(l)?,
                Lang::Shell => shell(l)?,
                Lang::Js => js(l)?,
                Lang::Other => return None,
            }))
        })
        .collect()
}

pub fn is_markdown(path: &str) -> bool {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx" | "mkd")
}

/// The symbols around `line`, outermost first: the headings above it by level, or the
/// definitions whose body (an indentation fold from `fold_ends`) holds it.
pub fn path_at<'a>(syms: &'a [Symbol], lines: &[String], fold_ends: &[Option<usize>], line: usize) -> Vec<&'a Symbol> {
    if syms.first().is_some_and(|s| s.level > 0) {
        let mut chain: Vec<&Symbol> = Vec::new();
        for s in syms.iter().take_while(|s| s.line <= line) {
            chain.retain(|p| p.level < s.level);
            chain.push(s);
        }
        return chain;
    }
    syms.iter().take_while(|s| s.line <= line).filter(|s| line <= extent(s.line, lines, fold_ends)).collect()
}

/// Each symbol's depth in the Outline: how many others hold it, as `path_at` counts
/// them, in one pass over the symbols (in line order).
pub fn depths(syms: &[Symbol], lines: &[String], fold_ends: &[Option<usize>]) -> Vec<usize> {
    // The holders still open: a heading's level, a definition's last line.
    let mut open: Vec<usize> = Vec::new();
    syms.iter()
        .map(|s| {
            if s.level > 0 {
                open.retain(|&l| l < s.level);
                open.push(s.level);
            } else {
                open.retain(|&end| s.line <= end);
                open.push(extent(s.line, lines, fold_ends));
            }
            open.len() - 1
        })
        .collect()
}

/// The last line of the definition at `start`: its body's fold (the first head line that
/// opens one with `{` or `:`), and the closing brace under it. A `;` first: no body.
fn extent(start: usize, lines: &[String], fold_ends: &[Option<usize>]) -> usize {
    let n = lines.len().min(fold_ends.len());
    let open = (start..n.min(start + HEAD_LINES)).find_map(|j| {
        let t = lines[j].trim_end();
        // A head has no blank line: past one is the next item.
        if t.trim_start().is_empty() {
            return Some(None);
        }
        match (t.find('{'), t.find(';')) {
            (Some(b), Some(s)) if s < b => Some(None),
            (None, Some(_)) => Some(None),
            (Some(_), _) => Some(Some(j)),
            _ => t.ends_with(':').then_some(Some(j)),
        }
    });
    let Some(Some(j)) = open else { return start };
    let end = fold_ends[j].unwrap_or(j);
    match lines.get(end + 1) {
        Some(l) if l.trim_start().starts_with('}') => end + 1,
        _ => end,
    }
}

fn headings(lines: &[String]) -> Vec<Symbol> {
    let mut fence = false;
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        let level = t.bytes().take_while(|&b| b == b'#').count();
        if fence || !(1..=6).contains(&level) || l.len() - t.len() > 3 {
            continue;
        }
        let rest = &t[level..];
        if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
            continue;
        }
        let name = rest.trim().trim_end_matches('#').trim_end();
        if !name.is_empty() {
            out.push(Symbol { line: i, name: name.to_string(), kind: "heading", level });
        }
    }
    out
}

/// The identifier `s` starts with (after blanks).
fn ident(s: &str) -> Option<&str> {
    let s = s.trim_start();
    let n = s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(s.len());
    (n > 0 && !s.as_bytes()[0].is_ascii_digit()).then(|| &s[..n])
}

/// `t` after its first word, trimmed.
fn after<'a>(t: &'a str, word: &str) -> &'a str {
    t.trim_start()[word.len()..].trim_start()
}

fn rust(l: &str) -> Option<(&'static str, String)> {
    let mut t = l.trim_start();
    loop {
        if let Some(r) = t.strip_prefix("pub(") {
            t = r.split_once(')')?.1.trim_start();
            continue;
        }
        if let Some(r) = t.strip_prefix("\"C\"") {
            t = r.trim_start();
            continue;
        }
        let w = ident(t)?;
        let rest = after(t, w);
        if matches!(w, "pub" | "async" | "unsafe" | "default" | "extern") || (w == "const" && ident(rest) == Some("fn")) {
            t = rest;
            continue;
        }
        let kind = match w {
            "fn" => "function",
            "struct" | "union" => "struct",
            "enum" => "enum",
            "trait" => "interface",
            "mod" => "module",
            "type" => "type",
            "const" => "constant",
            "static" => "variable",
            "impl" => {
                let head = t.split('{').next()?.split(" where").next()?.trim();
                return Some(("class", head.to_string()));
            }
            "macro_rules" => return Some(("function", ident(rest.strip_prefix('!')?)?.to_string())),
            _ => return None,
        };
        return Some((kind, ident(rest)?.to_string()));
    }
}

/// Words that can come before `(` without making a function head.
const C_NOT_FUNCTIONS: &[&str] = &["if", "for", "while", "switch", "return", "sizeof", "do", "else", "case", "typeof", "defined", "__attribute__", "__typeof__", "alignof", "_Alignof", "offsetof"];

fn c(l: &str, lines: &[String], i: usize) -> Option<(&'static str, String)> {
    if let Some(r) = l.strip_prefix("#define") {
        return r.starts_with([' ', '\t']).then(|| ident(r)).flatten().map(|n| ("constant", n.to_string()));
    }
    // Definitions start at column 0 (C++ methods inside a class body are not found).
    if l.starts_with(|c: char| c.is_whitespace() || c == '#' || c == '/' || c == '*' || c == '}') {
        return None;
    }
    if let Some(open) = l.find('(') {
        let head = &l[..open];
        if head.contains('=') {
            return None;
        }
        let name = head.trim_end().rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '~')).next()?;
        let short = name.rsplit("::").next().unwrap_or(name);
        if short.is_empty() || C_NOT_FUNCTIONS.contains(&short) || ident(short) != Some(short) || !opens_body(lines, i, open) {
            return None;
        }
        return Some(("function", name.to_string()));
    }
    let words: Vec<&str> = l.split_whitespace().collect();
    let k = words.iter().position(|w| matches!(*w, "struct" | "enum" | "union" | "class" | "namespace"))?;
    let name = ident(words.get(k + 1)?)?;
    let braced = l.trim_end().ends_with('{') || lines.get(i + 1).is_some_and(|n| n.trim_start().starts_with('{'));
    braced.then(|| (if words[k] == "enum" { "enum" } else { "struct" }, name.to_string()))
}

/// A `{` comes before any `;` from byte `from` of line `i` on: a definition, not a call or
/// a prototype.
fn opens_body(lines: &[String], i: usize, from: usize) -> bool {
    for (k, l) in lines.iter().enumerate().skip(i).take(HEAD_LINES) {
        let s = if k == i { &l[from..] } else { l.as_str() };
        if let Some(c) = s.chars().find(|&c| c == '{' || c == ';') {
            return c == '{';
        }
    }
    false
}

fn python(l: &str) -> Option<(&'static str, String)> {
    let t = l.trim_start();
    let t = t.strip_prefix("async ").map_or(t, str::trim_start);
    let w = ident(t)?;
    let kind = match w {
        "def" => "function",
        "class" => "class",
        _ => return None,
    };
    Some((kind, ident(after(t, w))?.to_string()))
}

fn go(l: &str) -> Option<(&'static str, String)> {
    if let Some(r) = l.strip_prefix("func ") {
        let r = r.trim_start();
        // A method: `func (r *T) Name(`.
        let r = if r.starts_with('(') { r.split_once(')')?.1 } else { r };
        return Some(("function", ident(r)?.to_string()));
    }
    let r = l.strip_prefix("type ")?;
    let name = ident(r)?;
    let kind = if after(r, name).starts_with("interface") { "interface" } else { "struct" };
    Some((kind, name.to_string()))
}

fn shell(l: &str) -> Option<(&'static str, String)> {
    let t = l.trim_start();
    if let Some(r) = t.strip_prefix("function ") {
        return Some(("function", ident(r)?.to_string()));
    }
    let name = ident(t)?;
    after(t, name).starts_with("()").then(|| ("function", name.to_string()))
}

fn js(l: &str) -> Option<(&'static str, String)> {
    let mut t = l.trim_start();
    loop {
        let w = ident(t)?;
        let rest = after(t, w);
        let kind = match w {
            "export" | "default" | "async" | "declare" | "abstract" | "static" | "public" | "private" | "protected" => {
                t = rest;
                continue;
            }
            "function" => {
                return Some(("function", ident(rest.trim_start_matches('*'))?.to_string()));
            }
            "class" => "class",
            "interface" => "interface",
            "enum" => "enum",
            "type" => "type",
            "const" | "let" | "var" => {
                let name = ident(rest)?;
                let value = after(rest, name).strip_prefix('=')?.trim_start();
                let callable = value.starts_with("function") || value.starts_with("async") || value.starts_with('(') && value.contains("=>");
                return callable.then(|| ("function", name.to_string()));
            }
            _ => return None,
        };
        return Some((kind, ident(rest)?.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    fn names(path: &str, text: &str) -> Vec<(usize, String, &'static str)> {
        symbols(path, &lines(text)).into_iter().map(|s| (s.line, s.name, s.kind)).collect()
    }

    #[test]
    fn rust_items_with_visibility_and_qualifiers() {
        let src = "pub(crate) struct A;\nimpl<T> Show for A where T: X {\n    pub async fn run(&self) {}\n    const fn k() {}\n}\n// fn not_this\nconst MAX: usize = 3;\nmacro_rules! m { () => {} }\nlet x = 1;\n";
        assert_eq!(
            names("a.rs", src),
            vec![
                (0, "A".into(), "struct"),
                (1, "impl<T> Show for A".into(), "class"),
                (2, "run".into(), "function"),
                (3, "k".into(), "function"),
                (6, "MAX".into(), "constant"),
                (7, "m".into(), "function"),
            ]
        );
    }

    #[test]
    fn c_definitions_not_calls_prototypes_or_statements() {
        let src = "#define MAX 3\nstatic int foo(int a,\n\t\tint b)\n{\n\tif (a) return bar(b);\n}\nint proto(void);\nEXPORT_SYMBOL(foo);\nstruct port {\n\tint x;\n};\nstatic int x = f(1);\nvoid Cls::method() {}\n";
        assert_eq!(
            names("a.c", src),
            vec![(0, "MAX".into(), "constant"), (1, "foo".into(), "function"), (8, "port".into(), "struct"), (12, "Cls::method".into(), "function")]
        );
    }

    #[test]
    fn python_go_shell_and_js() {
        assert_eq!(names("a.py", "class A:\n    async def f(self):\n        pass\n"), vec![(0, "A".into(), "class"), (1, "f".into(), "function")]);
        assert_eq!(names("a.go", "func (r *T) Do() {\n}\ntype S struct {\n"), vec![(0, "Do".into(), "function"), (2, "S".into(), "struct")]);
        assert_eq!(names("a.sh", "build() {\nfunction clean {\n"), vec![(0, "build".into(), "function"), (1, "clean".into(), "function")]);
        assert_eq!(
            names("a.ts", "export default async function go() {}\nconst f = (a) => a;\nconst n = 3;\nexport interface I {}\n"),
            vec![(0, "go".into(), "function"), (1, "f".into(), "function"), (3, "I".into(), "interface")]
        );
    }

    #[test]
    fn markdown_headings_skip_fences_and_nest_by_level() {
        let src = "# Top\ntext\n```sh\n# not a heading\n```\n## Sub ##\n#nospace\n### Deep\nbody\n## Next\n";
        let l = lines(src);
        let s = symbols("README.md", &l);
        assert_eq!(s.iter().map(|s| (s.line, s.name.as_str(), s.level)).collect::<Vec<_>>(), vec![(0, "Top", 1), (5, "Sub", 2), (7, "Deep", 3), (9, "Next", 2)]);
        let path = |line| path_at(&s, &l, &[], line).iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(path(8), ["Top", "Sub", "Deep"]);
        assert_eq!(path(9), ["Top", "Next"]);
    }

    #[test]
    fn a_signature_of_one_parameter_a_line_still_holds_its_body() {
        let params: String = (0..10).map(|i| format!("    p{i}: u8,\n")).collect();
        let src = format!("fn many(\n{params}) -> u8 {{\n    body();\n}}\ntype X = u8;\n\nfn next() {{}}\n");
        let l = lines(&src);
        let mut v = crate::editor::View::default();
        crate::editor::Buffer::from_text(&src, "Rust", "test").clamp_view(&mut v);
        let s = symbols("a.rs", &l);
        let path = |line| path_at(&s, &l, &v.fold_ends, line).iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(path(12), ["many"], "the body, 11 lines below the name");
        assert_eq!(path(16), ["next"]);
    }

    #[test]
    fn every_kind_given_has_its_own_icon() {
        let src = "fn f() {}\nstruct S {}\nimpl S {}\nenum E {}\ntrait T {}\nmod m {}\ntype X = u8;\nconst C: u8 = 1;\nstatic V: u8 = 1;\n";
        let kinds: Vec<&str> = symbols("a.rs", &lines(src)).iter().map(|s| s.kind).chain(symbols("a.md", &lines("# h\n")).iter().map(|s| s.kind)).collect();
        assert_eq!(kinds, KINDS);
    }

    #[test]
    fn outline_depths_nest_as_the_path_does() {
        let src = "struct S;\nimpl A {\n    fn f() {\n        x();\n    }\n    fn h() {}\n}\nfn g() {}\n";
        let l = lines(src);
        let mut v = crate::editor::View::default();
        crate::editor::Buffer::from_text(src, "Rust", "test").clamp_view(&mut v);
        let s = symbols("a.rs", &l);
        let names: Vec<(&str, usize)> = s.iter().map(|s| s.name.as_str()).zip(depths(&s, &l, &v.fold_ends)).collect();
        assert_eq!(names, [("S", 0), ("impl A", 0), ("f", 1), ("h", 1), ("g", 0)]);
        for (i, sym) in s.iter().enumerate() {
            assert_eq!(depths(&s, &l, &v.fold_ends)[i] + 1, path_at(&s, &l, &v.fold_ends, sym.line).len(), "{}", sym.name);
        }
        let md = lines("# Top\n## Sub\n### Deep\n## Next\n# Two\n");
        let h = symbols("README.md", &md);
        assert_eq!(depths(&h, &md, &[]), [0, 1, 2, 1, 0]);
    }

    #[test]
    fn the_path_holds_every_definition_whose_body_holds_the_line() {
        let src = "struct S;\nimpl A {\n    fn f() {\n        x();\n    }\n}\nfn g() {}\n";
        let l = lines(src);
        let mut v = crate::editor::View::default();
        let b = crate::editor::Buffer::from_text(src, "Rust", "test");
        b.clamp_view(&mut v);
        let s = symbols("a.rs", &l);
        let path = |line| path_at(&s, &l, &v.fold_ends, line).iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(path(0), ["S"], "a unit struct has no body to look for");
        assert_eq!(path(3), ["impl A", "f"]);
        assert_eq!(path(4), ["impl A", "f"], "the closing brace belongs to it");
        assert_eq!(path(5), ["impl A"]);
        assert_eq!(path(6), ["g"]);
    }
}

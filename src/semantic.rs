//! Two fixes on top of syntect's Sublime grammars, so a VS Code theme colours
//! code the way VS Code does:
//!
//! - `alias`: rename Sublime scopes to the VS Code TextMate names theme rules
//!   target (`keyword.control.import.include.c` -> `keyword.control.directive...`).
//! - `refine`: a per-line approximation of cpptools' semantic tokens for C/C++
//!   (and, more loosely, rust-analyzer's for Rust): calls, macros, types,
//!   properties, parameters and variables, each coloured through its cpptools
//!   scope. The only state carried between lines is the current function's
//!   parameter names, cached with each line's parser state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;
use syntect::highlighting::Style;
use syntect::parsing::{Scope, ScopeStack, ScopeStackOp};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sem {
    Function,
    Macro,
    Type,
    Parameter,
    Property,
    Variable,
    Label,
    Namespace,
    EnumMember,
    Constant,
}

/// Index order of `Syntax::sem`.
pub const SEMS: [Sem; 10] = [
    Sem::Function,
    Sem::Macro,
    Sem::Type,
    Sem::Parameter,
    Sem::Property,
    Sem::Variable,
    Sem::Label,
    Sem::Namespace,
    Sem::EnumMember,
    Sem::Constant,
];

impl Sem {
    /// cpptools' `semanticTokenScopes` (what theme rules see).
    pub fn scope(self) -> &'static str {
        match self {
            Sem::Function => "entity.name.function",
            Sem::Macro => "entity.name.function.preprocessor",
            Sem::Type => "entity.name.type",
            Sem::Parameter => "variable.parameter",
            Sem::Property => "variable.other.property",
            Sem::Variable => "variable.other.local",
            Sem::Label => "entity.name.label",
            Sem::Namespace => "entity.name.namespace",
            Sem::EnumMember => "variable.other.enummember",
            Sem::Constant => "variable.other.constant",
        }
    }

    /// VS Code semantic token type (key of a theme's `semanticTokenColors`).
    pub fn token(self) -> &'static str {
        match self {
            Sem::Function => "function",
            Sem::Macro => "macro",
            Sem::Type => "type",
            Sem::Parameter => "parameter",
            Sem::Property => "property",
            Sem::Variable => "variable",
            Sem::Label => "label",
            Sem::Namespace => "namespace",
            Sem::EnumMember => "enumMember",
            Sem::Constant => "variable.readonly",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    C,
    Rust,
    Other,
}

pub fn lang(syntax_name: &str) -> Lang {
    match syntax_name {
        "C" | "C++" | "Objective-C" | "Objective-C++" => Lang::C,
        "Rust" => Lang::Rust,
        _ => Lang::Other,
    }
}

/// Carried from one line to the next (cached per line start).
#[derive(Clone, Default, Debug)]
pub struct Ctx {
    params: Rc<Vec<String>>,
    /// Inside a parameter list that started on an earlier line.
    collecting: bool,
}

/// (Sublime prefix, VS Code replacement, C family only).
const ALIASES: &[(&str, &str, bool)] = &[
    ("keyword.control.import", "keyword.control.directive", true),
    ("keyword.operator.word", "keyword.operator.wordlike", true),
    ("entity.name.constant.preprocessor", "entity.name.function.preprocessor", false),
    ("entity.name.struct", "entity.name.type.struct", false),
    ("entity.name.union", "entity.name.type.union", false),
    ("entity.name.enum", "entity.name.type.enum", false),
    ("entity.name.trait", "entity.name.type.trait", false),
    ("entity.name.interface", "entity.name.type.interface", false),
    ("variable.function", "entity.name.function.call", false),
    ("variable.other.member", "variable.other.property", false),
];

/// The VS Code name for a Sublime scope (cached).
pub fn alias(s: Scope) -> Scope {
    thread_local! {
        static CACHE: RefCell<HashMap<Scope, Scope>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        *c.borrow_mut().entry(s).or_insert_with(|| {
            let name = s.build_string();
            let c_family = matches!(name.rsplit('.').next(), Some("c" | "c++" | "objc" | "objc++"));
            for (from, to, c_only) in ALIASES {
                let hit = name == *from || name.starts_with(&format!("{from}."));
                if hit && (c_family || !c_only) {
                    return Scope::new(&format!("{to}{}", &name[from.len()..])).unwrap_or(s);
                }
            }
            s
        })
    })
}

pub fn alias_ops(ops: &mut [(usize, ScopeStackOp)]) {
    for (_, op) in ops {
        if let ScopeStackOp::Push(s) = op {
            *s = alias(*s);
        }
    }
}

struct Scopes {
    meta: Scope,
    source: Scope,
    params: Scope,
    macro_params: Scope,
    function: Scope,
    macro_body: Scope,
    block: Scope,
    record: [Scope; 3],
    enum_: Scope,
    parameter: Scope,
    call: Scope,
}

fn scopes() -> &'static Scopes {
    static S: OnceLock<Scopes> = OnceLock::new();
    S.get_or_init(|| {
        let s = |n: &str| Scope::new(n).expect("valid scope");
        Scopes {
            meta: s("meta"),
            source: s("source"),
            params: s("meta.function.parameters"),
            macro_params: s("meta.preprocessor.macro.parameters"),
            function: s("meta.function"),
            macro_body: s("meta.preprocessor.macro"),
            block: s("meta.block"),
            record: [s("meta.struct"), s("meta.union"), s("meta.class")],
            enum_: s("meta.enum"),
            parameter: s("variable.parameter"),
            call: s("entity.name.function.call"),
        }
    })
}

fn has(stack: &[Scope], s: Scope) -> bool {
    stack.iter().any(|x| s.is_prefix_of(*x))
}

fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn all_caps(w: &str) -> bool {
    w.len() > 1 && w.bytes().any(|b| b.is_ascii_uppercase()) && !w.bytes().any(|b| b.is_ascii_lowercase())
}

/// The identifier `before` ends with, if any.
fn prev_word(before: &str) -> &str {
    let start = before.bytes().rposition(|b| !ident_byte(b)).map_or(0, |i| i + 1);
    &before[start..]
}

/// Where the cursor is, structurally, for one identifier.
struct Here<'a> {
    w: &'a str,
    before: &'a str,
    after: &'a str,
    stack: &'a [Scope],
}

fn classify_c(h: &Here, params: &[String]) -> Sem {
    let sc = scopes();
    let (w, before, after) = (h.w, h.before, h.after);
    let in_body = (has(h.stack, sc.function) && !has(h.stack, sc.params)) || has(h.stack, sc.macro_body);
    if after.starts_with('(') {
        return if all_caps(w) { Sem::Macro } else { Sem::Function };
    }
    if before.ends_with("->") || (before.ends_with('.') && !before.ends_with("..")) {
        return Sem::Property;
    }
    if all_caps(w) {
        return Sem::Macro;
    }
    if after.starts_with("::") {
        return Sem::Namespace;
    }
    if matches!(prev_word(before), "struct" | "union" | "enum" | "class") || w.ends_with("_t") {
        return Sem::Type;
    }
    if has(h.stack, sc.block) && sc.record.iter().any(|r| has(h.stack, *r)) {
        return Sem::Property;
    }
    if has(h.stack, sc.block) && has(h.stack, sc.enum_) {
        return Sem::EnumMember;
    }
    if in_body && params.iter().any(|p| p == w) {
        return Sem::Parameter;
    }
    if before.is_empty() && after.starts_with(':') && !after.starts_with("::") {
        return Sem::Label;
    }
    Sem::Variable
}

fn classify_rust(h: &Here, params: &[String]) -> Sem {
    let sc = scopes();
    let (w, before, after) = (h.w, h.before, h.after);
    let camel = w.starts_with(|c: char| c.is_ascii_uppercase()) && w.bytes().any(|b| b.is_ascii_lowercase());
    if after.starts_with('(') {
        return if camel { Sem::Type } else { Sem::Function };
    }
    if after.starts_with('!') && !after.starts_with("!=") {
        return Sem::Macro;
    }
    if before.ends_with('.') && !before.ends_with("..") {
        return Sem::Property;
    }
    if camel {
        return Sem::Type;
    }
    if after.starts_with("::") || (before.ends_with("::") && !all_caps(w)) {
        return Sem::Namespace;
    }
    if all_caps(w) {
        return Sem::Constant;
    }
    if has(h.stack, sc.function) && !has(h.stack, sc.params) && params.iter().any(|p| p == w) {
        return Sem::Parameter;
    }
    Sem::Variable
}

/// Recolour the identifiers the grammar left unclassified. `ops` are this
/// line's (aliased) ops, `stack` the scope stack at the line start, `pieces`
/// the highlighter's `(end, style)` per op-delimited token. Returns segments.
pub fn refine(
    lang: Lang,
    line: &str,
    ops: &[(usize, ScopeStackOp)],
    mut stack: ScopeStack,
    pieces: Vec<(usize, Style)>,
    ctx: &mut Ctx,
    sem: &[Style],
) -> Vec<(usize, Style)> {
    if lang == Lang::Other || sem.len() < SEMS.len() {
        return pieces;
    }
    let sc = scopes();
    let mut params: Option<Vec<String>> = None;
    let mut out = Vec::with_capacity(pieces.len() + 4);
    let (mut k, mut pos) = (0, 0);
    let ends = ops.iter().map(|(e, o)| (*e, Some(o))).chain(std::iter::once((line.len(), None)));
    for (end, op) in ends {
        if end > pos {
            let Some(&(pend, style)) = pieces.get(k) else { return pieces };
            if pend != end {
                return pieces; // not the token stream we expected: keep the grammar's colours
            }
            k += 1;
            let st = stack.as_slice();
            let inner = st.iter().rev().find(|s| !sc.meta.is_prefix_of(**s) && !sc.source.is_prefix_of(**s));
            let text = &line[pos..end];
            // Parameter names: a new list replaces the last one, even `(void)`.
            if has(st, sc.params) || has(st, sc.macro_params) {
                if !ctx.collecting {
                    params = Some(Vec::new());
                    ctx.collecting = true;
                }
                if inner.is_some_and(|s| sc.parameter.is_prefix_of(*s)) {
                    let w = text.trim();
                    if !w.is_empty() {
                        params.get_or_insert_with(|| ctx.params.to_vec()).push(w.to_string());
                    }
                }
            } else if !text.trim().is_empty() {
                ctx.collecting = false;
            }
            let eligible = inner.is_none_or(|s| sc.call.is_prefix_of(*s));
            if !eligible {
                out.push((end, style));
            } else {
                let cur = params.as_deref().unwrap_or(&ctx.params);
                let b = line.as_bytes();
                let mut i = pos;
                while i < end {
                    let starts = (b[i].is_ascii_alphabetic() || b[i] == b'_') && (i == 0 || !ident_byte(b[i - 1]));
                    if !starts {
                        i += 1;
                        continue;
                    }
                    let mut j = i;
                    while j < b.len() && ident_byte(b[j]) {
                        j += 1;
                    }
                    if j > end {
                        break; // runs past this token: not ours to classify
                    }
                    let here = Here { w: &line[i..j], before: line[..i].trim_end(), after: line[j..].trim_start(), stack: st };
                    let class = if lang == Lang::C { classify_c(&here, cur) } else { classify_rust(&here, cur) };
                    if i > pos && out.last().is_none_or(|(e, _)| *e < i) {
                        out.push((i, style));
                    }
                    out.push((j, sem[class as usize]));
                    i = j;
                }
                if out.last().is_none_or(|(e, _)| *e < end) {
                    out.push((end, style));
                }
            }
        }
        if let Some(op) = op {
            if stack.apply(op).is_err() {
                return pieces;
            }
        }
        pos = pos.max(end);
    }
    if let Some(p) = params {
        ctx.params = Rc::new(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_sublime_scopes_to_vscode_names() {
        let a = |s: &str| alias(Scope::new(s).unwrap()).build_string();
        assert_eq!(a("keyword.control.import.include.c"), "keyword.control.directive.include.c");
        assert_eq!(a("keyword.control.import.python"), "keyword.control.import.python", "C family only");
        assert_eq!(a("entity.name.constant.preprocessor.c"), "entity.name.function.preprocessor.c");
        assert_eq!(a("variable.function.c"), "entity.name.function.call.c");
        assert_eq!(a("entity.name.struct.rust"), "entity.name.type.struct.rust");
        assert_eq!(a("comment.line.c"), "comment.line.c");
    }

    #[test]
    fn classifies_c_identifiers() {
        let none: [Scope; 0] = [];
        let c = |line: &str, w: &str, params: &[&str]| {
            let i = line.find(w).unwrap();
            let p: Vec<String> = params.iter().map(|s| s.to_string()).collect();
            let body = [Scope::new("meta.function.c").unwrap(), Scope::new("meta.block.c").unwrap()];
            let stack: &[Scope] = if params.is_empty() { &none } else { &body };
            let h = Here { w, before: line[..i].trim_end(), after: line[i + w.len()..].trim_start(), stack };
            classify_c(&h, &p)
        };
        assert_eq!(c("pr_info(\"x\");", "pr_info", &[]), Sem::Function);
        assert_eq!(c("WARN_ON(x);", "WARN_ON", &[]), Sem::Macro);
        assert_eq!(c("return -EINVAL;", "EINVAL", &[]), Sem::Macro);
        assert_eq!(c("priv->port = 1;", "port", &[]), Sem::Property);
        assert_eq!(c("a.b = 1;", "b", &[]), Sem::Property);
        assert_eq!(c("struct mxl_priv *p", "mxl_priv", &[]), Sem::Type);
        assert_eq!(c("foo_t x;", "foo_t", &[]), Sem::Type);
        assert_eq!(c("priv->port = idx;", "priv", &["priv", "idx"]), Sem::Parameter);
        assert_eq!(c("priv->port = idx;", "idx", &["priv", "idx"]), Sem::Parameter);
        assert_eq!(c("int ret = 0;", "ret", &["priv"]), Sem::Variable);
        assert_eq!(c("out:", "out", &[]), Sem::Label);
    }
}

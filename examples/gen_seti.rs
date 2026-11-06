//! Regenerate assets/seti.json: VS Code's default file icon theme (Seti) mapped
//! to the Nerd Font's `nf-seti-*` codepoints.
//!
//!   cargo run --example gen_seti -- VSCODE_APP_DIR NERD_FONT.ttf > assets/seti.json
//!
//! VSCODE_APP_DIR is .../resources/app. A Seti icon id names its glyph
//! (`_c_1` is glyph `c` in a second colour); the Nerd Font names that glyph
//! `seti-c` in its `post` table, found here with a small cmap/post reader.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

fn u16_at(b: &[u8], o: usize) -> Option<usize> {
    Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?) as usize)
}

fn u32_at(b: &[u8], o: usize) -> Option<usize> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?) as usize)
}

fn table<'a>(font: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    (0..u16_at(font, 4)?).find_map(|i| {
        let r = 12 + 16 * i;
        (font.get(r..r + 4)? == tag).then_some(())?;
        font.get(u32_at(font, r + 8)?..u32_at(font, r + 8)? + u32_at(font, r + 12)?)
    })
}

/// Glyph id -> codepoint from a format 12 (3,10) or format 4 (3,1) cmap subtable.
fn cmap(t: &[u8]) -> Option<HashMap<usize, u32>> {
    let sub = |pe: (usize, usize)| {
        (0..u16_at(t, 2)?).find_map(|i| {
            let r = 4 + 8 * i;
            ((u16_at(t, r)?, u16_at(t, r + 2)?) == pe).then(|| u32_at(t, r + 4)).flatten()
        })
    };
    let mut m = HashMap::new();
    if let Some(o) = sub((3, 10)).filter(|&o| u16_at(t, o) == Some(12)) {
        for g in 0..u32_at(t, o + 12)? {
            let r = o + 16 + 12 * g;
            let (start, end, gid) = (u32_at(t, r)?, u32_at(t, r + 4)?, u32_at(t, r + 8)?);
            for c in start..=end {
                m.entry(gid + c - start).or_insert(c as u32);
            }
        }
        return Some(m);
    }
    let o = sub((3, 1)).filter(|&o| u16_at(t, o) == Some(4))?;
    let seg = u16_at(t, o + 6)? / 2;
    let (ends, starts, deltas, ranges) = (o + 14, o + 16 + 2 * seg, o + 16 + 4 * seg, o + 16 + 6 * seg);
    for s in 0..seg {
        let (start, end) = (u16_at(t, starts + 2 * s)?, u16_at(t, ends + 2 * s)?);
        let (delta, range) = (u16_at(t, deltas + 2 * s)?, u16_at(t, ranges + 2 * s)?);
        for c in start..=end.min(0xFFFE) {
            let g = if range == 0 {
                c + delta
            } else {
                match u16_at(t, ranges + 2 * s + range + 2 * (c - start))? {
                    0 => continue,
                    g => g + delta,
                }
            } & 0xFFFF;
            m.entry(g).or_insert(c as u32);
        }
    }
    Some(m)
}

/// Glyph names from a version 2.0 `post` table (only the font's own names).
fn post_names(t: &[u8]) -> Option<HashMap<usize, String>> {
    (u32_at(t, 0)? == 0x0002_0000).then_some(())?;
    let n = u16_at(t, 32)?;
    let mut strings = Vec::new();
    let mut o = 34 + 2 * n;
    while o < t.len() {
        let len = t[o] as usize;
        strings.push(String::from_utf8_lossy(t.get(o + 1..o + 1 + len)?).into_owned());
        o += 1 + len;
    }
    let mut m = HashMap::new();
    for g in 0..n {
        let i = u16_at(t, 34 + 2 * g)?;
        if let Some(s) = i.checked_sub(258).and_then(|k| strings.get(k)) {
            m.insert(g, s.clone());
        }
    }
    Some(m)
}

/// `_c_1_light` -> `c`; `_go2` stays `go2`.
fn seti_name(id: &str) -> String {
    let n = id.trim_start_matches('_').trim_end_matches("_light");
    match n.rsplit_once('_') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head.into(),
        _ => n.into(),
    }
}

/// JSON with non-ASCII escaped, so the private-use glyphs stay readable.
fn ascii_json(v: &Value) -> String {
    let mut out = String::new();
    for c in v.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [app, nerd] = &args[..] else {
        return Err("usage: gen_seti VSCODE_APP_DIR NERD_FONT.ttf".into());
    };
    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
    let parse = |p: &Path| -> Result<Value, String> { serde_json::from_slice(&read(p)?).map_err(|e| format!("{}: {e}", p.display())) };
    let theme = parse(&Path::new(app).join("extensions/theme-seti/icons/vs-seti-icon-theme.json"))?;
    let font = read(Path::new(nerd))?;
    let names = post_names(table(&font, b"post").ok_or("no post table")?).ok_or("unsupported post table")?;
    let cps = cmap(table(&font, b"cmap").ok_or("no cmap table")?).ok_or("unsupported cmap")?;
    let by_name: HashMap<&str, u32> = names.iter().filter_map(|(g, n)| Some((n.as_str(), *cps.get(g)?))).collect();
    // Icons whose glyph has another name: the post table holds one name per glyph
    // (nf-seti-json is U+E60B, named seti-less), and _html_erb draws Seti's ejs glyph.
    let shared = HashMap::from([
        ("json", "less"),
        ("clock", "time_cop"),
        ("git", "git_ignore"),
        ("npm", "npm_ignored"),
        ("html_erb", "ejs"),
    ]);

    let mut icons: Vec<Value> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut missing = std::collections::BTreeSet::new();
    let mut icon = |id: &str| -> usize {
        if let Some(&i) = index.get(id) {
            return i;
        }
        let d = &theme["iconDefinitions"][id];
        let name = seti_name(id);
        let alt = shared.get(name.as_str()).copied().unwrap_or(&name).replace('-', "_");
        let tries = [format!("seti-{name}"), format!("seti-{alt}"), format!("seti-{}", name.to_lowercase()), format!("custom-{name}")];
        let cp = tries.iter().find_map(|t| by_name.get(t.as_str())).copied().unwrap_or_else(|| {
            missing.insert(name.clone());
            by_name.get("custom-default").copied().unwrap_or(0x20)
        });
        let color = d["fontColor"].as_str().unwrap_or("").to_uppercase(); // "" = the list foreground
        icons.push(json!([char::from_u32(cp).unwrap_or(' ').to_string(), color]));
        index.insert(id.into(), icons.len() - 1);
        icons.len() - 1
    };

    let (mut lexts, mut lnames) = (BTreeMap::new(), BTreeMap::new());
    let mut pkgs: Vec<_> = std::fs::read_dir(Path::new(app).join("extensions"))
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path().join("package.json"))
        .filter(|p| p.is_file())
        .collect();
    pkgs.sort();
    for pkg in pkgs {
        let Ok(v) = parse(&pkg) else { continue };
        for lang in v["contributes"]["languages"].as_array().into_iter().flatten() {
            let Some(id) = lang["id"].as_str().and_then(|l| theme["languageIds"][l].as_str()) else { continue };
            for e in lang["extensions"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let k = e.trim_start_matches('.').to_lowercase();
                lexts.entry(k).or_insert_with(|| icon(id));
            }
            for n in lang["filenames"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let k = n.to_lowercase();
                lnames.entry(k).or_insert_with(|| icon(id));
            }
        }
    }
    let default = icon(theme["file"].as_str().ok_or("no default file icon")?);
    let mut map = |k: &str| -> Map<String, Value> {
        theme[k].as_object().into_iter().flatten().filter_map(|(n, id)| Some((n.to_lowercase(), json!(icon(id.as_str()?))))).collect()
    };
    let (names_m, exts_m) = (map("fileNames"), map("fileExtensions"));
    if !missing.is_empty() {
        eprintln!("no Nerd Font glyph (uses custom-default): {}", missing.into_iter().collect::<Vec<_>>().join(" "));
    }
    let out = json!({
        "source": format!("VS Code theme-seti {}", theme["version"].as_str().unwrap_or("")),
        "default": default,
        "names": names_m,
        "exts": exts_m,
        "lnames": lnames,
        "lexts": lexts,
        "icons": icons,
    });
    println!("{}", ascii_json(&out));
    Ok(())
}

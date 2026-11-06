//! Regenerate assets/codicons.json: the activity bar's large icons
//! (`herdr-code.icons.size: "large"`), VS Code codicons rasterized into 3x2 braille cells.
//!
//!   cargo run --example gen_codicons -- .../resources/app/out/media/codicon.ttf > assets/codicons.json
//!
//! The TrueType tables (cmap, head, maxp, loca, glyf) are read here, with no font crate:
//! each glyph's quadratic outline is flattened and filled with the nonzero rule on a
//! supersampled grid of 6x8 dots (3 cells of 2 dots by 2 cells of 4); a dot is set where
//! enough of it is covered. The icon is square, 6x6 dots, centred in the 8 rows. A
//! malformed font gives an error, never a panic.

use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// The activity bar's icons: name (as the panel asks for it), codicon codepoint.
const ICONS: &[(&str, u32)] = &[("files", 0xEAF0), ("search", 0xEA6D), ("source-control", 0xEA68)];
/// Dots per icon (braille: 2 per cell across, 4 down) and samples per dot side.
const W: usize = 6;
const H: usize = 8;
const SS: usize = 8;
/// Share of a dot's samples that must be inside the glyph: low, as a 1/16-em stroke covers
/// about a third of a dot at this size.
const COVER: f64 = 0.18;

fn u16_at(b: &[u8], o: usize) -> Option<usize> {
    Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?) as usize)
}

fn i16_at(b: &[u8], o: usize) -> Option<i32> {
    Some(i16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?) as i32)
}

fn u32_at(b: &[u8], o: usize) -> Option<usize> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?) as usize)
}

fn table<'a>(font: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    (0..u16_at(font, 4)?).find_map(|i| {
        let r = 12 + 16 * i;
        (font.get(r..r + 4)? == tag).then_some(())?;
        font.get(u32_at(font, r + 8)?..u32_at(font, r + 8)?.checked_add(u32_at(font, r + 12)?)?)
    })
}

/// Codepoint -> glyph id from a format 4 (3,1) cmap subtable (codicons are in the BMP).
fn cmap(t: &[u8]) -> Option<HashMap<u32, usize>> {
    let o = (0..u16_at(t, 2)?).find_map(|i| {
        let r = 4 + 8 * i;
        ((u16_at(t, r)?, u16_at(t, r + 2)?) == (3, 1)).then(|| u32_at(t, r + 4)).flatten()
    })?;
    (u16_at(t, o)? == 4).then_some(())?;
    let seg = u16_at(t, o + 6)? / 2;
    let (ends, starts, deltas, ranges) = (o + 14, o + 16 + 2 * seg, o + 16 + 4 * seg, o + 16 + 6 * seg);
    let mut m = HashMap::new();
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
            m.insert(c as u32, g);
        }
    }
    Some(m)
}

/// One contour's points: (x, y, on-curve).
type Contour = Vec<(f64, f64, bool)>;

/// The contours of a simple glyph.
fn outline(font: &[u8], gid: usize) -> Result<Vec<Contour>, String> {
    let head = table(font, b"head").ok_or("no head table")?;
    let long = i16_at(head, 50).ok_or("bad head")? == 1;
    let loca = table(font, b"loca").ok_or("no loca table")?;
    let glyf = table(font, b"glyf").ok_or("no glyf table (not TrueType outlines)")?;
    let at = |i: usize| if long { u32_at(loca, 4 * i) } else { u16_at(loca, 2 * i).map(|v| 2 * v) };
    let (a, b) = (at(gid).ok_or("bad loca")?, at(gid + 1).ok_or("bad loca")?);
    let g = glyf.get(a..b).ok_or("glyph outside glyf")?;
    let n = i16_at(g, 0).ok_or("empty glyph")?;
    if n < 0 {
        return Err(format!("glyph {gid} is a composite (not supported)"));
    }
    let n = n as usize;
    let ends: Vec<usize> = (0..n).map(|i| u16_at(g, 10 + 2 * i)).collect::<Option<_>>().ok_or("bad contour ends")?;
    let count = ends.last().map_or(0, |e| e + 1);
    let ilen = u16_at(g, 10 + 2 * n).ok_or("bad instructions")?;
    let mut o = 12 + 2 * n + ilen;
    let mut flags = Vec::with_capacity(count);
    while flags.len() < count {
        let f = *g.get(o).ok_or("flags past the glyph")?;
        o += 1;
        let rep = if f & 8 != 0 {
            o += 1;
            *g.get(o - 1).ok_or("flags past the glyph")? as usize
        } else {
            0
        };
        flags.extend(std::iter::repeat_n(f, rep + 1));
    }
    flags.truncate(count);
    let mut coord = |short: u8, same: u8| -> Result<Vec<i32>, String> {
        let mut v = 0i32;
        let mut out = Vec::with_capacity(count);
        for &f in &flags {
            if f & short != 0 {
                let d = *g.get(o).ok_or("coordinates past the glyph")? as i32;
                o += 1;
                v += if f & same != 0 { d } else { -d };
            } else if f & same == 0 {
                v += i16_at(g, o).ok_or("coordinates past the glyph")?;
                o += 2;
            }
            out.push(v);
        }
        Ok(out)
    };
    let xs = coord(2, 16)?;
    let ys = coord(4, 32)?;
    let mut contours = Vec::new();
    let mut s = 0;
    for &e in &ends {
        if e < s || e >= count {
            return Err("bad contour ends".into());
        }
        contours.push((s..=e).map(|i| (xs[i] as f64, ys[i] as f64, flags[i] & 1 != 0)).collect());
        s = e + 1;
    }
    Ok(contours)
}

/// A contour as a closed polyline: implied on-curve midpoints between off-curve points,
/// each quadratic flattened into 8 segments.
fn flatten(c: &[(f64, f64, bool)]) -> Vec<(f64, f64)> {
    let n = c.len();
    if n == 0 {
        return Vec::new();
    }
    let on = |i: usize| -> (f64, f64) {
        let (p, q) = (c[i % n], c[(i + 1) % n]);
        if p.2 { (p.0, p.1) } else { ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0) }
    };
    // Start on an on-curve point (or the midpoint after an off-curve one).
    let first = (0..n).find(|&i| c[i].2).unwrap_or(0);
    let start = if c[first].2 { (c[first].0, c[first].1) } else { on(first) };
    let mut out = vec![start];
    let mut cur = start;
    for k in 1..=n {
        let p = c[(first + k) % n];
        if p.2 {
            cur = (p.0, p.1);
            out.push(cur);
        } else {
            let next = c[(first + k + 1) % n];
            let end = if next.2 { (next.0, next.1) } else { ((p.0 + next.0) / 2.0, (p.1 + next.1) / 2.0) };
            for s in 1..=8 {
                let t = s as f64 / 8.0;
                let u = 1.0 - t;
                out.push((u * u * cur.0 + 2.0 * u * t * p.0 + t * t * end.0, u * u * cur.1 + 2.0 * u * t * p.1 + t * t * end.1));
            }
            cur = end;
        }
    }
    out
}

/// The glyph's dots (W x H), the em square mapped onto the middle W x W dots.
fn raster(contours: &[Contour], upm: f64, ascender: f64) -> Vec<Vec<bool>> {
    let polys: Vec<Vec<(f64, f64)>> = contours.iter().map(|c| flatten(c)).collect();
    let (gw, gh) = (W * SS, H * SS);
    let scale = gw as f64 / upm;
    let top = ((H - W) * SS / 2) as f64;
    // Edges in sample space: (x0, y0, x1, y1), y down.
    let edges: Vec<(f64, f64, f64, f64)> = polys
        .iter()
        .flat_map(|p| (0..p.len()).map(move |i| (p[i], p[(i + 1) % p.len()])))
        .map(|((x0, y0), (x1, y1))| (x0 * scale, top + (ascender - y0) * scale, x1 * scale, top + (ascender - y1) * scale))
        .collect();
    let mut inside = vec![vec![false; gw]; gh];
    for (sy, row) in inside.iter_mut().enumerate() {
        let y = sy as f64 + 0.5;
        let mut xs: Vec<(f64, i32)> = edges
            .iter()
            .filter(|e| (e.1 <= y) != (e.3 <= y))
            .map(|e| (e.0 + (y - e.1) / (e.3 - e.1) * (e.2 - e.0), if e.3 > e.1 { 1 } else { -1 }))
            .collect();
        xs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut wind = 0;
        for w in xs.windows(2) {
            wind += w[0].1;
            let (a, b) = (w[0].0.clamp(0.0, gw as f64).round() as usize, w[1].0.clamp(0.0, gw as f64).round() as usize);
            if wind != 0 && a < b {
                row[a..b].fill(true);
            }
        }
    }
    (0..H)
        .map(|dy| {
            (0..W)
                .map(|dx| {
                    let n = (0..SS).flat_map(|j| (0..SS).map(move |i| (i, j))).filter(|&(i, j)| inside[dy * SS + j][dx * SS + i]).count();
                    n as f64 >= COVER * (SS * SS) as f64
                })
                .collect()
        })
        .collect()
}

/// W x H dots as 2 rows of 3 braille cells.
fn braille(dots: &[Vec<bool>]) -> Vec<String> {
    const BIT: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    (0..H / 4)
        .map(|cy| {
            (0..W / 2)
                .map(|cx| {
                    let mut b = 0;
                    for (dx, col) in BIT.iter().enumerate() {
                        for (dy, bit) in col.iter().enumerate() {
                            if dots[cy * 4 + dy][cx * 2 + dx] {
                                b |= bit;
                            }
                        }
                    }
                    char::from_u32(0x2800 + b).unwrap_or(' ')
                })
                .collect()
        })
        .collect()
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path] = &args[..] else {
        return Err("usage: gen_codicons CODICON.ttf".into());
    };
    let font = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let head = table(&font, b"head").ok_or("no head table")?;
    let upm = u16_at(head, 18).filter(|&u| u > 0).ok_or("bad unitsPerEm")? as f64;
    let hhea = table(&font, b"hhea").ok_or("no hhea table")?;
    let ascender = i16_at(hhea, 4).ok_or("bad hhea")? as f64;
    let cps = cmap(table(&font, b"cmap").ok_or("no cmap table")?).ok_or("unsupported cmap (want format 4)")?;
    let mut out = Map::new();
    out.insert("//".into(), json!("VS Code codicons (CC BY 4.0, Microsoft) rasterized to 3x2 braille cells by examples/gen_codicons.rs"));
    for &(name, cp) in ICONS {
        let gid = *cps.get(&cp).ok_or(format!("U+{cp:04X} ({name}) not in the font"))?;
        let dots = raster(&outline(&font, gid)?, upm, ascender);
        let rows = braille(&dots);
        eprintln!("{name}:\n  {}\n  {}", rows[0], rows[1]);
        out.insert(name.into(), json!(rows));
    }
    println!("{}", serde_json::to_string_pretty(&Value::Object(out)).map_err(|e| e.to_string())?);
    Ok(())
}

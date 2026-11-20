//! Lane layout for the History view's graph: one text row per commit, drawn
//! like VS Code's Source Control Graph (`git log --graph` without its extra
//! connector rows). Pure: feed page after page through the same `Lanes` and
//! the shape continues across "View more" without a seam; a branch's colour
//! only reaches lanes opened on its commit's page (the GRAPH lays out all of
//! its pages again on each).

use crate::gitops::{Commit, RefLabel};
use std::collections::HashMap;

/// Lane colours past the rotating palette: a branch's badge colour, which its lane
/// takes from the branch's commit down (HEAD's branch, a remote one, any other).
pub const CURRENT: usize = 1 << 20;
pub const REMOTE: usize = CURRENT + 1;
pub const BRANCH: usize = CURRENT + 2;

/// The colour a commit's refs give its lane: its first branch (HEAD's own first, as
/// the badges order them). Tags give none.
fn ref_color(c: &Commit) -> Option<usize> {
    let mut v: Vec<&RefLabel> = c.refs.iter().filter(|r| !matches!(r, RefLabel::Tag(_))).collect();
    v.sort_by_key(|r| !matches!(r, RefLabel::Current(_) | RefLabel::Head));
    v.first().map(|r| match r {
        RefLabel::Current(_) => CURRENT,
        RefLabel::Remote(_) => REMOTE,
        _ => BRANCH,
    })
}

/// Lane state between rows, and so between pages. Start with `Lanes::default()`
/// for page 0 and keep it while the same history is paged.
#[derive(Clone, Debug, Default)]
pub struct Lanes {
    /// Per lane: the hash it waits for and its colour. None is a free lane.
    slots: Vec<Option<(String, usize)>>,
    next_color: usize,
}

/// One terminal cell. `color` is a lane colour index; the UI draws it with
/// `palette[color % palette.len()]`. Blank cells carry colour 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub glyph: char,
    pub color: usize,
    /// Where its lines go: `UP | DOWN | LEFT | RIGHT` (the node's too: up when a child
    /// waits for it, down when it has a parent), for drawing it in pixels.
    pub bits: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRow {
    /// Index in `cells` of the commit's `●`. Lane `n` is drawn at cell `2 * n`;
    /// odd cells only carry horizontal links.
    pub node: usize,
    /// No trailing blanks: rows differ in width, so pad to the widest visible row.
    pub cells: Vec<Cell>,
}

pub const UP: u8 = 1;
pub const DOWN: u8 = 2;
pub const LEFT: u8 = 4;
pub const RIGHT: u8 = 8;
/// Box-drawing glyph for each UP|DOWN|LEFT|RIGHT combination.
const GLYPHS: [char; 16] = [' ', '│', '│', '│', '─', '╯', '╮', '┤', '─', '╰', '╭', '├', '─', '┴', '┬', '┼'];

impl Lanes {
    /// Rows for the next commits, in the order `gitops::log_page` returns them.
    pub fn rows(&mut self, commits: &[Commit]) -> Vec<GraphRow> {
        // A merged branch's lane opens at the merge: in its colour already when its
        // commit is on this page.
        let tips: HashMap<&str, usize> = commits.iter().filter_map(|c| Some((c.hash.as_str(), ref_color(c)?))).collect();
        commits.iter().map(|c| self.row(c, &tips)).collect()
    }

    fn row(&mut self, c: &Commit, tips: &HashMap<&str, usize>) -> GraphRow {
        let waits = |s: &Option<(String, usize)>, h: &str| s.as_ref().is_some_and(|(w, _)| w == h);
        let hits: Vec<usize> = (0..self.slots.len()).filter(|&i| waits(&self.slots[i], &c.hash)).collect();
        let (col, color) = match hits.first() {
            Some(&i) => (i, self.slots[i].as_ref().map_or(0, |s| s.1)),
            None => (self.free(0), self.color()),
        };
        let color = tips.get(c.hash.as_str()).copied().unwrap_or(color);
        let mut cells: Vec<(u8, usize)> = Vec::new();
        for (i, s) in self.slots.iter().enumerate() {
            if let Some((_, k)) = s {
                if i != col && !hits.contains(&i) {
                    put(&mut cells, 2 * i, UP | DOWN, *k);
                }
            }
        }
        // Other children of this commit: their lanes end here, joining the node.
        for &i in hits.iter().skip(1) {
            let k = self.slots[i].take().map_or(0, |s| s.1);
            link(&mut cells, col, i, UP, k);
        }
        self.slots[col] = c.parents.first().map(|p| (p.clone(), color));
        // A merge's other parents join the lane already waiting for them, or open one to the right.
        for p in c.parents.iter().skip(1) {
            match self.slots.iter().position(|s| waits(s, p)) {
                Some(i) if i == col => {}
                Some(i) => {
                    let k = self.slots[i].as_ref().map_or(0, |s| s.1);
                    link(&mut cells, col, i, 0, k);
                }
                None => {
                    // The rotation advances either way: pages split anywhere still match.
                    let (i, k) = (self.free(col + 1), self.color());
                    let k = tips.get(p.as_str()).copied().unwrap_or(k);
                    self.slots[i] = Some((p.clone(), k));
                    link(&mut cells, col, i, DOWN, k);
                }
            }
        }
        while self.slots.last().is_some_and(Option::is_none) {
            self.slots.pop();
        }
        let node = 2 * col;
        put(&mut cells, node, 0, color);
        let mut out: Vec<Cell> = cells.iter().map(|&(b, k)| Cell { glyph: GLYPHS[b as usize], color: k, bits: b }).collect();
        let side = |i: Option<usize>, bit: u8| i.and_then(|i| cells.get(i)).is_some_and(|c| c.0 & bit != 0);
        let bits = if hits.is_empty() { 0 } else { UP }
            | if c.parents.is_empty() { 0 } else { DOWN }
            | if side(node.checked_sub(1), RIGHT) { LEFT } else { 0 }
            | if side(Some(node + 1), LEFT) { RIGHT } else { 0 };
        out[node] = Cell { glyph: '●', color, bits };
        while out.last().is_some_and(|c| c.glyph == ' ') {
            out.pop();
        }
        GraphRow { node, cells: out }
    }

    /// First free lane at or after `from`, appending one if none is free.
    fn free(&mut self, from: usize) -> usize {
        match self.slots.iter().skip(from).position(Option::is_none) {
            Some(i) => from + i,
            None => {
                self.slots.push(None);
                self.slots.len() - 1
            }
        }
    }

    fn color(&mut self) -> usize {
        self.next_color += 1;
        self.next_color - 1
    }
}

/// OR `bits` into cell `i`; the first edge drawn into a cell gives its colour,
/// so a crossing keeps the colour of the lane passing through it.
fn put(cells: &mut Vec<(u8, usize)>, i: usize, bits: u8, color: usize) {
    if cells.len() <= i {
        cells.resize(i + 1, (0, 0));
    }
    let c = &mut cells[i];
    if c.0 == 0 {
        c.1 = color;
    }
    c.0 |= bits;
}

/// A horizontal link from the node in lane `col` to lane `lane`, whose cell
/// also gets `vertical` (UP: the lane ends here, DOWN: it starts here).
fn link(cells: &mut Vec<(u8, usize)>, col: usize, lane: usize, vertical: u8, color: usize) {
    for i in 2 * col.min(lane) + 1..2 * col.max(lane) {
        put(cells, i, LEFT | RIGHT, color);
    }
    put(cells, 2 * lane, vertical | if lane > col { LEFT } else { RIGHT }, color);
}

/// A row of the graph as drawn in pixels: a commit's (its node filled: HEAD's), the
/// lanes passing a row under a commit (its files: cell, colour), or none.
pub enum Paint<'a> {
    Commit(&'a GraphRow, bool),
    Through(Vec<(usize, usize)>),
    Blank,
}

/// The lanes going on below `row`: its cells with a line down.
pub fn through(row: &GraphRow) -> Vec<(usize, usize)> {
    row.cells.iter().enumerate().filter(|(_, c)| c.bits & DOWN != 0).map(|(i, c)| (i, c.color)).collect()
}

/// `rows` drawn in pixels, `cols` cells wide, `cell` pixels a cell, as VS Code draws its
/// graph: lines a sixth of a cell thick, corners as curves, each commit a dot (a ring
/// but HEAD's), anti-aliased, on transparent pixels (the row's own background shows).
pub fn pixels(rows: &[Paint], cols: usize, (cw, ch): (u32, u32), color: &dyn Fn(usize) -> image::Rgba<u8>) -> image::RgbaImage {
    let mut img = image::RgbaImage::new(cols as u32 * cw, rows.len() as u32 * ch);
    let t = (cw as f32 / 6.0).clamp(1.2, 3.0);
    let r = (cw.min(ch) as f32 * 0.36).max(2.5);
    for (y, row) in rows.iter().enumerate() {
        let cells: Vec<(usize, u8, usize, Option<bool>)> = match row {
            Paint::Commit(g, filled) => g.cells.iter().enumerate().map(|(i, c)| (i, c.bits, c.color, (i == g.node).then_some(*filled))).collect(),
            Paint::Through(v) => v.iter().map(|&(i, k)| (i, UP | DOWN, k, None)).collect(),
            Paint::Blank => Vec::new(),
        };
        for (i, bits, k, node) in cells.into_iter().filter(|c| c.0 < cols && (c.1 != 0 || c.3.is_some())) {
            let (x0, y0) = (i as f32 * cw as f32, y as f32 * ch as f32);
            // Lines through pixel centres: sharp, not two half-lit pixels.
            let (cx, cy, x1, y1) = (x0 + (cw / 2) as f32 + 0.5, y0 + (ch / 2) as f32 + 0.5, x0 + cw as f32, y0 + ch as f32);
            // A ring's lines stop at it: its middle shows the row.
            let gap = if node == Some(false) { r } else { 0.0 };
            let mut segs: Vec<[f32; 4]> = Vec::new();
            let corner = matches!(bits, b if b.count_ones() == 2 && b & (UP | DOWN) != 0 && b & (LEFT | RIGHT) != 0);
            if corner && node.is_none() {
                // A quarter curve from the edge the line comes in at to the one it leaves by.
                let a = if bits & UP != 0 { (cx, y0) } else { (cx, y1) };
                let b = if bits & LEFT != 0 { (x0, cy) } else { (x1, cy) };
                let n = 12;
                let at = |s: f32| {
                    let u = 1.0 - s;
                    (u * u * a.0 + 2.0 * u * s * cx + s * s * b.0, u * u * a.1 + 2.0 * u * s * cy + s * s * b.1)
                };
                for j in 0..n {
                    let (p, q) = (at(j as f32 / n as f32), at((j + 1) as f32 / n as f32));
                    segs.push([p.0, p.1, q.0, q.1]);
                }
            } else {
                for (bit, seg) in [(UP, [cx, y0, cx, cy - gap]), (DOWN, [cx, cy + gap, cx, y1]), (LEFT, [x0, cy, cx - gap, cy]), (RIGHT, [cx + gap, cy, x1, cy])] {
                    if bits & bit != 0 {
                        segs.push(seg);
                    }
                }
            }
            let c = color(k);
            for py in y0 as u32..y1 as u32 {
                for px in x0 as u32..x1 as u32 {
                    let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                    let mut a = segs.iter().map(|s| cover(t / 2.0 - dist(fx, fy, s))).fold(0.0, f32::max);
                    if let Some(filled) = node {
                        let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                        a = a.max(if filled { cover(r - d) } else { cover(t / 2.0 - (d - r).abs()) });
                    }
                    if a > 0.0 {
                        let p = img.get_pixel_mut(px, py);
                        let under = f32::from(p[3]) / 255.0;
                        let out = a + under * (1.0 - a);
                        for ch in 0..3 {
                            p[ch] = ((f32::from(c[ch]) * a + f32::from(p[ch]) * under * (1.0 - a)) / out.max(1e-6)).round() as u8;
                        }
                        p[3] = (out * 255.0).round() as u8;
                    }
                }
            }
        }
    }
    img
}

/// How much of a pixel a shape covers, from how far inside its edge the pixel's centre is.
fn cover(inside: f32) -> f32 {
    (inside + 0.5).clamp(0.0, 1.0)
}

/// From (`x`, `y`) to the segment `s` (x0, y0, x1, y1).
fn dist(x: f32, y: f32, s: &[f32; 4]) -> f32 {
    let (dx, dy) = (s[2] - s[0], s[3] - s[1]);
    let len = dx * dx + dy * dy;
    let u = if len == 0.0 { 0.0 } else { (((x - s[0]) * dx + (y - s[1]) * dy) / len).clamp(0.0, 1.0) };
    ((x - s[0] - u * dx).powi(2) + (y - s[1] - u * dy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(hash: &str, parents: &[&str]) -> Commit {
        Commit { hash: hash.into(), parents: parents.iter().map(|p| p.to_string()).collect(), ..Default::default() }
    }

    fn text(rows: &[GraphRow]) -> Vec<String> {
        rows.iter().map(|r| r.cells.iter().map(|c| c.glyph).collect()).collect()
    }

    #[test]
    fn merge_then_fork_point() {
        let log = [c("M", &["C", "D"]), c("D", &["B"]), c("C", &["B"]), c("B", &["A"]), c("A", &[])];
        let rows = Lanes::default().rows(&log);
        assert_eq!(text(&rows), ["●─╮", "│ ●", "● │", "●─╯", "●"]);
        assert_eq!(rows.iter().map(|r| r.node).collect::<Vec<_>>(), [0, 2, 0, 0, 0]);
        // The first-parent line keeps colour 0; the side branch is colour 1 from end to end.
        assert_eq!([rows[0].cells[2].color, rows[1].cells[2].color, rows[3].cells[2].color], [1, 1, 1]);
        assert_eq!([rows[0].cells[0].color, rows[3].cells[0].color], [0, 0]);
    }

    #[test]
    fn a_branch_colours_its_lane_from_the_merge_down() {
        let r = |mut c: Commit, refs: Vec<RefLabel>| {
            c.refs = refs;
            c
        };
        let log = [
            r(c("M", &["A", "B"]), vec![RefLabel::Current("main".into())]),
            c("A", &["X"]),
            r(c("B", &["X"]), vec![RefLabel::Tag("v1".into()), RefLabel::Branch("feat".into())]),
            c("X", &[]),
        ];
        let rows = Lanes::default().rows(&log);
        assert_eq!(text(&rows), ["●─╮", "● │", "│ ●", "●─╯"]);
        let col = |row: usize, cell: usize| rows[row].cells[cell].color;
        assert_eq!([col(0, 0), col(1, 0), col(3, 0)], [CURRENT; 3], "HEAD's branch down its first parents");
        assert_eq!([col(0, 2), col(1, 2), col(2, 2), col(3, 2)], [BRANCH; 4], "the merged branch, merge to fork");
        // Its commit on a later page: its lane turns from there on.
        let mut lanes = Lanes::default();
        let mut split = lanes.rows(&log[..1]);
        split.extend(lanes.rows(&log[1..]));
        assert_eq!(text(&split), text(&rows));
        assert_eq!([split[0].cells[2].color, split[2].cells[2].color], [1, BRANCH]);
    }

    #[test]
    fn crossing_lanes_and_any_page_split() {
        let log = [
            c("T", &["X"]),
            c("U", &["V"]),
            c("W", &["Y"]),
            c("X", &["Z", "Y"]),
            c("V", &["Z"]),
            c("Y", &["Z"]),
            c("Z", &[]),
        ];
        let whole = Lanes::default().rows(&log);
        assert_eq!(text(&whole), ["●", "│ ●", "│ │ ●", "●─┼─┤", "│ ● │", "│ │ ●", "●─┴─╯"]);
        assert_eq!(whole[3].cells[2].color, 1, "a crossing keeps the passing lane's colour");
        for k in 1..log.len() {
            let mut lanes = Lanes::default();
            let mut rows = lanes.rows(&log[..k]);
            rows.extend(lanes.rows(&log[k..]));
            assert_eq!(rows, whole, "split at {k}");
        }
    }

    #[test]
    fn the_graph_in_pixels_dots_its_commits_curves_its_corners_and_leaves_the_rest_clear() {
        let log = [c("M", &["C", "D"]), c("D", &["B"]), c("C", &["B"]), c("B", &["A"]), c("A", &[])];
        let rows = Lanes::default().rows(&log);
        assert_eq!(rows[0].cells[0].bits, DOWN | RIGHT, "the merge: down to its parent, right to the branch");
        assert_eq!(rows[1].cells[2].bits, UP | DOWN, "the branch's commit: the merge above, its parent below");
        assert_eq!(rows[4].cells[0].bits, UP, "the root: nothing below");
        let colors = [image::Rgba([255, 0, 0, 255]), image::Rgba([0, 0, 255, 255])];
        let paint: Vec<Paint> = vec![Paint::Commit(&rows[0], true), Paint::Commit(&rows[1], false), Paint::Through(through(&rows[1])), Paint::Blank];
        let img = pixels(&paint, 3, (8, 16), &|k| colors[k % 2]);
        assert_eq!(img.dimensions(), (24, 64));
        let at = |x, y| *img.get_pixel(x, y);
        assert_eq!(at(4, 8), colors[0], "HEAD's dot, filled");
        assert_eq!(at(20, 24)[3], 0, "a ring's middle is clear");
        assert_eq!(at(20, 40), colors[1], "the lane goes on past a file row");
        assert_eq!((at(12, 2)[3], at(12, 60)[3]), (0, 0), "nothing where no line goes");
        // The corner ╮ (left, down) is a curve: its outer corner stays clear.
        assert_eq!(rows[0].cells[2].bits, LEFT | DOWN);
        assert_eq!(at(23, 9)[3], 0);
        assert!(at(19, 13)[3] > 0, "the curve passes inside the corner");
    }
}

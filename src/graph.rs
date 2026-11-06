//! Lane layout for the History view's graph: one text row per commit, drawn
//! like VS Code's Source Control Graph (`git log --graph` without its extra
//! connector rows). Pure: feed page after page through the same `Lanes` and
//! the graph continues across "View more" without a seam.

use crate::gitops::Commit;

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRow {
    /// Index in `cells` of the commit's `●`. Lane `n` is drawn at cell `2 * n`;
    /// odd cells only carry horizontal links.
    pub node: usize,
    /// No trailing blanks: rows differ in width, so pad to the widest visible row.
    pub cells: Vec<Cell>,
}

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;
/// Box-drawing glyph for each UP|DOWN|LEFT|RIGHT combination.
const GLYPHS: [char; 16] = [' ', '│', '│', '│', '─', '╯', '╮', '┤', '─', '╰', '╭', '├', '─', '┴', '┬', '┼'];

impl Lanes {
    /// Rows for the next commits, in the order `gitops::log_page` returns them.
    pub fn rows(&mut self, commits: &[Commit]) -> Vec<GraphRow> {
        commits.iter().map(|c| self.row(c)).collect()
    }

    fn row(&mut self, c: &Commit) -> GraphRow {
        let waits = |s: &Option<(String, usize)>, h: &str| s.as_ref().is_some_and(|(w, _)| w == h);
        let hits: Vec<usize> = (0..self.slots.len()).filter(|&i| waits(&self.slots[i], &c.hash)).collect();
        let (col, color) = match hits.first() {
            Some(&i) => (i, self.slots[i].as_ref().map_or(0, |s| s.1)),
            None => (self.free(0), self.color()),
        };
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
                    let (i, k) = (self.free(col + 1), self.color());
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
        let mut out: Vec<Cell> = cells.iter().map(|&(b, k)| Cell { glyph: GLYPHS[b as usize], color: k }).collect();
        out[node] = Cell { glyph: '●', color };
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
}

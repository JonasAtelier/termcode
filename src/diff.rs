//! Side-by-side diff rows: the old and new sides aligned line by line, the way
//! VS Code's diff editor pairs a replaced block and pads the shorter side.

use similar::{capture_diff_slices_deadline, Algorithm, DiffTag};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Same,
    Del,
    Ins,
    Change,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// (0-based line number, text); None = filler on that side.
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
    pub kind: Kind,
}

pub fn align(old: &str, new: &str) -> Vec<Row> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // Bounded: a pathological pair degrades to a coarser diff instead of hanging.
    let ops = capture_diff_slices_deadline(Algorithm::Myers, &a, &b, Some(Instant::now() + Duration::from_secs(2)));
    let mut rows = Vec::new();
    for op in ops {
        let (tag, ra, rb) = op.as_tag_tuple();
        let kind = match tag {
            DiffTag::Equal => Kind::Same,
            DiffTag::Delete => Kind::Del,
            DiffTag::Insert => Kind::Ins,
            DiffTag::Replace => Kind::Change,
        };
        for k in 0..ra.len().max(rb.len()) {
            let side = |r: &std::ops::Range<usize>, v: &[&str]| {
                (k < r.len()).then(|| (r.start + k, v[r.start + k].to_string()))
            };
            rows.push(Row { left: side(&ra, &a), right: side(&rb, &b), kind });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_changes_with_fillers() {
        let rows = align("a\nb\nc\nd\n", "a\nB\nB2\nc\nnew\n");
        let pairs: Vec<(Option<usize>, Option<usize>, Kind)> = rows
            .iter()
            .map(|r| (r.left.as_ref().map(|l| l.0), r.right.as_ref().map(|r| r.0), r.kind))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (Some(0), Some(0), Kind::Same),
                (Some(1), Some(1), Kind::Change),
                (None, Some(2), Kind::Change),
                (Some(2), Some(3), Kind::Same),
                (Some(3), Some(4), Kind::Change),
            ]
        );
        assert_eq!(rows[1].right.as_ref().unwrap().1, "B");
    }

    #[test]
    fn pure_insert_and_delete() {
        let rows = align("", "x\ny\n");
        assert!(rows.iter().all(|r| r.left.is_none() && r.kind == Kind::Ins));
        let rows = align("x\n", "");
        assert_eq!(rows, vec![Row { left: Some((0, "x".into())), right: None, kind: Kind::Del }]);
    }
}

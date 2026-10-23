//! Programs' floats (zett's popups, OSC 7733;layers) animated: one that comes fades in, one that
//! goes is kept as its last cells and fades and shrinks out over what is under it, one that moves
//! or changes size has its frame slide there. Matched frame to frame per pane: the same rectangle
//! is the same float, one overlapping where another was is that one moved.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A float's cell: column and row inside it, its text, colour, background, bold (1) italic (2).
pub type GhostCell = (usize, usize, String, (u8, u8, u8), Option<(u8, u8, u8)>, u8);

/// The side a float opens from (its anchor, as Neovim tells Neovide): it unrolls from there and
/// rolls back to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    /// below the cursor: grows down from its top edge
    Down,
    /// above the cursor, or on the bottom edge: grows up from its bottom edge
    Up,
    Right,
    Left,
    /// round the cursor (a centred box): grows from its middle
    Centre,
}

/// Where `r` opens from: the bottom edge it sits on (within the status and command lines), else the side of the cursor `(row, col)` it is on.
pub fn side_of(r: &[usize; 4], cursor: Option<(usize, usize)>, rows: usize) -> Side {
    if r[1] + r[3] + 2 >= rows && r[1] > 0 {
        return Side::Up;
    }
    match cursor {
        Some((cr, _)) if r[1] > cr => Side::Down,
        Some((cr, _)) if r[1] + r[3] <= cr => Side::Up,
        Some((_, cc)) if r[0] > cc => Side::Right,
        Some((_, cc)) if r[0] + r[2] <= cc => Side::Left,
        _ => Side::Centre,
    }
}

/// `r` (pixels) a part `k` (0–1) of the way out from `side`.
pub fn unrolled(r: [f32; 4], side: Side, k: f32) -> [f32; 4] {
    let k = k.clamp(0.0, 1.0);
    match side {
        Side::Down => [r[0], r[1], r[2], r[3] * k],
        Side::Up => [r[0], r[1] + r[3] * (1.0 - k), r[2], r[3] * k],
        Side::Right => [r[0], r[1], r[2] * k, r[3]],
        Side::Left => [r[0] + r[2] * (1.0 - k), r[1], r[2] * k, r[3]],
        Side::Centre => {
            let s = 0.9 + 0.1 * k;
            let (w, h) = (r[2] * s, r[3] * s);
            [r[0] + (r[2] - w) / 2.0, r[1] + (r[3] - h) / 2.0, w, h]
        }
    }
}

pub struct Seen {
    /// where it is, in cells
    pub rect: [usize; 4],
    /// when it came (its fade-in)
    pub born: Instant,
    /// where it slides from (cells) and since when
    pub from: Option<([usize; 4], Instant)>,
    /// its cells as last drawn, for its fade-out
    /// the side it opened from (kept while it lives)
    pub side: Side,
    pub cells: Vec<GhostCell>,
}

pub struct Gone {
    pub pane: u64,
    pub rect: [usize; 4],
    pub cells: Vec<GhostCell>,
    pub side: Side,
    pub at: Instant,
}

#[derive(Default)]
pub struct Layers {
    pub seen: HashMap<u64, Vec<Seen>>,
    pub gone: Vec<Gone>,
    /// the first frame was drawn: floats there before it (a new window, a snapshot) do not fade in
    primed: bool,
    /// headless pictures of a fade-out: the frame's floats taken as gone
    pub none_this_frame: bool,
}

/// Fast at first, gentle at the end.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn overlap(a: &[usize; 4], b: &[usize; 4]) -> bool {
    a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
}

impl Layers {
    /// Pane `pane`'s floats this frame (`now`, in cells), and `cells_of` to copy one's cells.
    pub fn update(&mut self, pane: u64, now: &[[usize; 4]], at: Instant, cells_of: impl Fn(&[usize; 4]) -> Vec<GhostCell>, side: impl Fn(&[usize; 4]) -> Side) {
        let now: &[[usize; 4]] = if self.none_this_frame { &[] } else { now };
        let old = self.seen.remove(&pane).unwrap_or_default();
        let mut used = vec![false; old.len()];
        let mut next = Vec::new();
        for r in now {
            // the same one, else one it overlaps (moved or resized), else a new one
            let same = old.iter().enumerate().position(|(i, s)| !used[i] && s.rect == *r);
            let near = same.or_else(|| old.iter().enumerate().position(|(i, s)| !used[i] && overlap(&s.rect, r)));
            let s = match near {
                Some(i) => {
                    used[i] = true;
                    let o = &old[i];
                    let from = if o.rect == *r { o.from } else { Some((o.rect, at)) };
                    Seen { rect: *r, born: o.born, from, cells: cells_of(r), side: o.side }
                }
                None => Seen { rect: *r, born: if self.primed { at } else { at.checked_sub(Duration::from_secs(3600)).unwrap_or(at) }, from: None, cells: cells_of(r), side: side(r) },
            };
            next.push(s);
        }
        for (i, s) in old.into_iter().enumerate() {
            if !used[i] {
                self.gone.push(Gone { pane, rect: s.rect, cells: s.cells, side: s.side, at });
            }
        }
        if !next.is_empty() {
            self.seen.insert(pane, next);
        }
        self.primed = true;
    }

    /// The fade-outs still running (older ones dropped).
    pub fn prune(&mut self, at: Instant, dur: Duration) {
        self.gone.retain(|g| at.duration_since(g.at) < dur);
    }

    /// Something is still moving: another frame is wanted.
    pub fn busy(&self, at: Instant, dur: Duration) -> bool {
        !self.gone.is_empty() || self.seen.values().flatten().any(|s| at.duration_since(s.born) < dur || s.from.is_some_and(|(_, t)| at.duration_since(t) < dur))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_come_move_and_go() {
        let mut l = Layers::default();
        let t0 = Instant::now();
        let cells = |r: &[usize; 4]| vec![(0, 0, format!("{}", r[0]), (255, 255, 255), None, 0)];
        // a first frame without floats (a window just opened): from then on floats fade in
        l.update(1, &[], t0, cells, |_| Side::Down);
        l.update(1, &[[10, 5, 20, 4]], t0, cells, |_| Side::Down);
        assert_eq!(l.seen[&1][0].born, t0);
        // moved one column: the same float, sliding from where it was
        let t1 = t0 + Duration::from_millis(50);
        l.update(1, &[[11, 5, 20, 4]], t1, cells, |_| Side::Down);
        let s = &l.seen[&1][0];
        assert_eq!((s.born, s.from.map(|f| f.0)), (t0, Some([10, 5, 20, 4])));
        assert!(l.gone.is_empty());
        // gone: kept as its last cells for the fade-out
        let t2 = t0 + Duration::from_millis(100);
        l.update(1, &[], t2, cells, |_| Side::Down);
        assert_eq!(l.gone.len(), 1);
        assert_eq!(l.gone[0].cells[0].2, "11");
        assert!(l.busy(t2, Duration::from_millis(140)));
        l.prune(t2 + Duration::from_millis(200), Duration::from_millis(140));
        assert!(!l.busy(t2 + Duration::from_millis(200), Duration::from_millis(140)));
        // one far away: a new float (the old one goes)
        l.update(2, &[[0, 0, 5, 2]], t2, cells, |_| Side::Down);
        l.update(2, &[[40, 10, 5, 2]], t2, cells, |_| Side::Down);
        assert_eq!(l.gone.len(), 1);
        assert_eq!(l.seen[&2][0].born, t2);
    }
}

impl Layers {
    /// For headless pictures of the animation: every float shown as if it came `ms` ago.
    pub fn came_ago(&mut self, ms: u64, at: Instant) {
        let t = at.checked_sub(Duration::from_millis(ms)).unwrap_or(at);
        for s in self.seen.values_mut().flatten() {
            s.born = t;
        }
    }
    /// For headless pictures: every float gone `ms` ago (as its cells were).
    pub fn went_ago(&mut self, ms: u64, at: Instant) {
        let t = at.checked_sub(Duration::from_millis(ms)).unwrap_or(at);
        for (pane, list) in self.seen.drain() {
            for s in list {
                self.gone.push(Gone { pane, rect: s.rect, cells: s.cells, side: s.side, at: t });
            }
        }
    }
}

#[cfg(test)]
mod first_frame_tests {
    use super::*;

    #[test]
    fn floats_there_at_the_first_frame_do_not_fade_in() {
        let mut l = Layers::default();
        let t0 = Instant::now();
        l.update(1, &[[0, 0, 10, 3]], t0, |_| Vec::new(), |_| Side::Down);
        assert!(!l.busy(t0, Duration::from_millis(140)), "a new window: its floats are simply there");
    }
}

#[cfg(test)]
mod side_tests {
    use super::*;

    #[test]
    fn floats_open_from_the_cursor_side() {
        // completion under the cursor at row 4 opens down, a hover above it up
        assert_eq!(side_of(&[10, 5, 20, 6], Some((4, 12)), 40), Side::Down);
        assert_eq!(side_of(&[10, 0, 20, 4], Some((4, 12)), 40), Side::Up);
        // which-key on the bottom edge rises from it, wherever the cursor is
        assert_eq!(side_of(&[0, 30, 80, 10], Some((2, 0)), 40), Side::Up);
        // beside the cursor; a centred box round it
        assert_eq!(side_of(&[20, 3, 10, 3], Some((4, 12)), 40), Side::Right);
        assert_eq!(side_of(&[5, 2, 40, 6], Some((4, 12)), 40), Side::Centre);
        // unrolling down keeps the top edge, up the bottom one
        assert_eq!(unrolled([0.0, 10.0, 100.0, 40.0], Side::Down, 0.5), [0.0, 10.0, 100.0, 20.0]);
        assert_eq!(unrolled([0.0, 10.0, 100.0, 40.0], Side::Up, 0.5), [0.0, 30.0, 100.0, 20.0]);
    }
}

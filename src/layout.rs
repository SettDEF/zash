//! The panes of a tab: a binary tree of splits. Pure geometry (no sessions,
//! no drawing), so every rule here is tested on its own.

use crate::keys::Dir;

#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    Leaf(u64),
    Split {
        /// true: a | b side by side; false: a above b
        side_by_side: bool,
        /// a's share of the space (0..1)
        ratio: f32,
        a: Box<Layout>,
        b: Box<Layout>,
    },
}

/// A pane's place in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn centre(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

impl Layout {
    /// Every pane with its rectangle, left to right / top to bottom.
    pub fn rects(&self, area: Rect) -> Vec<(u64, Rect)> {
        let mut out = Vec::new();
        self.place(area, &mut out);
        out
    }

    fn place(&self, r: Rect, out: &mut Vec<(u64, Rect)>) {
        match self {
            Layout::Leaf(id) => out.push((*id, r)),
            Layout::Split { side_by_side, ratio, a, b } => {
                if *side_by_side {
                    let wa = (r.w * ratio).round();
                    a.place(Rect { w: wa, ..r }, out);
                    b.place(Rect { x: r.x + wa, w: r.w - wa, ..r }, out);
                } else {
                    let ha = (r.h * ratio).round();
                    a.place(Rect { h: ha, ..r }, out);
                    b.place(Rect { y: r.y + ha, h: r.h - ha, ..r }, out);
                }
            }
        }
    }

    pub fn ids(&self) -> Vec<u64> {
        match self {
            Layout::Leaf(id) => vec![*id],
            Layout::Split { a, b, .. } => {
                let mut v = a.ids();
                v.extend(b.ids());
                v
            }
        }
    }

    /// Splits the pane `at`: the new pane goes right of it or below it, half each.
    pub fn split(&mut self, at: u64, new: u64, side_by_side: bool) -> bool {
        match self {
            Layout::Leaf(id) if *id == at => {
                *self = Layout::Split { side_by_side, ratio: 0.5, a: Box::new(Layout::Leaf(at)), b: Box::new(Layout::Leaf(new)) };
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split { a, b, .. } => a.split(at, new, side_by_side) || b.split(at, new, side_by_side),
        }
    }

    /// The tree without pane `id` (its sibling takes the space); None when it was the last.
    pub fn remove(self, id: u64) -> Option<Layout> {
        match self {
            Layout::Leaf(x) if x == id => None,
            Layout::Leaf(_) => Some(self),
            Layout::Split { side_by_side, ratio, a, b } => match (a.remove(id), b.remove(id)) {
                (Some(a), Some(b)) => Some(Layout::Split { side_by_side, ratio, a: Box::new(a), b: Box::new(b) }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// Moves the divider nearest to pane `id` that runs across `dir` by `by` (a share of
    /// that split's size): Right/Down push it right/down, Left/Up pull it back.
    pub fn resize(&mut self, id: u64, dir: Dir, by_px: f32, area: Rect) -> bool {
        let horizontal = matches!(dir, Dir::Left | Dir::Right);
        let sign = if matches!(dir, Dir::Right | Dir::Down) { 1.0 } else { -1.0 };
        self.resize_in(id, horizontal, sign * by_px, area)
    }

    fn resize_in(&mut self, id: u64, horizontal: bool, delta_px: f32, r: Rect) -> bool {
        let Layout::Split { side_by_side, ratio, a, b } = self else { return false };
        let (ra, rb) = if *side_by_side {
            let wa = (r.w * *ratio).round();
            (Rect { w: wa, ..r }, Rect { x: r.x + wa, w: r.w - wa, ..r })
        } else {
            let ha = (r.h * *ratio).round();
            (Rect { h: ha, ..r }, Rect { y: r.y + ha, h: r.h - ha, ..r })
        };
        let in_a = a.ids().contains(&id);
        if !in_a && !b.ids().contains(&id) {
            return false;
        }
        // the innermost matching split wins
        let inner = if in_a { a.resize_in(id, horizontal, delta_px, ra) } else { b.resize_in(id, horizontal, delta_px, rb) };
        if inner {
            return true;
        }
        if *side_by_side == horizontal {
            let size = if horizontal { r.w } else { r.h };
            if size > 0.0 {
                *ratio = (*ratio + delta_px / size).clamp(0.08, 0.92);
            }
            return true;
        }
        false
    }

    /// The pane next to `from` in a direction: the nearest one whose span overlaps.
    pub fn neighbour(&self, from: u64, dir: Dir, area: Rect) -> Option<u64> {
        let all = self.rects(area);
        let (_, f) = *all.iter().find(|(id, _)| *id == from)?;
        let (fx, fy) = f.centre();
        all.iter()
            .filter(|(id, _)| *id != from)
            .filter(|(_, r)| match dir {
                Dir::Left => r.x + r.w <= f.x + 1.0 && r.y < f.y + f.h && r.y + r.h > f.y,
                Dir::Right => r.x >= f.x + f.w - 1.0 && r.y < f.y + f.h && r.y + r.h > f.y,
                Dir::Up => r.y + r.h <= f.y + 1.0 && r.x < f.x + f.w && r.x + r.w > f.x,
                Dir::Down => r.y >= f.y + f.h - 1.0 && r.x < f.x + f.w && r.x + r.w > f.x,
            })
            .min_by(|(_, a), (_, b)| {
                let d = |r: &Rect| {
                    let (x, y) = r.centre();
                    (x - fx).abs() + (y - fy).abs()
                };
                d(a).total_cmp(&d(b))
            })
            .map(|(id, _)| *id)
    }

    /// The pane under a point.
    pub fn at(&self, x: f32, y: f32, area: Rect) -> Option<u64> {
        self.rects(area).into_iter().find(|(_, r)| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h).map(|(id, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 };

    fn three() -> Layout {
        // 1 | (2 over 3)
        let mut l = Layout::Leaf(1);
        assert!(l.split(1, 2, true));
        assert!(l.split(2, 3, false));
        l
    }

    #[test]
    fn splits_place_panes() {
        let r = three().rects(AREA);
        assert_eq!(r[0], (1, Rect { x: 0.0, y: 0.0, w: 400.0, h: 600.0 }));
        assert_eq!(r[1], (2, Rect { x: 400.0, y: 0.0, w: 400.0, h: 300.0 }));
        assert_eq!(r[2], (3, Rect { x: 400.0, y: 300.0, w: 400.0, h: 300.0 }));
    }

    #[test]
    fn neighbours_by_direction() {
        let l = three();
        assert_eq!(l.neighbour(1, Dir::Right, AREA), Some(2));
        assert_eq!(l.neighbour(3, Dir::Left, AREA), Some(1));
        assert_eq!(l.neighbour(2, Dir::Down, AREA), Some(3));
        assert_eq!(l.neighbour(3, Dir::Up, AREA), Some(2));
        assert_eq!(l.neighbour(1, Dir::Left, AREA), None);
    }

    #[test]
    fn closing_gives_the_space_to_the_sibling() {
        let l = three().remove(2).unwrap();
        assert_eq!(l.ids(), vec![1, 3]);
        assert_eq!(l.rects(AREA)[1].1, Rect { x: 400.0, y: 0.0, w: 400.0, h: 600.0 });
        assert_eq!(Layout::Leaf(7).remove(7), None);
    }

    #[test]
    fn resize_moves_the_nearest_matching_divider() {
        let mut l = three();
        // pane 3, Up: the horizontal divider between 2 and 3 moves up
        assert!(l.resize(3, Dir::Up, 60.0, AREA));
        assert_eq!(l.rects(AREA)[2].1.y, 240.0);
        // pane 3, Left: the vertical divider (the outer split) moves left
        assert!(l.resize(3, Dir::Left, 80.0, AREA));
        assert_eq!(l.rects(AREA)[0].1.w, 320.0);
    }

    #[test]
    fn pane_under_a_point() {
        assert_eq!(three().at(500.0, 450.0, AREA), Some(3));
        assert_eq!(three().at(10.0, 10.0, AREA), Some(1));
    }
}

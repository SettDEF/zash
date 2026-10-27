//! A small layout engine for the cell grid: columns and rows with gap, padding, fixed / fill / fit sizes
//! and cross alignment. Deterministic integer cells; a tree is laid out in one pass after one measure.

/// x, y, w, h in cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn new(x: usize, y: usize, w: usize, h: usize) -> Rect {
        Rect { x, y, w, h }
    }
    /// Shrunk by padding (left, top, right, bottom), never below zero.
    pub fn inset(self, p: [usize; 4]) -> Rect {
        let w = self.w.saturating_sub(p[0] + p[2]);
        let h = self.h.saturating_sub(p[1] + p[3]);
        Rect { x: self.x + p[0].min(self.w), y: self.y + p[1].min(self.h), w, h }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    /// exactly n cells (cut to the space there is)
    Fixed(usize),
    /// a share of what is left, by weight
    Fill(u16),
    /// what the content measures
    Fit,
    /// a fraction of the parent's inner size (a split: 0.25 = a quarter)
    Part(f32),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dir {
    #[default]
    Column,
    Row,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    /// the child takes the whole cross size
    Stretch,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub dir: Dir,
    pub w: Size,
    pub h: Size,
    pub gap: usize,
    pub pad: [usize; 4],
    pub align: Align,
    /// a leaf's own size (Fit)
    pub fit: (usize, usize),
    /// caps (0: none)
    pub max: (usize, usize),
    pub children: Vec<Node>,
}

impl Default for Node {
    fn default() -> Node {
        Node { dir: Dir::Column, w: Size::Fit, h: Size::Fit, gap: 0, pad: [0; 4], align: Align::Start, fit: (0, 0), max: (0, 0), children: Vec::new() }
    }
}

fn cap(v: usize, m: usize) -> usize {
    if m > 0 { v.min(m) } else { v }
}

impl Node {
    fn size(&self, across: bool) -> Size {
        if across { self.w } else { self.h }
    }

    /// What it measures when it fits its content: (w, h).
    pub fn measure(&self) -> (usize, usize) {
        let (pw, ph) = (self.pad[0] + self.pad[2], self.pad[1] + self.pad[3]);
        let (mut w, mut h) = if self.children.is_empty() { self.fit } else { (0, 0) };
        let row = self.dir == Dir::Row;
        for (i, c) in self.children.iter().enumerate() {
            let m = c.measure();
            let cw = if let Size::Fixed(n) = c.w { n } else { m.0 };
            let ch = if let Size::Fixed(n) = c.h { n } else { m.1 };
            let g = if i > 0 { self.gap } else { 0 };
            if row { w += cw + g; h = h.max(ch) } else { h += ch + g; w = w.max(cw) }
        }
        (cap(w + pw, self.max.0), cap(h + ph, self.max.1))
    }

    /// Every node's rect, in pre-order (the node first, then each child's subtree).
    pub fn layout(&self, r: Rect) -> Vec<Rect> {
        let mut out = Vec::new();
        self.place(r, &mut out);
        out
    }

    fn place(&self, r: Rect, out: &mut Vec<Rect>) {
        out.push(r);
        if self.children.is_empty() {
            return;
        }
        let inner = r.inset(self.pad);
        let across = self.dir == Dir::Row;
        let (main, cross) = if across { (inner.w, inner.h) } else { (inner.h, inner.w) };
        let gaps = self.gap * (self.children.len() - 1);
        // main sizes: fixed, part and fit first, then what is left shared by the fills
        let mut sizes: Vec<usize> = self.children.iter().map(|c| match c.size(across) {
            Size::Fixed(n) => n,
            Size::Part(f) => (main as f32 * f.clamp(0.0, 1.0)).round() as usize,
            Size::Fit => { let m = c.measure(); if across { m.0 } else { m.1 } }
            Size::Fill(_) => 0,
        }).collect();
        let used: usize = sizes.iter().sum::<usize>() + gaps;
        let weights: Vec<u32> = self.children.iter().map(|c| if let Size::Fill(k) = c.size(across) { k.max(1) as u32 } else { 0 }).collect();
        let total: u32 = weights.iter().sum();
        if total > 0 {
            let free = main.saturating_sub(used);
            let mut given = 0;
            for (s, k) in sizes.iter_mut().zip(&weights).filter(|(_, k)| **k > 0) {
                *s = free * *k as usize / total as usize;
                given += *s;
            }
            // the remainder one cell each, first fills first
            for (s, _) in sizes.iter_mut().zip(&weights).filter(|(_, k)| **k > 0).take(free - given) {
                *s += 1;
            }
        }
        let end = if across { inner.x + inner.w } else { inner.y + inner.h };
        let mut at = if across { inner.x } else { inner.y };
        for (i, (c, s)) in self.children.iter().zip(sizes).enumerate() {
            if i > 0 {
                at = (at + self.gap).min(end);
            }
            let cap_main = if across { c.max.0 } else { c.max.1 };
            let s = cap(s, cap_main).min(end - at);
            let want = match c.size(!across) {
                Size::Fixed(n) => n,
                Size::Fill(_) => cross,
                Size::Part(f) => (cross as f32 * f.clamp(0.0, 1.0)).round() as usize,
                Size::Fit => if self.align == Align::Stretch { cross } else { let m = c.measure(); if across { m.1 } else { m.0 } },
            };
            let cs = cap(want, if across { c.max.1 } else { c.max.0 }).min(cross);
            let off = match self.align {
                Align::Start | Align::Stretch => 0,
                Align::Center => (cross - cs) / 2,
                Align::End => cross - cs,
            };
            let cr = if across { Rect::new(at, inner.y + off, s, cs) } else { Rect::new(inner.x + off, at, cs, s) };
            c.place(cr, out);
            at += s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(w: usize, h: usize) -> Node {
        Node { fit: (w, h), ..Default::default() }
    }

    #[test]
    fn row_fixed_fill_fit() {
        // a 22-col sidebar, a fill, a fit of 5: 80 wide
        let n = Node { dir: Dir::Row, children: vec![Node { w: Size::Fixed(22), h: Size::Fill(1), ..leaf(0, 0) }, Node { w: Size::Fill(1), ..leaf(1, 1) }, leaf(5, 2)], ..Default::default() };
        let r = n.layout(Rect::new(0, 0, 80, 24));
        assert_eq!(r, vec![Rect::new(0, 0, 80, 24), Rect::new(0, 0, 22, 24), Rect::new(22, 0, 53, 1), Rect::new(75, 0, 5, 2)]);
    }

    #[test]
    fn column_gap_pad_and_weights() {
        let n = Node { gap: 1, pad: [2, 1, 2, 1], align: Align::Stretch, children: vec![leaf(3, 1), Node { h: Size::Fill(1), ..leaf(0, 0) }, Node { h: Size::Fill(2), ..leaf(0, 0) }], ..Default::default() };
        // inner 16x10; 1 + 2 gaps = 3 used, 7 free: 2 and 4, the remainder 1 to the first fill
        let r = n.layout(Rect::new(0, 0, 20, 12));
        assert_eq!(&r[1..], &[Rect::new(2, 1, 16, 1), Rect::new(2, 3, 16, 3), Rect::new(2, 7, 16, 4)]);
    }

    #[test]
    fn align_part_and_overflow() {
        let n = Node { dir: Dir::Row, align: Align::Center, children: vec![leaf(4, 2), Node { w: Size::Part(0.25), ..leaf(1, 1) }], ..Default::default() };
        let r = n.layout(Rect::new(0, 0, 40, 6));
        assert_eq!(&r[1..], &[Rect::new(0, 2, 4, 2), Rect::new(4, 2, 10, 1)]);
        // more than fits: cut at the end, later ones empty
        let n = Node { dir: Dir::Row, children: vec![leaf(8, 1), leaf(8, 1)], ..Default::default() };
        assert_eq!(&n.layout(Rect::new(0, 0, 10, 1))[1..], &[Rect::new(0, 0, 8, 1), Rect::new(8, 0, 2, 1)]);
        // nested measure: a fit column of two rows of gap 1
        let n = Node { gap: 1, pad: [1; 4], children: vec![leaf(3, 1), leaf(6, 1)], ..Default::default() };
        assert_eq!(n.measure(), (8, 5));
        // max caps a fill
        let n = Node { dir: Dir::Row, children: vec![Node { w: Size::Fill(1), max: (30, 0), ..leaf(0, 1) }], ..Default::default() };
        assert_eq!(n.layout(Rect::new(0, 0, 80, 1))[1], Rect::new(0, 0, 30, 1));
    }
}

//! Drawing on the terminal with a pen (pen.rs brings the pen): strokes kept per pane in cell units,
//! on the absolute line they were drawn at, so they scroll with the text and survive a new font
//! size; drawn as round dots in the rect pipeline (like fx's marks), over the text. A stroke begun
//! on a multiplexer's zfloat (zeit's floating pane) belongs to it: kept relative to the zfloat, it
//! moves with it, hides while it is closed and comes back with it, and does not scroll with the text
//! behind it.
//!
//! TODO (planned 2026-10-21): zeit's tiled panes the same way (zeit tells zash each pane's rect and
//! scroll, OSC 7733), so a stroke resizes, hides with its tab and scrolls with that pane's own
//! history; when a resize re-wraps the text, strokes follow their line (anchored to the text cell).

use crate::render::Rect;

/// Where a zfloat is now: its id and top-left cell on the screen.
pub type FloatAt = Option<(u64, usize, usize)>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// absolute line (history + screen row), fractional; on a zfloat: the row inside it
    pub line: f64,
    /// column, fractional; on a zfloat: the column inside it
    pub col: f32,
    /// pressure 0–1
    pub p: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub pts: Vec<Point>,
    pub color: [f32; 4],
    /// the widest it gets, in cells (full pressure)
    pub width: f32,
    /// the zfloat it was drawn on (its points are inside it), or None: on the pane's text
    pub anchor: Option<u64>,
}

impl Stroke {
    /// As it is on the screen now (absolute lines): a zfloat's stroke placed where the zfloat is,
    /// None while that zfloat is not shown.
    fn placed(&self, top: f64, float: FloatAt) -> Option<Stroke> {
        match self.anchor {
            None => Some(self.clone()),
            Some(id) => {
                let (fid, x, y) = float?;
                if fid != id {
                    return None;
                }
                let pts = self.pts.iter().map(|q| Point { line: top + y as f64 + q.line, col: x as f32 + q.col, p: q.p }).collect();
                Some(Stroke { pts, anchor: None, ..self.clone() })
            }
        }
    }
}
#[derive(Default, Debug)]
pub struct Ink {
    pub strokes: Vec<Stroke>,
    /// the stroke being drawn
    pub live: Option<Stroke>,
}

impl Ink {
    pub fn begin(&mut self, at: Point, color: [f32; 4], width: f32) {
        self.begin_on(None, at, color, width);
    }
    /// A stroke on zfloat `anchor` (its point inside the zfloat), or on the text (None).
    pub fn begin_on(&mut self, anchor: Option<u64>, at: Point, color: [f32; 4], width: f32) {
        self.end();
        self.live = Some(Stroke { pts: vec![at], color, width, anchor });
    }
    /// The zfloat the stroke being drawn belongs to.
    pub fn live_anchor(&self) -> Option<u64> {
        self.live.as_ref().and_then(|s| s.anchor)
    }
    /// The next point; ones closer than a tenth of a cell to the last are skipped.
    pub fn add(&mut self, at: Point) {
        if let Some(s) = &mut self.live {
            let l = *s.pts.last().unwrap();
            if ((at.line - l.line).abs() as f32) < 0.05 && (at.col - l.col).abs() < 0.1 {
                s.pts.last_mut().unwrap().p = at.p.max(l.p);
                return;
            }
            s.pts.push(at);
        }
    }
    pub fn end(&mut self) {
        if let Some(s) = self.live.take() {
            self.strokes.push(s);
        }
    }
    /// Takes away every stroke that passes within `r` cells of `at` (the eraser end; absolute, the
    /// screen's top at `top`, the zfloat where `float` says). True if any.
    pub fn erase(&mut self, at: Point, r: f32, top: f64, float: FloatAt) -> bool {
        let n = self.strokes.len();
        // in line units (a cell is about twice as tall as wide), to each segment, not only its ends
        let near = |a: &Point, b: &Point| {
            let (px, py) = (at.col * 0.5, at.line as f32);
            let (ax, ay, bx, by) = (a.col * 0.5, a.line as f32, b.col * 0.5, b.line as f32);
            let (dx, dy) = (bx - ax, by - ay);
            let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
            (ax + dx * t - px).hypot(ay + dy * t - py) < r
        };
        self.strokes.retain(|s| {
            let Some(s) = s.placed(top, float) else { return true };
            !(near(&s.pts[0], &s.pts[0]) || s.pts.windows(2).any(|w| near(&w[0], &w[1])))
        });
        self.strokes.len() != n
    }
    pub fn clear(&mut self) {
        self.strokes.clear();
        self.live = None;
    }
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.live.is_none()
    }
    /// Lines that left the scrollback take their strokes with them.
    #[allow(dead_code)] // for when graphics trims too (graphics.rs)
    pub fn trim(&mut self, oldest_line: f64) {
        self.strokes.retain(|s| s.anchor.is_some() || s.pts.iter().any(|q| q.line >= oldest_line));
    }

    /// The dots for what shows: `top` is the absolute line at the top of the screen, `rows` the
    /// screen's height; (ox, oy) the pane's origin, (cw, ch) a cell.
    #[allow(dead_code)] // the tests draw through it
    pub fn rects(&self, top: f64, rows: f64, float: FloatAt, ox: f32, oy: f32, cw: f32, ch: f32) -> Vec<Rect> {
        dots(&self.shown(top, rows, float), top, rows, ox, oy, cw, ch)
    }

    /// The strokes (the live one last) near the screen whose top is the absolute line `top`, in
    /// absolute lines; a zfloat's where `float` is now (none while it is closed).
    pub fn shown(&self, top: f64, rows: f64, float: FloatAt) -> Vec<Stroke> {
        self.strokes.iter().chain(self.live.iter()).filter_map(|s| s.placed(top, float)).filter(|s| s.pts.iter().any(|q| q.line >= top - 2.0 && q.line <= top + rows + 2.0)).collect()
    }
}

/// Round dots along each stroke, a third of a radius apart, their size following the pressure.
pub fn dots(strokes: &[Stroke], top: f64, rows: f64, ox: f32, oy: f32, cw: f32, ch: f32) -> Vec<Rect> {
    {
        let mut out = Vec::new();
        for s in strokes {
            if !s.pts.iter().any(|q| q.line >= top - 2.0 && q.line <= top + rows + 2.0) {
                continue;
            }
            let at = |q: &Point| (ox + q.col * cw, oy + (q.line - top) as f32 * ch);
            let rad = |p: f32| (s.width * ch * (0.18 + 0.82 * p) / 2.0).max(0.6);
            let mut dot = |x: f32, y: f32, r: f32| out.push(Rect { xywh: [x - r, y - r, 2.0 * r, 2.0 * r], color: s.color, p: [r, 0.9] });
            let first = &s.pts[0];
            let (x0, y0) = at(first);
            dot(x0, y0, rad(first.p));
            for w in s.pts.windows(2) {
                let ((ax, ay), (bx, by)) = (at(&w[0]), at(&w[1]));
                let (ra, rb) = (rad(w[0].p), rad(w[1].p));
                // dots a third of a radius apart: a solid line, its width following the pressure
                let len = (bx - ax).hypot(by - ay);
                let n = (len / (ra.min(rb) * 0.35).max(0.4)).ceil().clamp(1.0, 400.0) as usize;
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    dot(ax + (bx - ax) * t, ay + (by - ay) * t, ra + (rb - ra) * t);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(line: f64, col: f32) -> Point {
        Point { line, col, p: 0.5 }
    }

    #[test]
    fn a_stroke_scrolls_with_its_line() {
        let mut ink = Ink::default();
        ink.begin(pt(100.0, 2.0), [1.0; 4], 0.5);
        ink.add(pt(100.0, 6.0));
        ink.end();
        // line 100 at the top of the screen, then 10 lines further down the output
        let a = ink.rects(100.0, 30.0, None, 0.0, 0.0, 10.0, 20.0);
        let b = ink.rects(90.0, 30.0, None, 0.0, 0.0, 10.0, 20.0);
        assert!(!a.is_empty() && a.len() == b.len());
        assert!((b[0].xywh[1] - a[0].xywh[1] - 200.0).abs() < 0.01, "10 lines lower: 200 px down");
        // scrolled far away: nothing drawn
        assert!(ink.rects(500.0, 30.0, None, 0.0, 0.0, 10.0, 20.0).is_empty());
    }

    #[test]
    fn the_line_has_no_gaps_and_follows_pressure() {
        let mut ink = Ink::default();
        ink.begin(Point { line: 0.0, col: 0.0, p: 0.1 }, [1.0; 4], 0.5);
        ink.add(Point { line: 0.0, col: 20.0, p: 1.0 });
        ink.end();
        let r = ink.rects(0.0, 30.0, None, 0.0, 0.0, 10.0, 20.0);
        for w in r.windows(2) {
            let gap = (w[1].xywh[0] + w[1].xywh[2] / 2.0) - (w[0].xywh[0] + w[0].xywh[2] / 2.0);
            assert!(gap <= w[0].xywh[2] / 2.0, "dots overlap");
        }
        assert!(r.last().unwrap().xywh[2] > r[0].xywh[2] * 3.0, "harder is wider");
    }

    #[test]
    fn a_stroke_on_a_zfloat_goes_where_it_goes() {
        let mut ink = Ink::default();
        // drawn on zfloat 7 at its row 1, columns 2 to 6
        ink.begin_on(Some(7), pt(1.0, 2.0), [1.0; 4], 0.5);
        ink.add(pt(1.0, 6.0));
        ink.end();
        let at = |top: f64, f: FloatAt| ink.rects(top, 30.0, f, 0.0, 0.0, 10.0, 20.0).first().map(|r| (r.xywh[0] + r.xywh[2] / 2.0, r.xywh[1] + r.xywh[3] / 2.0));
        let (x1, y1) = at(100.0, Some((7, 10, 5))).unwrap();
        assert!((x1 - 120.0).abs() < 0.01 && (y1 - 120.0).abs() < 0.01, "column 10+2, row 5+1: {x1},{y1}");
        let (x2, y2) = at(100.0, Some((7, 20, 8))).unwrap();
        assert!((x2 - x1 - 100.0).abs() < 0.01 && (y2 - y1 - 60.0).abs() < 0.01, "the zfloat moved: so did the stroke");
        assert_eq!(at(140.0, Some((7, 10, 5))), Some((x1, y1)), "the text behind scrolled: the stroke stays on the zfloat");
        assert!(at(100.0, None).is_none() && at(100.0, Some((8, 10, 5))).is_none(), "closed (or another zfloat): not drawn");
        // the eraser finds it where the zfloat is now
        assert!(!ink.erase(pt(106.0, 14.0), 0.8, 100.0, Some((7, 30, 30))));
        assert!(ink.erase(pt(106.0, 14.0), 0.8, 100.0, Some((7, 10, 5))));
    }

    #[test]
    fn the_eraser_takes_whole_strokes_it_touches() {
        let mut ink = Ink::default();
        for line in [10.0, 20.0] {
            ink.begin(pt(line, 0.0), [1.0; 4], 0.5);
            ink.add(pt(line, 10.0));
            ink.end();
        }
        assert!(ink.erase(pt(10.2, 5.0), 0.8, 0.0, None));
        assert_eq!(ink.strokes.len(), 1);
        assert_eq!(ink.strokes[0].pts[0].line, 20.0);
        assert!(!ink.erase(pt(40.0, 5.0), 0.8, 0.0, None));
    }
}

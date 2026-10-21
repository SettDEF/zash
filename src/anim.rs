//! Lottie animations in the text (EffectCraft exports them): velato reads the file, vello_cpu draws
//! each frame on the CPU into a picture the image layer shows (graphics.rs), on a thread of its own
//! that stops when the animation is done or gone. Colours can be swapped for the theme's
//! (`color.ff0000=accent`), so an animation looks at home in every theme.
//!
//! OSC 7733;anim;file=/path.json;rows=4[;cols=20][;loop=0][;speed=1][;color.RRGGBB=name|RRGGBB]
//! (loop=0: forever; rows/cols in cells; the animation keeps its shape, centred).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::graphics::Image;
use velato::model::fixed;
use velato::{Composition, RenderSink, Renderer};
use vello_cpu::kurbo::{Affine, Shape};
use vello_cpu::peniko::{BlendMode, Brush, Color};
use vello_cpu::{Pixmap, RenderContext};

/// A running animation: its latest frame, for the image layer.
pub struct Anim {
    pub id: u32,
    pub frame: Mutex<Option<Image>>,
    pub generation: AtomicU64,
    done: AtomicBool,
}

/// What an escape asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct Ask {
    pub file: String,
    pub rows: usize,
    pub cols: Option<usize>,
    /// times to play; 0: forever
    pub loops: u32,
    pub speed: f64,
    /// colour (rgb) → its replacement
    pub swaps: Vec<([u8; 3], [u8; 3])>,
}

fn rgb(s: &str) -> Option<[u8; 3]> {
    let s = s.trim_start_matches('#');
    (s.len() == 6).then(|| [0, 2, 4].map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0)))
}

/// A theme colour by name (accent, fg, bg, red, green, yellow, blue, magenta, cyan), or hex.
pub fn theme_color(name: &str, p: &crate::config::Palette) -> Option<[u8; 3]> {
    let c = match name {
        "accent" | "cursor" => p.cursor_bg,
        "fg" => p.fg,
        "bg" => p.bg,
        "red" => p.ansi[1],
        "green" => p.ansi[2],
        "yellow" => p.ansi[3],
        "blue" => p.ansi[4],
        "magenta" => p.ansi[5],
        "cyan" => p.ansi[6],
        other => return rgb(other),
    };
    Some([c.0, c.1, c.2])
}

/// `file=…;rows=4;…` → what to play, theme names resolved with `p`.
pub fn parse(body: &str, p: &crate::config::Palette) -> Option<Ask> {
    let mut a = Ask { file: String::new(), rows: 3, cols: None, loops: 0, speed: 1.0, swaps: vec![] };
    for kv in body.split(';') {
        let Some((k, v)) = kv.split_once('=') else { continue };
        match k {
            "file" => a.file = v.to_string(),
            "rows" => a.rows = v.parse().ok()?,
            "cols" => a.cols = v.parse().ok(),
            "loop" => a.loops = v.parse().unwrap_or(0),
            "speed" => a.speed = v.parse::<f64>().unwrap_or(1.0).clamp(0.05, 20.0),
            k => {
                if let (Some(from), Some(to)) = (k.strip_prefix("color.").and_then(rgb), theme_color(v, p)) {
                    a.swaps.push((from, to));
                }
            }
        }
    }
    (!a.file.is_empty()).then(|| Ask { rows: a.rows.clamp(1, 200), ..a })
}

/// velato's drawing, onto vello_cpu, with the colour swaps.
struct Sink<'a> {
    ctx: &'a mut RenderContext,
    swaps: &'a [([u8; 3], [u8; 3])],
}

impl Sink<'_> {
    fn paint(&self, b: &fixed::Brush) -> Option<vello_cpu::PaintType> {
        match b {
            Brush::Solid(c) => {
                let [r, g, bl, a] = c.to_rgba8().to_u8_array();
                let near = |x: [u8; 3]| x.iter().zip([r, g, bl]).all(|(p, q)| p.abs_diff(q) <= 2);
                let out = self.swaps.iter().find(|(from, _)| near(*from)).map(|(_, to)| Color::from_rgba8(to[0], to[1], to[2], a)).unwrap_or(*c);
                Some(out.into())
            }
            Brush::Gradient(g) => Some(vello_cpu::PaintType::Gradient(g.clone())),
            Brush::Image(_) => None,
        }
    }
}

impl RenderSink for Sink<'_> {
    fn push_layer(&mut self, blend: impl Into<BlendMode>, alpha: f32, transform: Affine, shape: &impl Shape) {
        self.ctx.set_transform(transform);
        self.ctx.push_layer(Some(&shape.to_path(0.1)), Some(blend.into()), Some(alpha), None, None);
    }
    fn push_clip_layer(&mut self, transform: Affine, shape: &impl Shape) {
        self.ctx.set_transform(transform);
        self.ctx.push_clip_layer(&shape.to_path(0.1));
    }
    fn pop_layer(&mut self) {
        self.ctx.pop_layer();
    }
    fn draw(&mut self, stroke: Option<&fixed::Stroke>, transform: Affine, brush: &fixed::Brush, shape: &impl Shape) {
        let Some(paint) = self.paint(brush) else { return };
        self.ctx.set_transform(transform);
        self.ctx.set_paint(paint);
        let path = shape.to_path(0.1);
        match stroke {
            Some(s) => {
                self.ctx.set_stroke(s.clone());
                self.ctx.stroke_path(&path);
            }
            None => self.ctx.fill_path(&path),
        }
    }
    // pictures inside animations are not drawn (`zash anim check` says so)
    fn draw_image(&mut self, _: &velato::model::ImageAsset, _: Affine, _: f64) {}
}

/// The animation's frame `frame` in a `w`×`h` picture: its shape kept, centred.
pub fn render(c: &Composition, frame: f64, w: u16, h: u16, swaps: &[([u8; 3], [u8; 3])]) -> Image {
    let (w, h) = (w.max(1), h.max(1));
    let mut ctx = RenderContext::new(w, h);
    let s = (w as f64 / c.width as f64).min(h as f64 / c.height as f64);
    let fit = Affine::translate(((w as f64 - c.width as f64 * s) / 2.0, (h as f64 - c.height as f64 * s) / 2.0)) * Affine::scale(s);
    Renderer::new().append(c, frame, fit, 1.0, &mut Sink { ctx: &mut ctx, swaps });
    ctx.flush();
    let mut px = Pixmap::new(w, h);
    ctx.render(&mut px, &mut vello_cpu::Resources::new());
    let bytes = px.take_rgba8(vello_cpu::peniko::ImageAlphaType::Alpha);
    Image { w: w as u32, h: h as u32, rgba: Arc::new(bytes) }
}

/// What in a Lottie file will not play here (an empty list: all of it plays).
pub fn check(json: &serde_json::Value) -> Vec<String> {
    let mut out = vec![];
    let mut walk = |layers: &serde_json::Value, where_: &str| {
        for l in layers.as_array().into_iter().flatten() {
            let name = l["nm"].as_str().unwrap_or("?");
            match l["ty"].as_i64() {
                Some(5) => out.push(format!("{where_}layer {name:?}: text layers are not drawn (print the text, animate around it)")),
                Some(2) => out.push(format!("{where_}layer {name:?}: pictures are not drawn")),
                _ => {}
            }
            if l.get("ef").is_some_and(|e| e.as_array().is_some_and(|a| !a.is_empty())) {
                out.push(format!("{where_}layer {name:?}: effects are skipped"));
            }
            if l.to_string().contains("\"x\":\"") {
                out.push(format!("{where_}layer {name:?}: expressions are not run"));
            }
        }
    };
    walk(&json["layers"], "");
    for a in json["assets"].as_array().into_iter().flatten() {
        if a.get("layers").is_some() {
            walk(&a["layers"], &format!("asset {:?}, ", a["id"].as_str().unwrap_or("?")));
        }
    }
    out
}

pub fn load(path: &str) -> Result<Composition, String> {
    let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    Composition::from_slice(&data).map_err(|e| format!("{path}: {e}"))
}

impl Anim {
    /// Starts playing `c` into `w`×`h` pixels; `wake` asks the window for a frame.
    pub fn start(id: u32, c: Composition, w: u32, h: u32, ask: &Ask, wake: Box<dyn Fn() + Send + Sync>) -> Arc<Anim> {
        let a = Arc::new(Anim { id, frame: Mutex::new(None), generation: AtomicU64::new(0), done: AtomicBool::new(false) });
        let (w, h) = (w.clamp(1, 4096) as u16, h.clamp(1, 4096) as u16);
        let len = (c.frames.end - c.frames.start).max(1.0);
        let fps = c.frame_rate.max(1.0);
        let (loops, speed, swaps) = (ask.loops, ask.speed, ask.swaps.clone());
        // the clock: frozen captures (ZASH_FX_AT) show the frame at that time, from 0
        let start = if crate::fx::frozen().is_some() { 0.0 } else { crate::fx::now() };
        let at = move || {
            let t = (crate::fx::now() - start).max(0.0) as f64 * speed;
            let n = t * fps;
            let finished = loops > 0 && n >= len * loops as f64;
            let f = if finished { c.frames.end - 0.001 } else { c.frames.start + n % len };
            (f, finished)
        };
        let show = |a: &Anim, img: Image| {
            *a.frame.lock().unwrap() = Some(img);
            a.generation.fetch_add(1, Ordering::Relaxed);
        };
        // the first frame at once (a capture, or a slow first draw, still shows something)
        let (f0, _) = at();
        show(&a, render(&c, f0, w, h, &swaps));
        let weak = Arc::downgrade(&a);
        let _ = std::thread::Builder::new().name(format!("zash anim {id}")).spawn(move || {
            // at most 30 a second; still frames (a frozen clock, or the end) stop the thread
            let step = std::time::Duration::from_secs_f64(1.0 / fps.min(30.0));
            loop {
                std::thread::sleep(step);
                let Some(a) = weak.upgrade() else { return };
                if crate::fx::frozen().is_some() {
                    return;
                }
                let (f, finished) = at();
                show(&a, render(&c, f, w, h, &swaps));
                wake();
                if finished {
                    a.done.store(true, Ordering::Relaxed);
                    return;
                }
            }
        });
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: &str = include_str!("../tests/visual/anim-square.json");

    #[test]
    fn a_red_square_draws_and_takes_the_theme_colour() {
        let c = Composition::from_slice(SQUARE.as_bytes()).unwrap();
        let img = render(&c, 0.0, 64, 64, &[]);
        let px = |x: usize, y: usize| { let i = (y * 64 + x) * 4; [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3]] };
        assert_eq!(px(32, 32), [255, 0, 0, 255], "the middle is the square, red");
        assert_eq!(px(1, 1)[3], 0, "the corner is clear");
        let img = render(&c, 0.0, 64, 64, &[([255, 0, 0], [0x89, 0xb4, 0xfa])]);
        assert_eq!(&img.rgba[(32 * 64 + 32) * 4..(32 * 64 + 32) * 4 + 3], &[0x89, 0xb4, 0xfa], "red swapped for the accent");
    }

    #[test]
    fn it_turns_over_time() {
        let c = Composition::from_slice(SQUARE.as_bytes()).unwrap();
        // a quarter turn later the square's corner points the other way: the pixels differ
        assert_ne!(render(&c, 0.0, 64, 64, &[]).rgba, render(&c, 7.0, 64, 64, &[]).rgba);
    }

    #[test]
    fn asks_and_checks() {
        let p = crate::config::Palette::default();
        let a = parse("file=/x.json;rows=4;loop=2;color.ff0000=accent", &p).unwrap();
        assert_eq!((a.rows, a.loops, a.swaps.len()), (4, 2, 1));
        assert_eq!(a.swaps[0].1, theme_color("accent", &p).unwrap());
        assert!(parse("rows=4", &p).is_none(), "no file, nothing to play");
        let j: serde_json::Value = serde_json::from_str(r#"{"layers":[{"ty":5,"nm":"Title"},{"ty":4,"nm":"Shape"}]}"#).unwrap();
        let c = check(&j);
        assert_eq!(c.len(), 1);
        assert!(c[0].contains("Title"));
    }
}

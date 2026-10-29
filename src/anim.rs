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
    /// `at=SECONDS`: that one frame, held (a scrubber, a still from an animation)
    pub at: Option<f64>,
    /// `name=X`: a later escape with the same name replaces this one where it is
    pub name: Option<String>,
    /// `text.NAME=words`: the words drawn in the text layer named NAME (percent-encoded: %3B is ;)
    pub texts: Vec<(String, String)>,
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
    let mut a = Ask { file: String::new(), rows: 3, cols: None, loops: 0, speed: 1.0, swaps: vec![], at: None, name: None, texts: vec![] };
    for kv in body.split(';') {
        let Some((k, v)) = kv.split_once('=') else { continue };
        match k {
            "file" => a.file = v.to_string(),
            "rows" => a.rows = v.parse().ok()?,
            "cols" => a.cols = v.parse().ok(),
            "loop" => a.loops = v.parse().unwrap_or(0),
            "speed" => a.speed = v.parse::<f64>().unwrap_or(1.0).clamp(0.05, 20.0),
            "at" => a.at = v.parse::<f64>().ok().filter(|t| t.is_finite()).map(|t| t.max(0.0)),
            "name" => a.name = Some(v.chars().take(64).collect()),
            k if k.starts_with("text.") => a.texts.push((k[5..].to_string(), unpct(v).chars().take(400).collect())),
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
    render_with(c, frame, w, h, swaps, &[])
}

/// A frame with its text layers drawn (the program's words in the ones it named).
pub fn render_with(c: &Composition, frame: f64, w: u16, h: u16, swaps: &[([u8; 3], [u8; 3])], slots: &[Slot]) -> Image {
    let (w, h) = (w.max(1), h.max(1));
    let mut ctx = RenderContext::new(w, h);
    let s = (w as f64 / c.width as f64).min(h as f64 / c.height as f64);
    let fit = Affine::translate(((w as f64 - c.width as f64 * s) / 2.0, (h as f64 - c.height as f64 * s) / 2.0)) * Affine::scale(s);
    Renderer::new().append(c, frame, fit, 1.0, &mut Sink { ctx: &mut ctx, swaps });
    ctx.flush();
    let mut px = Pixmap::new(w, h);
    ctx.render(&mut px, &mut vello_cpu::Resources::new());
    let bytes = px.take_rgba8(vello_cpu::peniko::ImageAlphaType::Alpha);
    let mut img = Image { w: w as u32, h: h as u32, rgba: Arc::new(bytes) };
    draw_slots(&mut img, c, frame, slots);
    img
}

/// What in a Lottie file will not play here (an empty list: all of it plays).
pub fn check(json: &serde_json::Value) -> Vec<String> {
    let mut out = vec![];
    let mut walk = |layers: &serde_json::Value, where_: &str| {
        for l in layers.as_array().into_iter().flatten() {
            let name = l["nm"].as_str().unwrap_or("?");
            match l["ty"].as_i64() {
                Some(5) => out.push(format!("{where_}layer {name:?}: a text layer: zash draws it (position, scale, turn, fade; plain fill, linear easing); text.NAME= puts a program's words in it")),
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
    // velato refuses text layers: they go before it reads the file (zash draws them itself, `slots`)
    let data = match serde_json::from_slice::<serde_json::Value>(&data) {
        Ok(mut j) => {
            let strip = |l: &mut serde_json::Value| {
                if let Some(a) = l.as_array_mut() {
                    a.retain(|x| x["ty"].as_i64() != Some(5));
                }
            };
            strip(&mut j["layers"]);
            for a in j["assets"].as_array_mut().into_iter().flatten() {
                if a.get("layers").is_some() {
                    strip(&mut a["layers"]);
                }
            }
            serde_json::to_vec(&j).unwrap_or(data)
        }
        Err(_) => data,
    };
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
        let slots = slots(&ask.file, &ask.texts, &swaps);
        // a held frame: drawn once, no thread
        if let Some(t) = ask.at {
            let f = (c.frames.start + (t * speed * fps).min(len - 0.001)).min(c.frames.end - 0.001);
            *a.frame.lock().unwrap() = Some(render_with(&c, f, w, h, &swaps, &slots));
            a.generation.fetch_add(1, Ordering::Relaxed);
            a.done.store(true, Ordering::Relaxed);
            return a;
        }
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
        show(&a, render_with(&c, f0, w, h, &swaps, &slots));
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
                show(&a, render_with(&c, f, w, h, &swaps, &slots));
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

// ── SVG (VectorCraft): a still picture on the same escape, drawn once ──

/// An SVG file's size (its own width/height) and its source with the theme colours swapped in.
pub fn load_svg(path: &str, swaps: &[([u8; 3], [u8; 3])]) -> Result<(String, f32, f32), String> {
    let mut src = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    for (from, to) in swaps {
        let hex = |c: &[u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        for f in [hex(from), hex(from).to_uppercase().replacen("#", "#", 1)] {
            src = src.replace(&f, &hex(to));
        }
    }
    let tree = resvg::usvg::Tree::from_str(&src, &resvg::usvg::Options::default()).map_err(|e| format!("{path}: {e}"))?;
    let s = tree.size();
    Ok((src, s.width(), s.height()))
}

/// The SVG in a `w`×`h` picture: its shape kept, centred.
pub fn render_svg(src: &str, w: u32, h: u32) -> Option<Image> {
    let tree = resvg::usvg::Tree::from_str(src, &resvg::usvg::Options::default()).ok()?;
    let (w, h) = (w.clamp(1, 4096), h.clamp(1, 4096));
    let mut px = resvg::tiny_skia::Pixmap::new(w, h)?;
    let s = tree.size();
    let k = (w as f32 / s.width()).min(h as f32 / s.height());
    let t = resvg::tiny_skia::Transform::from_translate((w as f32 - s.width() * k) / 2.0, (h as f32 - s.height() * k) / 2.0).pre_scale(k, k);
    resvg::render(&tree, t, &mut px.as_mut());
    // tiny-skia keeps premultiplied alpha; the image layer takes straight
    let mut rgba = px.take();
    for p in rgba.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255) / a).min(255) as u8;
            }
        }
    }
    Some(Image { w, h, rgba: Arc::new(rgba) })
}

impl Anim {
    /// A still picture (an SVG) shown like an animation's frame.
    pub fn still(id: u32, img: Image) -> Arc<Anim> {
        Arc::new(Anim { id, frame: Mutex::new(Some(img)), generation: AtomicU64::new(1), done: AtomicBool::new(true) })
    }
}

// ── text slots: a Lottie's text layers drawn by zash (velato skips them), the program's words in a named one ──

/// A text layer: its name, its own words, its look and its transform (the JSON, read per frame).
#[derive(Clone, Debug)]
pub struct Slot {
    pub name: String,
    pub text: String,
    size: f64,
    font: String,
    /// 0 left, 1 right, 2 centre
    justify: i64,
    fill: [u8; 3],
    ks: serde_json::Value,
    ip: f64,
    op: f64,
}

/// A property at frame `f`: its value, or its keyframes eased linearly (Lottie's curves are not followed).
fn prop(v: &serde_json::Value, f: f64) -> Option<Vec<f64>> {
    let nums = |x: &serde_json::Value| -> Option<Vec<f64>> { x.as_f64().map(|n| vec![n]).or_else(|| x.as_array().map(|a| a.iter().filter_map(|n| n.as_f64()).collect())) };
    let k = v.get("k")?;
    let keys = k.as_array().filter(|a| a.first().is_some_and(|x| x.get("t").is_some()));
    let Some(keys) = keys else { return nums(k) };
    let at = |i: usize| keys.get(i).and_then(|x| x["t"].as_f64());
    let i = (0..keys.len()).rev().find(|&i| at(i).is_some_and(|t| t <= f)).unwrap_or(0);
    let a = nums(&keys[i]["s"])?;
    let (Some(t0), Some(t1)) = (at(i), at(i + 1)) else { return Some(a) };
    let b = nums(&keys[i + 1]["s"]).or_else(|| nums(&keys[i]["e"]))?;
    if f <= t0 || t1 <= t0 {
        return Some(a);
    }
    let u = ((f - t0) / (t1 - t0)).clamp(0.0, 1.0);
    Some(a.iter().zip(&b).map(|(x, y)| x + (y - x) * u).collect())
}

/// The text layers of a Lottie file (top level), with the program's words put into the ones it names.
pub fn slots(path: &str, words: &[(String, String)], swaps: &[([u8; 3], [u8; 3])]) -> Vec<Slot> {
    let Ok(json) = std::fs::read(path).map_err(|_| ()).and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).map_err(|_| ())) else { return vec![] };
    json["layers"].as_array().into_iter().flatten().filter(|l| l["ty"].as_i64() == Some(5)).map(|l| {
        let d = &l["t"]["d"]["k"][0]["s"];
        let name = l["nm"].as_str().unwrap_or("").to_string();
        let mut fill = [0, 2, 1].map(|_| 255u8);
        if let Some(fc) = d["fc"].as_array() {
            for (i, c) in fc.iter().take(3).enumerate() {
                fill[i] = (c.as_f64().unwrap_or(1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        if let Some((_, to)) = swaps.iter().find(|(from, _)| *from == fill) {
            fill = *to;
        }
        let own = d["t"].as_str().unwrap_or("").replace('\r', "\n");
        Slot {
            text: words.iter().find(|(n, _)| *n == name).map(|(_, w)| w.clone()).unwrap_or(own),
            name,
            size: d["s"].as_f64().unwrap_or(24.0),
            font: d["f"].as_str().unwrap_or("sans-serif").to_string(),
            justify: d["j"].as_i64().unwrap_or(0),
            fill,
            ks: l["ks"].clone(),
            ip: l["ip"].as_f64().unwrap_or(f64::MIN),
            op: l["op"].as_f64().unwrap_or(f64::MAX),
        }
    }).collect()
}

fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: std::sync::OnceLock<Arc<resvg::usvg::fontdb::Database>> = std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        // the generic names as fontconfig means them here (fontdb guesses otherwise)
        let fc = |g: &str| std::process::Command::new("fc-match").args(["-f", "%{family[0]}", g]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty());
        if let Some(f) = fc("sans-serif") { db.set_sans_serif_family(f) }
        if let Some(f) = fc("serif") { db.set_serif_family(f) }
        if let Some(f) = fc("monospace") { db.set_monospace_family(f) }
        Arc::new(db)
    }).clone()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The slots' words at frame `f` over `img` (the composition fitted into it as `render` fits it).
fn draw_slots(img: &mut Image, c: &Composition, f: f64, slots: &[Slot]) {
    let live: Vec<&Slot> = slots.iter().filter(|s| f >= s.ip && f < s.op && !s.text.is_empty()).collect();
    if live.is_empty() {
        return;
    }
    let (w, h) = (img.w as f64, img.h as f64);
    let k = (w / c.width as f64).min(h / c.height as f64);
    let (dx, dy) = ((w - c.width as f64 * k) / 2.0, (h - c.height as f64 * k) / 2.0);
    let mut body = String::new();
    for s in live {
        let g = |n: &str, d: Vec<f64>| prop(&s.ks[n], f).unwrap_or(d);
        let (p, a, sc, r, o) = (g("p", vec![0.0, 0.0]), g("a", vec![0.0, 0.0]), g("s", vec![100.0, 100.0]), g("r", vec![0.0]), g("o", vec![100.0]));
        let at = |v: &Vec<f64>, i: usize| v.get(i).copied().unwrap_or(0.0);
        // each run of one font its own <text>, placed after the one before (usvg would draw a whole line in a
        // fallback font when one character needs it); the line as wide as its runs for the justification
        let mut lines = String::new();
        for (i, l) in s.text.lines().enumerate() {
            let parts = runs(&s.font, l);
            let widths: Vec<f64> = parts.iter().map(|(f, t)| run_width(f, s.size, t)).collect();
            let total: f64 = widths.iter().sum();
            let mut x = match s.justify { 1 => -total, 2 => -total / 2.0, _ => 0.0 };
            let y = i as f64 * s.size * 1.2;
            for ((f, t), wd) in parts.iter().zip(&widths) {
                lines.push_str(&format!("<text x=\"{x}\" y=\"{y}\" font-family=\"{}\" xml:space=\"preserve\">{}</text>", esc(f), esc(t)));
                x += wd;
            }
        }
        body.push_str(&format!(
            "<g transform=\"translate({} {}) rotate({}) scale({} {}) translate({} {})\" font-size=\"{}\" fill=\"#{:02x}{:02x}{:02x}\" fill-opacity=\"{}\">{lines}</g>",
            at(&p, 0), at(&p, 1), at(&r, 0), at(&sc, 0) / 100.0, at(&sc, 1) / 100.0, -at(&a, 0), -at(&a, 1),
            s.size, s.fill[0], s.fill[1], s.fill[2], (at(&o, 0) / 100.0).clamp(0.0, 1.0)
        ));
    }
    let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\"><g transform=\"translate({dx} {dy}) scale({k})\">{body}</g></svg>");
    let opt = resvg::usvg::Options { fontdb: fonts(), ..Default::default() };
    let Ok(tree) = resvg::usvg::Tree::from_str(&svg, &opt) else { return };
    let Some(mut px) = resvg::tiny_skia::Pixmap::new(img.w, img.h) else { return };
    resvg::render(&tree, resvg::tiny_skia::Transform::identity(), &mut px.as_mut());
    // the words (premultiplied) over the frame (straight alpha)
    let dst = Arc::make_mut(&mut img.rgba);
    for (d, s) in dst.chunks_exact_mut(4).zip(px.data().chunks_exact(4)) {
        let sa = s[3] as f32 / 255.0;
        if sa == 0.0 {
            continue;
        }
        let da = d[3] as f32 / 255.0;
        let oa = sa + da * (1.0 - sa);
        for i in 0..3 {
            let v = s[i] as f32 + d[i] as f32 * da * (1.0 - sa);
            d[i] = (v / oa).round().clamp(0.0, 255.0) as u8;
        }
        d[3] = (oa * 255.0).round() as u8;
    }
}

/// %XX in an escape's value (so words can hold ; and =).
fn unpct(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The family fontconfig draws `c` with when `family` is asked for (itself when it has the glyph); asked once per pair.
fn family_for(family: &str, c: char) -> String {
    static SEEN: std::sync::OnceLock<Mutex<std::collections::HashMap<(String, char), String>>> = std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Some(f) = seen.lock().unwrap().get(&(family.to_string(), c)) {
        return f.clone();
    }
    let pat = format!("{family}:charset={:x}", c as u32);
    let f = std::process::Command::new("fc-match").args(["-f", "%{family[0]}", &pat]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| family.to_string());
    seen.lock().unwrap().insert((family.to_string(), c), f.clone());
    f
}


/// A line cut into runs of characters one font draws (family, text): a symbol the font lacks gets its own.
fn runs(family: &str, line: &str) -> Vec<(String, String)> {
    let base = family_for(family, 'a');
    let mut out: Vec<(String, String)> = Vec::new();
    for c in line.chars() {
        let f = if c.is_ascii() { base.clone() } else { family_for(family, c) };
        match out.last_mut() {
            Some((cf, s)) if *cf == f => s.push(c),
            _ => out.push((f, c.to_string())),
        }
    }
    out
}

/// How far `text` moves the pen in `family` at `size` (usvg's layout): its box with an `l` after it, less the `l`'s
/// (a box alone leaves out trailing spaces). Kept per text.
fn run_width(family: &str, size: f64, text: &str) -> f64 {
    static SEEN: std::sync::OnceLock<Mutex<std::collections::HashMap<(String, u64, String), f64>>> = std::sync::OnceLock::new();
    let key = (family.to_string(), size.to_bits(), text.to_string());
    let seen = SEEN.get_or_init(Default::default);
    if let Some(w) = seen.lock().unwrap().get(&key) {
        return *w;
    }
    let right = |t: &str| -> f64 {
        let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"><text x=\"0\" y=\"{size}\" font-family=\"{}\" font-size=\"{size}\" xml:space=\"preserve\">{}</text></svg>", esc(family), esc(t));
        let opt = resvg::usvg::Options { fontdb: fonts(), ..Default::default() };
        resvg::usvg::Tree::from_str(&svg, &opt).ok().map(|tr| { let b = tr.root().abs_bounding_box(); (b.x() + b.width()) as f64 }).unwrap_or(0.0)
    };
    let w = (right(&format!("{text}l")) - right("l")).max(0.0);
    seen.lock().unwrap().insert(key, w);
    w
}

#[cfg(test)]
mod slot_tests {
    use super::*;

    #[test]
    fn keyframes_ease_linearly_and_hold_at_the_ends() {
        let v: serde_json::Value = serde_json::json!({ "a": 1, "k": [{ "t": 0, "s": [0, 10] }, { "t": 10, "s": [100, 10] }] });
        assert_eq!(prop(&v, 5.0), Some(vec![50.0, 10.0]));
        assert_eq!(prop(&v, -3.0), Some(vec![0.0, 10.0]));
        assert_eq!(prop(&v, 99.0), Some(vec![100.0, 10.0]));
        assert_eq!(prop(&serde_json::json!({ "k": 42 }), 3.0), Some(vec![42.0]));
    }

    #[test]
    fn a_named_text_layer_takes_the_programs_words() {
        let s = slots("tests/visual/anim-title.json", &[("title".into(), "Hi".into())], &[]);
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].name.as_str(), s[0].text.as_str(), s[0].size), ("title", "Hi", 40.0));
        // no words given: its own
        assert_eq!(slots("tests/visual/anim-title.json", &[], &[])[0].text, "Placeholder");
        // the file loads for velato (its text layer taken out first)
        assert!(load("tests/visual/anim-title.json").is_ok());
        assert_eq!(parse("file=x.json;text.title=a%3Bb", &crate::config::Palette::default()).unwrap().texts, vec![("title".to_string(), "a;b".to_string())]);
    }
}

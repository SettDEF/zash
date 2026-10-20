//! Text effects (OSC 7733;fx): shimmer, pulse, fade, rainbow, typewriter, glow on a run of text
//! or its background, each with its own parameters. A run carries its effect as a hyperlink with
//! a `zash-fx:` address, so it stays on its cells through scrolling, scrollback and resizing.
//!
//! ```text
//! ESC ] 7733 ; fx ; shimmer ; target=bg ; speed=1.2 ; color=accent ; loop=2 ST   text   ESC ] 7733 ; fx ; end ST
//! ESC ] 7733 ; fx ; done ST   text   ESC ] 7733 ; fx ; end ST          (a preset from [fx.presets])
//! ```
//! Unknown keys and values are ignored, so programs written for a newer zash still work here.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use crate::config::Rgb;

/// The hyperlink scheme effects travel in (never opened as a link).
pub const SCHEME: &str = "zash-fx:";

/// `[fx]` in zash.toml.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// false: effects show as plain text
    pub enabled: bool,
    /// false: zfloats keep their plain panel, no GLSL ([zfloat.shaders] is ignored)
    pub shaders: bool,
    /// the most frames a second effects ask for (old machines: lower)
    pub fps: f32,
    /// name → the same text an escape carries (`shimmer;color=good;loop=2`)
    pub presets: HashMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, shaders: true, fps: 30.0, presets: HashMap::new() }
    }
}

/// What an `fx;…` escape body asks for.
#[derive(Debug, PartialEq)]
pub enum Escape {
    /// the address of a new run's effect
    Start(String),
    End,
}

impl Escape {
    /// `NAME;key=value;…` starts a run, `end` (or nothing) ends it.
    pub fn parse(body: &str) -> Escape {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let body = body.trim();
        if body.is_empty() || body == "end" {
            return Escape::End;
        }
        // each run its own id, so two runs next to each other stay two runs
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Escape::Start(format!("{SCHEME}{id};{};{body}", now_ms()))
    }
}

/// Seconds on the clock effects share (from zash's start).
pub fn now() -> f32 {
    epoch().elapsed().as_secs_f32()
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

fn epoch() -> Instant {
    static E: OnceLock<Instant> = OnceLock::new();
    *E.get_or_init(Instant::now)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// a bright band sweeping across
    Shimmer,
    /// the whole run breathing towards the colour
    Pulse,
    /// fading in
    Fade,
    /// hues running along the text
    Rainbow,
    /// characters appearing one by one
    Typewriter,
    /// a soft halo behind the run, breathing
    Glow,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Text,
    Background,
    Both,
}

/// A colour: one of the theme's roles (it follows the palette) or a fixed one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    /// the palette tone: a(ccent), g(ood), b(ad), r(unning), m(uted), f(oreground)
    Role(char),
    Fixed(Rgb),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Life {
    Cycles(f32),
    Seconds(f32),
    Forever,
}

/// One run's effect, read from its address.
#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    pub kind: Kind,
    pub target: Target,
    /// 1 = the effect's own pace
    pub speed: f32,
    /// 0..1: how far towards the colour
    pub intensity: f32,
    pub color: Paint,
    /// shimmer: the band's half width, in cells; rainbow: cells per full hue turn
    pub width: f32,
    /// seconds before it starts (fade and typewriter keep the text hidden meanwhile)
    pub delay: f32,
    life: Life,
    /// seconds on the shared clock when the run began
    start: f32,
}

/// What one cell of a run looks like at a moment. `None`: as it is without the effect.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    /// the text colour (alpha 0: not drawn)
    pub fg: Option<[f32; 4]>,
    pub bg: Option<[f32; 4]>,
    /// a soft halo behind the run (glow)
    pub halo: Option<[f32; 4]>,
}

impl Effect {
    /// Reads an address (`zash-fx:ID;START;NAME;key=value…`). A preset's own parameters come
    /// first, the escape's on top; an unknown effect name gives nothing (plain text).
    pub fn from_address(addr: &str, s: &Settings) -> Option<Effect> {
        let rest = addr.strip_prefix(SCHEME)?;
        let mut parts = rest.split(';');
        let _id = parts.next()?;
        let start = parts.next()?.parse::<u64>().ok()? as f32 / 1000.0;
        let name = parts.next()?.trim();
        let own: Vec<&str> = parts.collect();
        let (kind_name, preset): (&str, Vec<&str>) = match s.presets.get(name) {
            Some(p) => {
                let mut it = p.split(';');
                (it.next().unwrap_or("").trim(), it.collect())
            }
            None => (name, Vec::new()),
        };
        let kind = match kind_name {
            "shimmer" => Kind::Shimmer,
            "pulse" => Kind::Pulse,
            "fade" => Kind::Fade,
            "rainbow" => Kind::Rainbow,
            "typewriter" => Kind::Typewriter,
            "glow" => Kind::Glow,
            _ => return None,
        };
        let once = matches!(kind, Kind::Fade | Kind::Typewriter);
        let mut e = Effect {
            kind,
            target: if kind == Kind::Glow { Target::Background } else { Target::Text },
            speed: 1.0,
            intensity: if kind == Kind::Rainbow { 1.0 } else { 0.8 },
            color: Paint::Role('a'),
            width: if kind == Kind::Rainbow { 24.0 } else { 4.0 },
            delay: 0.0,
            life: Life::Cycles(if once { 1.0 } else { 2.0 }),
            start,
        };
        for kv in preset.into_iter().chain(own) {
            let Some((k, v)) = kv.split_once('=') else { continue };
            e.set(k.trim(), v.trim());
        }
        Some(e)
    }

    /// One parameter; a value that does not read is ignored (the default stays).
    fn set(&mut self, k: &str, v: &str) {
        match k {
            "target" => match v {
                "text" | "fg" => self.target = Target::Text,
                "bg" | "background" => self.target = Target::Background,
                "both" => self.target = Target::Both,
                _ => {}
            },
            "speed" => self.speed = num(v).filter(|s| *s > 0.0).unwrap_or(self.speed),
            "intensity" => self.intensity = num(v).map(|i| i.clamp(0.0, 1.0)).unwrap_or(self.intensity),
            "width" => self.width = num(v).filter(|w| *w > 0.0).unwrap_or(self.width),
            "delay" => self.delay = secs(v).unwrap_or(self.delay),
            "color" => self.color = paint(v).unwrap_or(self.color),
            "loop" => self.life = if v == "forever" { Life::Forever } else { num(v).filter(|n| *n > 0.0).map(Life::Cycles).unwrap_or(self.life) },
            "duration" => self.life = if v == "forever" { Life::Forever } else { secs(v).filter(|d| *d > 0.0).map(Life::Seconds).unwrap_or(self.life) },
            _ => {}
        }
    }

    pub fn styles_text(&self) -> bool {
        matches!(self.target, Target::Text | Target::Both)
    }

    pub fn styles_background(&self) -> bool {
        matches!(self.target, Target::Background | Target::Both)
    }

    /// One cycle in seconds, for a run of `n` cells.
    fn cycle(&self, n: usize) -> f32 {
        let base = match self.kind {
            Kind::Shimmer => (n as f32 + 2.0 * self.width) / 24.0,
            Kind::Pulse => 1.2,
            Kind::Fade => 0.6,
            Kind::Rainbow => 2.0,
            Kind::Typewriter => n as f32 / 40.0,
            Kind::Glow => 1.6,
        };
        base.max(0.05) / self.speed
    }

    /// Still moving at `now` (a run of `n` cells)? Afterwards it is drawn as plain text.
    pub fn live(&self, now: f32, n: usize) -> bool {
        let t = now - self.start - self.delay;
        match self.life {
            Life::Forever => true,
            Life::Cycles(c) => t < c * self.cycle(n),
            Life::Seconds(s) => t < s,
        }
    }

    /// Cell `i` of a run of `n` at `now`, over these colours. `tone` resolves palette roles.
    pub fn look(&self, now: f32, i: usize, n: usize, fg: [f32; 4], bg: [f32; 4], tone: impl Fn(char) -> Rgb) -> Look {
        let t = now - self.start - self.delay;
        let color = match self.color {
            Paint::Role(r) => tone(r).f32(),
            Paint::Fixed(c) => c.f32(),
        };
        let k = self.intensity;
        let cyc = self.cycle(n);
        // 0..1: how far this cell is towards the colour (or, for fade/typewriter, shown)
        let f = if t < 0.0 {
            0.0
        } else {
            match self.kind {
                Kind::Shimmer => {
                    let pos = (t / cyc).fract() * (n as f32 + 2.0 * self.width) - self.width;
                    (1.0 - (i as f32 + 0.5 - pos).abs() / self.width).max(0.0)
                }
                // starts and ends at rest, so a finished effect never jumps
                Kind::Pulse | Kind::Glow => (1.0 - (t / cyc * std::f32::consts::TAU).cos()) / 2.0,
                Kind::Fade => (t / cyc).clamp(0.0, 1.0),
                Kind::Typewriter => ((t / cyc * n as f32) > i as f32) as u8 as f32,
                Kind::Rainbow => 1.0,
            }
        };
        let mut out = Look::default();
        match self.kind {
            Kind::Shimmer | Kind::Pulse => {
                if self.styles_text() {
                    out.fg = Some(mix(fg, color, f * k));
                }
                if self.styles_background() {
                    out.bg = Some(mix(bg, color, f * k));
                }
            }
            Kind::Fade | Kind::Typewriter => {
                if self.styles_text() {
                    out.fg = Some([fg[0], fg[1], fg[2], fg[3] * f]);
                }
                if self.styles_background() {
                    out.bg = Some([bg[0], bg[1], bg[2], bg[3] * f]);
                }
            }
            Kind::Rainbow => {
                let hue = (i as f32 / self.width + t / cyc).fract();
                let c = hsv(hue, 0.6, 1.0);
                if self.styles_text() {
                    out.fg = Some(mix(fg, c, k));
                }
                if self.styles_background() {
                    out.bg = Some(mix(bg, c, k * 0.5));
                }
            }
            Kind::Glow => {
                out.halo = Some([color[0], color[1], color[2], 0.45 * k * (0.35 + 0.65 * f)]);
                if self.styles_text() {
                    out.fg = Some(mix(fg, color, 0.3 * f * k));
                }
            }
        }
        out
    }
}

fn num(v: &str) -> Option<f32> {
    v.parse::<f32>().ok().filter(|n| n.is_finite())
}

/// `800ms`, `1.5s` or a plain number of seconds.
fn secs(v: &str) -> Option<f32> {
    match v.strip_suffix("ms") {
        Some(ms) => num(ms).map(|m| m / 1000.0),
        None => num(v.strip_suffix('s').unwrap_or(v)),
    }
    .filter(|s| *s >= 0.0)
}

/// A theme role or `#rrggbb`.
fn paint(v: &str) -> Option<Paint> {
    let role = match v {
        "accent" => 'a',
        "good" => 'g',
        "bad" => 'b',
        "running" => 'r',
        "muted" => 'm',
        "fg" | "foreground" => 'f',
        _ => {
            let h = v.strip_prefix('#').filter(|h| h.len() == 6)?;
            let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
            return Some(Paint::Fixed(Rgb(c(0)?, c(2)?, c(4)?)));
        }
    };
    Some(Paint::Role(role))
}

fn mix(a: [f32; 4], b: [f32; 4], f: f32) -> [f32; 4] {
    let f = f.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f, a[3]]
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 4] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff(body: &str, s: &Settings) -> Effect {
        let Escape::Start(addr) = Escape::parse(body) else { panic!("an end") };
        Effect::from_address(&addr, s).expect("an effect")
    }

    #[test]
    fn parameters_presets_and_unknowns() {
        let mut s = Settings::default();
        s.presets.insert("done".into(), "shimmer;color=good;speed=2".into());
        let e = eff("done;speed=3;target=both;color=#ff8000;nonsense=1;width=x", &s);
        assert_eq!(e.kind, Kind::Shimmer);
        assert_eq!(e.speed, 3.0); // the escape's own beats the preset's
        assert_eq!(e.target, Target::Both);
        assert_eq!(e.color, Paint::Fixed(Rgb(255, 128, 0)));
        assert_eq!(e.width, 4.0); // a bad value keeps the default
        assert_eq!(eff("glow", &s).target, Target::Background);
        assert_eq!(eff("fade;delay=250ms", &s).delay, 0.25);
        let Escape::Start(a) = Escape::parse("sparkle") else { panic!() };
        assert!(Effect::from_address(&a, &s).is_none());
        assert_eq!(Escape::parse("end"), Escape::End);
    }

    #[test]
    fn effects_end_where_they_began() {
        let s = Settings::default();
        let tone = |_| Rgb(255, 255, 255);
        let (fg, bg) = ([0.5, 0.5, 0.5, 1.0], [0.0, 0.0, 0.0, 1.0]);
        let e = eff("pulse;loop=2", &s);
        let end = e.start + 2.0 * e.cycle(5);
        assert!(e.live(end - 0.01, 5) && !e.live(end + 0.01, 5));
        // a pulse at the end of its cycles is back at the text's own colour
        let at_end = e.look(end, 0, 5, fg, bg, tone).fg.unwrap();
        assert!((at_end[0] - 0.5).abs() < 0.01);
        // typewriter: the first character shows before the last
        let t = eff("typewriter", &s);
        let mid = t.start + t.cycle(10) / 2.0;
        assert_eq!(t.look(mid, 0, 10, fg, bg, tone).fg.unwrap()[3], 1.0);
        assert_eq!(t.look(mid, 9, 10, fg, bg, tone).fg.unwrap()[3], 0.0);
        assert!(eff("rainbow;loop=forever", &s).live(1e6, 3));
    }
}

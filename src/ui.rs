//! UI hints (OSC 7733;ui): a program says which cells form a panel or a big heading, zash draws
//! them (render.rs), and every other terminal shows the plain cells the program printed anyway.
//!
//! OSC 7733;ui;REGION;REGION… BEL — the whole set for the screen, sent again on each redraw;
//! OSC 7733;ui BEL clears it. Cells are the pane's, at the time of the escape: the set scrolls
//! with the text (and is only shown on the screen it was sent on, main or alternate).
//! REGION = KIND,X,Y,W,H[,key=value]… (values percent-encoded: %2C , %3B ; %25 %)
//!   panel,…[,role=surface-container][,r=12][,border=outline]   a rounded rectangle behind the cells
//!   text,…,text=Inbox[,scale=2][,weight=bold][,role=on-surface] the words drawn that size over the
//!     rect (left, centred in its height); the cells under it are not drawn
//! Unknown kinds and keys are skipped (button, list, input come later).

use crate::config::{Palette, Rgb};

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Panel { radius: f32, border: Option<String> },
    Text { text: String, scale: f32, bold: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub kind: Kind,
    /// x, y, w, h in cells
    pub rect: [usize; 4],
    /// a theme colour name (role), or hex
    pub role: String,
}

/// A set as sent: its regions, the absolute line of the screen's top then, and which screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiSet {
    pub regions: Vec<Region>,
    pub top: usize,
    pub alt: bool,
}

fn unescape(s: &str) -> String {
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

/// The regions of an escape's body (after `7733;ui;`); bad ones are skipped.
pub fn parse(body: &str) -> Vec<Region> {
    body.split(';').filter_map(|r| {
        let mut f = r.split(',');
        let kind = f.next()?.trim();
        let n: Vec<usize> = f.by_ref().take(4).filter_map(|x| x.trim().parse().ok()).collect();
        if n.len() != 4 || n[2] == 0 || n[3] == 0 || n[2] > 1000 || n[3] > 1000 {
            return None;
        }
        let kv: Vec<(&str, String)> = f.filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim(), unescape(v))).collect();
        let get = |k: &str| kv.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        let (kind, role) = match kind {
            "panel" => (Kind::Panel { radius: get("r").and_then(|v| v.parse().ok()).unwrap_or(10.0f32).clamp(0.0, 64.0), border: get("border") }, get("role").unwrap_or("surface-container".into())),
            "text" => (Kind::Text { text: get("text")?, scale: get("scale").and_then(|v| v.parse().ok()).unwrap_or(2.0f32).clamp(0.5, 6.0), bold: get("weight").is_some_and(|w| w == "bold") }, get("role").unwrap_or("on-surface".into())),
            _ => return None,
        };
        Some(Region { kind, rect: [n[0], n[1], n[2], n[3]], role })
    }).take(256).collect()
}

fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// A Material role from zash's palette (surface steps are the background lifted toward the text),
/// else the anim escape's names (accent, fg, red…) or hex.
pub fn role(name: &str, p: &Palette) -> Option<Rgb> {
    let (bg, fg, acc) = (p.bg, p.fg, p.cursor_bg);
    Some(match name {
        "surface" | "background" => bg,
        "surface-container-lowest" => mix(bg, Rgb(0, 0, 0), 0.25),
        "surface-container-low" => mix(bg, fg, 0.03),
        "surface-container" => mix(bg, fg, 0.06),
        "surface-container-high" => mix(bg, fg, 0.09),
        "surface-container-highest" | "surface-variant" => mix(bg, fg, 0.13),
        "on-surface" | "on-background" => fg,
        "on-surface-variant" => mix(fg, bg, 0.3),
        "outline" => mix(bg, fg, 0.4),
        "outline-variant" => mix(bg, fg, 0.2),
        "primary" => acc,
        "on-primary" => p.cursor_fg,
        "primary-container" => mix(bg, acc, 0.3),
        "on-primary-container" => mix(fg, acc, 0.4),
        "secondary" => p.ansi[6],
        "tertiary" => p.ansi[5],
        "error" => p.ansi[1],
        other => crate::anim::theme_color(other, p).map(|c| Rgb(c[0], c[1], c[2]))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_parse() {
        let r = parse("panel,0,0,20,24,role=surface-container-high,r=12,border=outline;text,22,1,30,2,scale=2.5,weight=bold,text=Hello%2C%20world%3B");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], Region { kind: Kind::Panel { radius: 12.0, border: Some("outline".into()) }, rect: [0, 0, 20, 24], role: "surface-container-high".into() });
        assert_eq!(r[1].kind, Kind::Text { text: "Hello, world;".into(), scale: 2.5, bold: true });
        assert_eq!(r[1].role, "on-surface");
        // defaults; empty, zero-size, unknown and text without words skipped
        assert_eq!(parse("panel,1,2,3,4")[0].kind, Kind::Panel { radius: 10.0, border: None });
        assert!(parse("").is_empty());
        assert!(parse("panel,0,0,0,4;button,0,0,4,1;text,0,0,4,1;panel,1,x,3,4").is_empty());
        // a stray % stays
        assert_eq!(parse("text,0,0,4,1,text=50%")[0].kind, Kind::Text { text: "50%".into(), scale: 2.0, bold: false });
    }

    #[test]
    fn roles_resolve() {
        let p = Palette::default();
        assert_eq!(role("surface", &p), Some(p.bg));
        assert_eq!(role("primary", &p), Some(p.cursor_bg));
        assert_eq!(role("red", &p), Some(p.ansi[1]));
        assert_eq!(role("ff0000", &p), Some(Rgb(255, 0, 0)));
        assert_eq!(role("nope", &p), None);
        // containers step up from the background
        let l = |n| { let c = role(n, &p).unwrap(); c.0 as u32 + c.1 as u32 + c.2 as u32 };
        assert!(l("surface") < l("surface-container-low") && l("surface-container") < l("surface-container-highest"));
    }
}

/// `zash ui demo`: a mock app screen (a sidebar, a heading, cards) in plain cells, and the hints
/// that make zash draw it as a UI (`plain`: the cells only, as another terminal shows them).
pub fn demo(plain: bool) -> String {
    let mut s = String::from("\x1b[H\x1b[2J");
    let at = |s: &mut String, row: usize, col: usize, text: &str| s.push_str(&format!("\x1b[{};{}H{text}", row + 1, col + 1));
    at(&mut s, 1, 2, "\x1b[1mzett\x1b[22m");
    for (i, item) in ["Inbox", "Today", "Projects", "Notes", "Archive"].iter().enumerate() {
        at(&mut s, 3 + i, 3, item);
    }
    at(&mut s, 1, 22, "\x1b[1mGood morning\x1b[22m");
    at(&mut s, 3, 22, "\x1b[2m3 tasks due today · 2 notes changed\x1b[22m");
    at(&mut s, 6, 24, "\x1b[1mShip the ui escape\x1b[22m");
    at(&mut s, 7, 24, "zash · panels and big text · due 17:00");
    at(&mut s, 9, 24, "\x1b[34m▸ Open\x1b[39m   Later");
    at(&mut s, 13, 24, "Plain text in every other terminal.");
    if !plain {
        s.push_str(concat!(
            "\x1b]7733;ui;",
            "panel,0,0,19,17,role=surface-container-low,r=0;",
            "panel,1,3,17,1,role=primary-container,r=8;",
            "text,2,1,16,1,text=zett,scale=1.3,weight=bold;",
            "text,22,1,40,2,text=Good morning,scale=2.2,weight=bold;",
            "panel,22,5,42,6,role=surface-container-high,r=14,border=outline-variant;",
            "text,24,6,38,1,text=Ship the ui escape,scale=1.25,weight=bold,role=on-surface;",
            "panel,22,12,42,3,role=primary-container,r=12",
            "\x07"
        ));
    }
    at(&mut s, 16, 22, "");
    s
}

//! UI hints (OSC 7733;ui): a program says which cells form a panel or a big heading, zash draws
//! them (render.rs), and every other terminal shows the plain cells the program printed anyway.
//!
//! OSC 7733;ui;REGION;REGION… BEL — the whole set for the screen, sent again on each redraw;
//! OSC 7733;ui BEL clears it. Cells are the pane's, at the time of the escape: the set scrolls
//! with the text (and is only shown on the screen it was sent on, main or alternate).
//! REGION = KIND,X,Y,W,H[,key=value]… (values percent-encoded: %2C , %3B ; %25 %)
//!   panel,…  a rounded rectangle behind the cells
//!     role=surface-container  r=PX (default: from the cell height)  pad=CELLS (0.5: its edge sits
//!     that far outside the cells)  border=ROLE (a 1px line at 50%)  shadow=1 (zash's float shadow)
//!   pill,…   a fully rounded label background (a selected row, a button): role=primary-container,
//!     pad=0.6; with text=… the words are drawn on it, centred, in fg= (default on-ROLE) and the cells hidden
//!   text,…,text=Inbox  the words drawn over the rect (left, centred in its height); cells hidden
//!     scale=1.6  weight=bold|semibold|medium|light|100..900  role=on-surface
//!   text and pill: font=mono (the cell font, default) | ui (the UI font, ui_font in zash.toml) | a family;
//!     a name fontdb lacks (sans-serif, serif, an alias of ~/.config/fontconfig) is what fc-match picks
//!     (asked once per name), else a sans
//! Unknown kinds and keys are skipped (button, list, input come later).

use crate::config::{Palette, Rgb};

/// Words on a region: what, how big (times the cell font), how heavy, which font.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub scale: f32,
    pub weight: u16,
    pub font: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// radius None: from the cell height; pad in cells
    Panel { radius: Option<f32>, border: Option<String>, shadow: f32, pad: f32 },
    Pill { label: Option<Label>, fg: Option<String>, pad: f32 },
    Text(Label),
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
        let num = |k: &str, d: f32, lo: f32, hi: f32| get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d).clamp(lo, hi);
        // words default to the cell size, a taller rect's to a heading's
        let label = |d: f32| get("text").filter(|t| !t.is_empty()).map(|text| Label { text, scale: num("scale", d, 0.5, 6.0), weight: weight(get("weight").as_deref()), font: get("font").unwrap_or("mono".into()) });
        let (kind, role) = match kind {
            "panel" => (Kind::Panel { radius: get("r").and_then(|v| v.parse().ok()).map(|r: f32| r.clamp(0.0, 64.0)), border: get("border"), shadow: num("shadow", 0.0, 0.0, 3.0), pad: num("pad", 0.5, 0.0, 4.0) }, get("role").unwrap_or("surface-container".into())),
            "pill" => (Kind::Pill { label: label(1.0), fg: get("fg"), pad: num("pad", 0.6, 0.0, 4.0) }, get("role").unwrap_or("primary-container".into())),
            "text" => (Kind::Text(label(if n[3] > 1 { 1.6 } else { 1.0 })?), get("role").unwrap_or("on-surface".into())),
            _ => return None,
        };
        Some(Region { kind, rect: [n[0], n[1], n[2], n[3]], role })
    }).take(256).collect()
}

/// A CSS-style weight: a name or 100..900 (default: regular).
fn weight(w: Option<&str>) -> u16 {
    match w {
        Some("bold") => 700,
        Some("semibold") => 600,
        Some("medium") => 500,
        Some("light") => 300,
        Some(n) => n.parse().map(|n: u16| n.clamp(100, 900)).unwrap_or(400),
        None => 400,
    }
}

fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    Rgb(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// A Material role from zash's palette (surface steps are the background lifted toward the text),
/// else the anim escape's names (accent, fg, red…) or hex.
pub fn role(name: &str, p: &Palette) -> Option<Rgb> {
    let (bg, fg, acc) = (p.bg, p.fg, p.cursor_bg);
    // surfaces: the background lifted a little toward the text, with a trace of the accent (M3's tint)
    let lift = |k: f32| mix(mix(bg, fg, k), acc, k * 0.35);
    Some(match name {
        "surface" | "background" => bg,
        "surface-container-lowest" => mix(bg, Rgb(0, 0, 0), 0.3),
        "surface-container-low" => lift(0.04),
        "surface-container" => lift(0.06),
        "surface-container-high" => lift(0.085),
        "surface-container-highest" | "surface-variant" => lift(0.115),
        "on-surface" | "on-background" => fg,
        "on-surface-variant" => mix(fg, bg, 0.35),
        "outline" => mix(bg, fg, 0.35),
        "outline-variant" => mix(bg, fg, 0.16),
        "primary" => acc,
        "on-primary" => p.cursor_fg,
        "primary-container" => mix(bg, acc, 0.22),
        "on-primary-container" => mix(fg, acc, 0.35),
        "secondary" => p.ansi[6],
        "secondary-container" => mix(bg, p.ansi[6], 0.18),
        "on-secondary-container" => mix(fg, p.ansi[6], 0.3),
        "tertiary" => p.ansi[5],
        "error" => p.ansi[1],
        other => crate::anim::theme_color(other, p).map(|c| Rgb(c[0], c[1], c[2]))?,
    })
}

/// The text colour on a role: on-ROLE when there is one, else the surface's text.
pub fn on_role(name: &str, p: &Palette) -> Rgb {
    role(&format!("on-{name}"), p).filter(|_| !name.starts_with("surface")).unwrap_or(p.fg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_parse() {
        let r = parse("panel,0,0,20,24,role=surface-container-high,r=12,border=outline;text,22,1,30,2,scale=2.5,weight=bold,text=Hello%2C%20world%3B");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0], Region { kind: Kind::Panel { radius: Some(12.0), border: Some("outline".into()), shadow: 0.0, pad: 0.5 }, rect: [0, 0, 20, 24], role: "surface-container-high".into() });
        assert_eq!(r[1].kind, Kind::Text(Label { text: "Hello, world;".into(), scale: 2.5, weight: 700, font: "mono".into() }));
        assert_eq!(r[1].role, "on-surface");
        // defaults; empty, zero-size, unknown and text without words skipped
        assert_eq!(parse("panel,1,2,3,4")[0].kind, Kind::Panel { radius: None, border: None, shadow: 0.0, pad: 0.5 });
        assert!(parse("").is_empty());
        assert!(parse("panel,0,0,0,4;button,0,0,4,1;text,0,0,4,1;panel,1,x,3,4").is_empty());
        // a stray % stays
        assert_eq!(parse("text,0,0,4,1,text=50%")[0].kind, Kind::Text(Label { text: "50%".into(), scale: 1.0, weight: 400, font: "mono".into() }));
        // a pill: its words in the UI font, weight by number; without words the cells stay
        assert_eq!(parse("pill,1,1,6,1,text=Open,font=ui,weight=550,role=primary")[0].kind, Kind::Pill { label: Some(Label { text: "Open".into(), scale: 1.0, weight: 550, font: "ui".into() }), fg: None, pad: 0.6 });
        assert_eq!(parse("pill,1,1,6,1")[0].kind, Kind::Pill { label: None, fg: None, pad: 0.6 });
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

/// `zash ui demo`: a mock zett screen (a sidebar, a heading, a card with buttons, a status line) in
/// plain cells, and the hints that make zash draw it as a UI (`plain`: the cells only, as another
/// terminal shows them). `cols` x `rows`: the screen.
pub fn demo(plain: bool, cols: usize, rows: usize) -> String {
    let (cols, rows) = (cols.max(60), rows.max(18));
    let mut s = String::from("\x1b[?25l\x1b[H\x1b[2J");
    let at = |s: &mut String, row: usize, col: usize, text: &str| s.push_str(&format!("\x1b[{};{}H{text}", row + 1, col + 1));
    let mut ui: Vec<String> = Vec::new();
    let dim = |t: &str| format!("\x1b[2m{t}\x1b[22m");
    let enc = |t: &str| t.replace('%', "%25").replace(',', "%2C").replace(';', "%3B");
    let side = 24;
    let last = rows - 1;
    // the sidebar: a lower tone, the app's name, entries with icons, the chosen one a pill
    ui.push(format!("panel,0,0,{side},{last},role=surface-container-low,r=0,pad=0"));
    at(&mut s, 1, 3, "\x1b[1mzett\x1b[22m");
    ui.push("text,3,1,12,1,text=zett,font=ui,weight=650,scale=1.15".into());
    let items = [("\u{f01c}", "Inbox", "12"), ("\u{f073}", "Today", "3"), ("\u{f07b}", "Projects", ""), ("\u{f15c}", "Notes", ""), ("\u{f187}", "Archive", "")];
    for (i, (icon, name, n)) in items.iter().enumerate() {
        let row = 3 + i;
        let sel = i == 0;
        let line = format!("{icon}  {name:<11}{n:>3}");
        at(&mut s, row, 3, &if sel && plain { format!("\x1b[7m {line} \x1b[27m") } else if sel { format!(" {icon}  {name:<11}{n:>3} ") } else { format!(" \x1b[2m{icon}\x1b[22m  {name:<11}{} ", dim(&format!("{n:>3}"))) });
        if sel {
            ui.push(format!("pill,3,{row},{},1,role=primary-container", side - 5));
        }
        let role = if sel { "on-primary-container" } else { "on-surface" };
        ui.push(format!("text,7,{row},11,1,text={name},font=ui,weight={},role={role}", if sel { 600 } else { 450 }));
    }
    at(&mut s, 9, 4, &dim("PINNED"));
    ui.push("text,4,9,12,1,text=PINNED,font=ui,weight=600,scale=0.8,role=on-surface-variant".into());
    for (i, (dot, name)) in [("\x1b[32m●\x1b[39m", "zash"), ("\x1b[35m●\x1b[39m", "zeit")].iter().enumerate() {
        at(&mut s, 10 + i, 4, &format!("{dot}  {name}"));
        ui.push(format!("text,7,{},12,1,text={name},font=ui,weight=450", 10 + i));
    }
    // the page: a heading, a subtitle, a card with two buttons, a quieter note
    let x = side + 4;
    let w = (cols - x - 4).min(60);
    at(&mut s, 1, x, "\x1b[1mGood morning\x1b[22m");
    ui.push(format!("text,{x},1,{w},2,text=Good morning,font=ui,weight=600,scale=1.75"));
    at(&mut s, 3, x, &dim("3 tasks due today · 2 notes changed"));
    ui.push(format!("text,{x},3,{w},1,text={},font=ui,weight=400,scale=1.0,role=on-surface-variant", enc("3 tasks due today · 2 notes changed")));
    let (cx, cw) = (x + 1, w - 2);
    if plain {
        at(&mut s, 5, x - 1, &dim(&format!("╭{}╮", "─".repeat(w))));
        for r in 6..10 {
            at(&mut s, r, x - 1, &dim("│"));
            at(&mut s, r, x + w, &dim("│"));
        }
        at(&mut s, 10, x - 1, &dim(&format!("╰{}╯", "─".repeat(w))));
    }
    ui.push(format!("panel,{x},6,{w},4,role=surface-container-high,border=outline-variant,shadow=1,pad=0.6"));
    at(&mut s, 6, cx, "\x1b[1mShip the ui escape\x1b[22m");
    ui.push(format!("text,{cx},6,{cw},1,text=Ship the ui escape,font=ui,weight=600,scale=1.15"));
    at(&mut s, 7, cx, &dim("zash · panels, pills and fonts · due 17:00"));
    ui.push(format!("text,{cx},7,{cw},1,text={},font=ui,role=on-surface-variant", enc("zash · panels, pills and fonts · due 17:00")));
    // buttons: their labels in cells, a pill behind each with its words on top
    at(&mut s, 9, cx, &if plain { "\x1b[7m Open \x1b[27m  [ Later ]".to_string() } else { " Open    Later".to_string() });
    ui.push(format!("pill,{cx},9,6,1,role=primary,text=Open,font=ui,weight=600,pad=0.8"));
    ui.push(format!("pill,{},9,7,1,role=surface-container-highest,text=Later,font=ui,weight=500,pad=0.8", cx + 9));
    // the command on the right: the words before it are narrower in the UI font than in cells
    let cmd = "zash ui demo --plain";
    at(&mut s, 13, cx, "Other terminals: plain");
    at(&mut s, 13, cx + cw - cmd.len(), &format!("\x1b[36m{cmd}\x1b[39m"));
    ui.push(format!("panel,{x},13,{w},1,role=surface-container,pad=0.6"));
    let note = "Other terminals: plain";
    ui.push(format!("text,{cx},13,{},1,text={},font=ui,role=on-surface-variant", note.chars().count(), enc(note)));
    // the status line, as zett's: the mode, the file, then branch, type and place on the right
    let right = "\u{e0a0} master   Markdown   Top ";
    let rx = cols - right.chars().count();
    at(&mut s, last, 0, &if plain { "\x1b[7m N \x1b[27m".to_string() } else { "    ".to_string() });
    at(&mut s, last, 5, "\u{f48a} README.md \x1b[2m 1:1\x1b[22m");
    at(&mut s, last, rx, "\x1b[35m\u{e0a0} master\x1b[39m   Markdown   \x1b[2mTop\x1b[22m ");
    ui.push(format!("panel,0,{last},{cols},1,role=surface-container,r=0,pad=0"));
    ui.push(format!("pill,1,{last},3,1,role=primary,text=N,font=ui,weight=700,pad=0.2"));
    if !plain {
        s.push_str(&format!("\x1b]7733;ui;{}\x07", ui.join(";")));
    }
    at(&mut s, 4, x, "");
    s
}

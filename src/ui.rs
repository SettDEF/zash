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
//!   line,…   a 1px line along one edge of the rect: edge=top|bottom|left|right (default bottom),
//!     role=outline-variant
//!   button,… a rounded button: text=…, role=secondary-container (on-ROLE words), pad=0.6
//!   input,…  a text field: text=the value, placeholder=…, role=surface-container-highest; its border is the accent
//!     while state=focus
//!   progress,… a bar across the rect: value=0..1, role=primary (the fill) over surface-container-highest
//!   list,… items=Inbox|Today|Done (one a row), sel=N (drawn chosen, role=primary-container), font=; with id=NAME each
//!     row reports as NAME:N
//!   table,… cols=12|8|6 (widths in cells), header=1 (default): a header band (role=surface-container-highest), striped
//!     rows and faint column lines under the program's own cells
//!   any kind: id=NAME makes it interactive: zash draws hover and press itself (a short fade) and tells the
//!     program: ESC ] 7733;ev;enter|leave|press|click;NAME BEL on its input (clicks on it are not text selection);
//!     state=idle|focus|active|done|disabled is the program's own (Tab and Enter stay its keys: it moves focus)
//! Unknown kinds and keys are skipped.

use crate::config::{Palette, Rgb};

pub mod kit;
pub mod layout;
pub mod tokens;

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
    /// a 1px line on this edge of the rect
    Line(Edge),
    Button { label: Option<Label>, pad: f32 },
    Input { label: Option<Label>, placeholder: bool },
    /// how far along, 0..1
    Progress { value: f32 },
    /// rows of words, one a row; `sel` drawn chosen; with id=, each row is ID:N for its events
    List { items: Vec<String>, sel: Option<usize>, font: String },
    /// a header band, striped rows and faint lines between columns of these widths (cells), under the text
    Table { cols: Vec<usize>, header: bool },
}

/// What the program says a region is in (state=); hover and press zash knows itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Idle,
    Focus,
    Active,
    Done,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub kind: Kind,
    /// x, y, w, h in cells
    pub rect: [usize; 4],
    /// a theme colour name (role), or hex
    pub role: String,
    /// interactive under this name (id=)
    pub id: Option<String>,
    pub state: State,
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
            "line" => (Kind::Line(match get("edge").as_deref() { Some("top") => Edge::Top, Some("left") => Edge::Left, Some("right") => Edge::Right, _ => Edge::Bottom }), get("role").unwrap_or("outline-variant".into())),
            "text" => (Kind::Text(label(if n[3] > 1 { 1.6 } else { 1.0 })?), get("role").unwrap_or("on-surface".into())),
            "button" => (Kind::Button { label: label(1.0), pad: num("pad", 0.6, 0.0, 4.0) }, get("role").unwrap_or("secondary-container".into())),
            "input" => {
                let (l, ph) = match label(1.0) { Some(l) => (Some(l), false), None => (get("placeholder").filter(|t| !t.is_empty()).map(|text| Label { text, scale: 1.0, weight: 400, font: get("font").unwrap_or("mono".into()) }), true) };
                (Kind::Input { label: l, placeholder: ph }, get("role").unwrap_or("surface-container-highest".into()))
            }
            "progress" => (Kind::Progress { value: num("value", 0.0, 0.0, 1.0) }, get("role").unwrap_or("primary".into())),
            "list" => (Kind::List { items: get("items").map(|s| s.split('|').map(str::to_string).take(200).collect()).unwrap_or_default(), sel: get("sel").and_then(|v| v.parse().ok()), font: get("font").unwrap_or("mono".into()) }, get("role").unwrap_or("primary-container".into())),
            "table" => (Kind::Table { cols: get("cols").map(|s| s.split('|').filter_map(|c| c.parse().ok()).take(64).collect()).unwrap_or_default(), header: get("header").as_deref() != Some("0") }, get("role").unwrap_or("surface-container-highest".into())),
            _ => return None,
        };
        let state = match get("state").as_deref() { Some("focus") => State::Focus, Some("active") => State::Active, Some("done") => State::Done, Some("disabled") => State::Disabled, _ => State::Idle };
        Some(Region { kind, rect: [n[0], n[1], n[2], n[3]], role, id: get("id").filter(|i| !i.is_empty() && i.len() <= 64), state })
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

pub(crate) fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
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
        assert_eq!(r[0], Region { kind: Kind::Panel { radius: Some(12.0), border: Some("outline".into()), shadow: 0.0, pad: 0.5 }, rect: [0, 0, 20, 24], role: "surface-container-high".into(), id: None, state: State::Idle });
        assert_eq!(r[1].kind, Kind::Text(Label { text: "Hello, world;".into(), scale: 2.5, weight: 700, font: "mono".into() }));
        assert_eq!(r[1].role, "on-surface");
        // defaults; empty, zero-size, unknown and text without words skipped
        assert_eq!(parse("panel,1,2,3,4")[0].kind, Kind::Panel { radius: None, border: None, shadow: 0.0, pad: 0.5 });
        assert!(parse("").is_empty());
        assert!(parse("panel,0,0,0,4;widget,0,0,4,1;text,0,0,4,1;panel,1,x,3,4").is_empty());
        // a stray % stays
        assert_eq!(parse("text,0,0,4,1,text=50%")[0].kind, Kind::Text(Label { text: "50%".into(), scale: 1.0, weight: 400, font: "mono".into() }));
        // a pill: its words in the UI font, weight by number; without words the cells stay
        assert_eq!(parse("pill,1,1,6,1,text=Open,font=ui,weight=550,role=primary")[0].kind, Kind::Pill { label: Some(Label { text: "Open".into(), scale: 1.0, weight: 550, font: "ui".into() }), fg: None, pad: 0.6 });
        assert_eq!(parse("pill,1,1,6,1")[0].kind, Kind::Pill { label: None, fg: None, pad: 0.6 });
        // a line: its edge, outline-variant by default
        assert_eq!(parse("line,0,0,4,1,edge=top")[0], Region { kind: Kind::Line(Edge::Top), rect: [0, 0, 4, 1], role: "outline-variant".into(), id: None, state: State::Idle });
        assert_eq!(parse("line,0,0,4,1")[0].kind, Kind::Line(Edge::Bottom));
    }

    #[test]
    fn widgets_parse() {
        let r = parse("button,2,1,8,1,text=Save,id=save,state=focus;input,2,3,20,1,placeholder=Search,id=q;progress,2,5,20,1,value=0.4;button,0,0,4,1,id=x,state=disabled");
        assert_eq!(r.len(), 4);
        assert_eq!((r[0].id.as_deref(), r[0].state, r[0].role.as_str()), (Some("save"), State::Focus, "secondary-container"));
        assert!(matches!(&r[0].kind, Kind::Button { label: Some(l), .. } if l.text == "Save"));
        // no value: the placeholder is shown, marked as one
        assert!(matches!(&r[1].kind, Kind::Input { label: Some(l), placeholder: true } if l.text == "Search"));
        assert!(matches!(parse("input,0,0,9,1,text=abc,placeholder=x")[0].kind, Kind::Input { placeholder: false, .. }));
        assert_eq!(r[2].kind, Kind::Progress { value: 0.4 });
        assert_eq!(parse("progress,0,0,9,1,value=7")[0].kind, Kind::Progress { value: 1.0 }, "clamped");
        assert_eq!(r[3].state, State::Disabled);
        // an id too long or empty: not interactive
        assert_eq!(parse(&format!("button,0,0,4,1,id={}", "a".repeat(65)))[0].id, None);
        assert_eq!(parse("button,0,0,4,1,id=")[0].id, None);
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

/// `zash ui demo`: zett's editor screen built with the kit (activity bar, explorer, tabs, code, a
/// terminal panel, the status line); `plain`: as other terminals show it. `cols` x `rows`: the screen.
pub fn demo(plain: bool, cols: usize, rows: usize) -> String {
    use kit::*;
    use layout::Size;
    let (cols, rows) = (cols.max(60), rows.max(18));
    let explorer = section("zash", [tree([
        folder("scripts", false, [file("visual.sh")]),
        folder("src", true, [
            folder("ui", true, [file("kit.rs").selected(), file("layout.rs"), file("tokens.rs")]),
            file("main.rs"), file("render.rs"), file("ui.rs"),
        ]),
        file("Cargo.toml"), file("README.md"),
    ])]);
    let outline = section("outline", [list(None, [item("\u{eb5b}", "Screen"), item("\u{eb5b}", "Ui"), item("\u{ea8c}", "render"), item("\u{ea8c}", "draw"), item("\u{ea8c}", "highlight")])]);
    let code = [
        "/// The tree on a cols x rows screen.",
        "pub fn render(&self, cols: usize, rows: usize) -> Screen {",
        "    let rects = self.node().layout(Rect::new(0, 0, cols, rows));",
        "    let mut out = Screen::default();",
        "    for plain in [true, false] {",
        "        let mut c = Canvas::new(cols, rows);",
        "        self.draw(&rects, &mut 0, &mut c, plain);",
        "        out.text = c.emit(); // \"cells\"",
        "    }",
        "    out",
        "}",
        "",
        "/// A tree and its last render.",
        "pub struct Ui {",
        "    root: El,",
        "    last: Option<((usize, usize), Screen)>,",
        "}",
    ];
    let term = [
        mono("$ cargo test ui::", St::default()),
        mono("test ui::layout::tests::row_fixed_fill_fit ... ok", St { dim: true, ..St::default() }),
        spans(vec![span("test result: ", None), span("ok", Some(GREEN)), span(". 9 passed; 0 failed", None)]),
    ];
    let editor_area = column([
        tabs(&[("kit.rs", true, true), ("layout.rs", false, false), ("README.md", false, false)]),
        breadcrumb(&["src", "ui", "kit.rs", "render"]),
        editor(code.iter().map(|l| highlight(l)).collect(), 6),
        panel(&["terminal", "problems"], 0, term).h(Size::Part(0.28)),
    ]).h(Size::Fill(1)).gap(tokens::Gap::None);
    let status = status_bar(
        [badge("N"), spans(vec![span("\u{e7a8} ", None), Span("kit.rs".into(), St { bold: true, ..St::default() })]), mono("7:9", St { dim: true, ..St::default() })],
        [mono("\u{e0a0} ui-panels", St { fg: Some(MAGENTA), ..St::default() }), mono("Rust", St::default()), mono("42%", St { dim: true, ..St::default() })],
    );
    let ui = column([
        row([
            activity_bar(&["\u{f0c5}", "\u{f002}", "\u{e725}", "\u{f0ae}"], 0, &["\u{f013}"]),
            sidebar(24, [explorer, outline]),
            editor_area,
        ]).h(Size::Fill(1)),
        status,
    ]).h(Size::Fill(1));
    let s = ui.render(cols, rows);
    if plain { s.plain } else { s.zash() }
}

#[cfg(test)]
mod list_table_tests {
    use super::*;

    #[test]
    fn lists_and_tables_parse() {
        let r = parse("list,0,0,8,3,items=Inbox|Today|Done,sel=1,id=nav;table,10,0,20,4,cols=8|6|6,header=0");
        assert_eq!(r[0].kind, Kind::List { items: vec!["Inbox".into(), "Today".into(), "Done".into()], sel: Some(1), font: "mono".into() });
        assert_eq!(r[0].id.as_deref(), Some("nav"));
        assert_eq!(r[1].kind, Kind::Table { cols: vec![8, 6, 6], header: false });
        assert_eq!(parse("table,0,0,4,2")[0].kind, Kind::Table { cols: vec![], header: true });
    }
}

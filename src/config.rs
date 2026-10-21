//! zash's settings: font, size, padding, colours.
//!
//! Defaults come from the user's WezTerm setup (docs/TERMINAL.md, "Starting
//! config"): MonaspiceNe Nerd Font 11.5 with its stylistic sets, emoji
//! fallback, no padding, and the colours tinct writes for WezTerm
//! (~/.config/wezterm/tinct_colors.lua), so zash follows the wallpaper too.
//! Overrides: ZASH_FONT, ZASH_FONT_SIZE (until `term.lua` lands).

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(s: &str) -> Option<Rgb> {
        let s = s.trim().trim_start_matches('#');
        if s.len() != 6 {
            return None;
        }
        let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
        Some(Rgb(p(0)?, p(2)?, p(4)?))
    }
    pub fn f32(self) -> [f32; 4] {
        // the surface is sRGB: shaders get linear values
        let lin = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        [lin(self.0), lin(self.1), lin(self.2), 1.0]
    }
}

#[derive(Clone, Debug)]
pub struct Palette {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor_bg: Rgb,
    pub cursor_fg: Rgb,
    pub selection_bg: Rgb,
    pub selection_fg: Rgb,
    /// 0–7 normal, 8–15 bright
    pub ansi: [Rgb; 16],
}

impl Default for Palette {
    fn default() -> Self {
        // tinct's dark output on 2026-10-17, so a missing file still looks like home
        let h = |s| Rgb::hex(s).unwrap();
        Palette {
            fg: h("#e3e2e5"),
            bg: h("#181B1E"),
            cursor_bg: h("#a8caeb"),
            cursor_fg: h("#0b334e"),
            selection_bg: h("#42474d"),
            selection_fg: h("#c2c7ce"),
            ansi: [
                h("#181B1E"), h("#F0474D"), h("#14B861"), h("#F0CA0F"), h("#3F84F3"), h("#D140E7"), h("#0EAFD8"), h("#C9C5CA"),
                h("#6F6B70"), h("#FB6065"), h("#33E686"), h("#FADB42"), h("#74A8FB"), h("#E46FF6"), h("#34CEF4"), h("#DDE3EF"),
            ],
        }
    }
}

impl Palette {
    /// The palette for a screen in reverse video (DECSCNM): text and background swap.
    pub fn reversed(&self) -> Palette {
        Palette { fg: self.bg, bg: self.fg, cursor_bg: self.bg, cursor_fg: self.fg, ..self.clone() }
    }

    /// Reads tinct's WezTerm colour file: `key = "#hex"` lines and the
    /// `ansi = { … }` / `brights = { … }` lists. Anything missing keeps its default.
    pub fn from_tinct_lua(src: &str) -> Palette {
        let mut p = Palette::default();
        let mut list: Option<(&str, usize)> = None;
        for raw in src.lines() {
            let line = raw.split("--").next().unwrap_or("").trim();
            if let Some((name, idx)) = list.as_mut() {
                if line.starts_with('}') {
                    list = None;
                    continue;
                }
                if let Some(c) = line.split('"').nth(1).and_then(Rgb::hex) {
                    let base = if *name == "ansi" { 0 } else { 8 };
                    if *idx < 8 {
                        p.ansi[base + *idx] = c;
                    }
                    *idx += 1;
                }
                continue;
            }
            let Some((key, val)) = line.split_once('=') else { continue };
            let key = key.trim();
            if (key == "ansi" || key == "brights") && val.contains('{') {
                list = Some((if key == "ansi" { "ansi" } else { "brights" }, 0));
                continue;
            }
            let Some(c) = val.split('"').nth(1).and_then(Rgb::hex) else { continue };
            match key {
                "foreground" => p.fg = c,
                "background" => p.bg = c,
                "cursor_bg" => p.cursor_bg = c,
                "cursor_fg" => p.cursor_fg = c,
                "selection_bg" => p.selection_bg = c,
                "selection_fg" => p.selection_fg = c,
                _ => {}
            }
        }
        p
    }

    pub fn tinct_file() -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join(".config/wezterm/tinct_colors.lua")
    }

    pub fn load() -> Palette {
        std::fs::read_to_string(Self::tinct_file()).map(|s| Self::from_tinct_lua(&s)).unwrap_or_default()
    }

    /// The 256-colour table: 16 named, the 6×6×6 cube, 24 greys.
    pub fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb(step(i / 36), step((i / 6) % 6), step(i % 6))
            }
            _ => {
                let v = 8 + (i - 232) * 10;
                Rgb(v, v, v)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// what a plain `zash` runs instead of the shell
    pub command: Option<String>,
    /// the shell to start; zash sets SHELL to it, so programs inside agree
    pub shell: Option<String>,
    pub font_family: String,
    /// points, as WezTerm counts them
    pub font_size_pt: f32,
    pub line_height: f32,
    /// OpenType features, e.g. "ss04"
    pub features: Vec<(String, u32)>,
    pub padding: f32,
    pub scrollback: usize,
    /// cursor glide time constant, ms (0: no animation)
    pub cursor_ms: f32,
    /// scrolling glides for this long (ms; 0 = it jumps a line at a time)
    pub smooth_scroll_ms: f32,
    pub cursor_trail: bool,
    /// the cursor's style, a TUI's own cursor, and where zash's look applies (`[cursor]`)
    pub cursor: CursorConf,
    pub popup: PopupLook,
    /// a multiplexer's zfloats: their panel and frame (`[zfloat]`)
    pub zfloat: ZfloatLook,
    /// a GLSL shader as a zfloat's background: by its title, else the default (`[zfloat] shader`)
    pub zfloat_shaders: ZfloatShaders,
    /// text effects (OSC 7733;fx): on or off, their frame rate, presets (`[fx]`)
    pub fx: crate::fx::Settings,
    /// what a copy puts on the clipboard (`[copy]`, copy.rs)
    pub copy: crate::copy::Settings,
    /// drawing with the pen (`[pen]`, ink.rs)
    pub pen: PenSettings,
    /// the text centred in the space whole cells do not fill (else top-left), and moved by an offset
    pub center: bool,
    pub offset: (f32, f32),
    pub palette: Palette,
    /// the colour file followed live
    pub colors_file: PathBuf,
    /// window background opacity, 0..1 (text and cell colours stay solid)
    pub opacity: f32,
    /// the shell's Glass panel writes this (opacity = …); followed live unless the config sets one
    pub glass_file: Option<PathBuf>,
    /// zash on the wallpaper (`[desktop]`, `zash --desktop`)
    pub desktop: DesktopConf,
    /// `zash --float`: the window's size, logical pixels (`[float] size`)
    pub float_size: (u32, u32),
}

impl Config {
    /// `return { opacity = 0.95 }`, as the Glass panel writes it for WezTerm.
    pub fn read_glass(path: &std::path::Path) -> Option<f32> {
        let s = std::fs::read_to_string(path).ok()?;
        let v = s.split("opacity").nth(1)?.split('=').nth(1)?;
        let num: String = v.trim().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        num.parse::<f32>().ok().map(|o| o.clamp(0.1, 1.0))
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            shell: None,
            command: None,
            font_family: std::env::var("ZASH_FONT").unwrap_or_else(|_| "MonaspiceNe Nerd Font".into()),
            font_size_pt: std::env::var("ZASH_FONT_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(11.5),
            line_height: 1.2,
            features: ["cv06", "cv14", "cv32", "ss04", "ss07", "ss09"].iter().map(|f| (f.to_string(), 1)).collect(),
            padding: 0.0,
            scrollback: 10_000,
            cursor_ms: 90.0,
            smooth_scroll_ms: 80.0,
            cursor_trail: true,
            cursor: CursorConf::default(),
            popup: PopupLook::default(),
            zfloat: ZfloatLook::default(),
            zfloat_shaders: ZfloatShaders::default(),
            fx: crate::fx::Settings::default(),
            copy: crate::copy::Settings::default(),
            pen: PenSettings::default(),
            center: true,
            offset: (0.0, 0.0),
            palette: Palette::load(),
            colors_file: Palette::tinct_file(),
            opacity: 1.0,
            glass_file: None,
            desktop: DesktopConf::default(),
            float_size: (900, 560),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tinct_lua() {
        let src = r##"return {
    foreground    = "#e3e2e5",
    background    = "#101112", -- a comment "#ffffff"
    ansi = {
        "#000001",
        "#000002",
    },
    brights = {
        "#000009",
    },
    tab_bar = { background = "#1a1c1e" },
}"##;
        let p = Palette::from_tinct_lua(src);
        assert_eq!(p.bg, Rgb(0x10, 0x11, 0x12));
        assert_eq!(p.ansi[0], Rgb(0, 0, 1));
        assert_eq!(p.ansi[1], Rgb(0, 0, 2));
        assert_eq!(p.ansi[8], Rgb(0, 0, 9));
        assert_eq!(p.ansi[2], Palette::default().ansi[2], "missing entries keep their default");
    }

    #[test]
    fn colour_cube() {
        let p = Palette::default();
        assert_eq!(p.indexed(16), Rgb(0, 0, 0));
        assert_eq!(p.indexed(231), Rgb(255, 255, 255));
        assert_eq!(p.indexed(232), Rgb(8, 8, 8));
        assert_eq!(p.indexed(255), Rgb(238, 238, 238));
    }
}

/// `~/.config/zash/zash.toml`: every field optional, missing ones keep the
/// defaults above (taken from the WezTerm setup).
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    /// what a plain `zash` runs instead of the shell (e.g. "zeit new")
    command: Option<String>,
    font: Option<String>,
    /// the shell to start (else $SHELL)
    shell: Option<String>,
    size: Option<f32>,
    line_height: Option<f32>,
    features: Option<Vec<String>>,
    padding: Option<f32>,
    scrollback: Option<usize>,
    cursor_animation: Option<f32>,
    smooth_scroll: Option<f32>,
    cursor_trail: Option<bool>,
    cursor: Option<CursorFile>,
    /// 0..1; leave out to follow the shell's Glass panel
    opacity: Option<f32>,
    /// a tinct/WezTerm-style colour file to follow instead of the default
    colors: Option<String>,
    /// "center" or "top-left"
    align: Option<String>,
    /// [right, down] in pixels
    offset: Option<[f32; 2]>,
    /// the completion popup's look
    popup: Option<PopupFile>,
    zfloat: Option<ZfloatFile>,
    fx: Option<FxFile>,
    copy: Option<CopyFile>,
    pen: Option<PenFile>,
    desktop: Option<DesktopFile>,
    float: Option<FloatFile>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CursorFile {
    animation: Option<f32>,
    trail: Option<bool>,
    style: Option<String>,
    drawn: Option<bool>,
    scope: Option<String>,
}

/// Where zash's own cursor look (glide, trail, style) applies.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CursorScope {
    #[default]
    All,
    /// not inside a zfloat
    Terminal,
    /// only inside a zfloat
    Zfloat,
}

/// `[cursor]`: the glide and trail live in cursor_ms / cursor_trail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorConf {
    /// a shape over what programs ask for; None: theirs
    pub style: Option<alacritty_terminal::vte::ansi::CursorShape>,
    /// follow a cursor a program draws itself while it hides the real one
    pub drawn: bool,
    pub scope: CursorScope,
}

impl Default for CursorConf {
    fn default() -> Self {
        CursorConf { style: None, drawn: true, scope: CursorScope::All }
    }
}

impl CursorConf {
    /// A style name: "app" (None) or a shape.
    pub fn shape(name: &str) -> Result<Option<alacritty_terminal::vte::ansi::CursorShape>, String> {
        use alacritty_terminal::vte::ansi::CursorShape as S;
        Ok(match name {
            "app" => None,
            "block" => Some(S::Block),
            "beam" | "bar" => Some(S::Beam),
            "underline" => Some(S::Underline),
            "hollow" => Some(S::HollowBlock),
            o => return Err(format!("style = {o:?}: use \"app\", \"block\", \"beam\", \"underline\" or \"hollow\"")),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PenSettings {
    /// the widest a stroke gets, in cell heights (full pressure)
    pub width: f32,
    /// its colour; None: the cursor colour
    pub color: Option<Rgb>,
}

impl Default for PenSettings {
    fn default() -> Self {
        PenSettings { width: 0.35, color: None }
    }
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PenFile {
    width: Option<f32>,
    color: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CopyFile {
    unwrap: Option<String>,
    trim: Option<bool>,
    paste_guard: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FxFile {
    enabled: Option<bool>,
    shaders: Option<bool>,
    fps: Option<f32>,
    presets: Option<std::collections::HashMap<String, String>>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ZfloatFile {
    opacity: Option<f32>,
    border: Option<String>,
    shader: Option<String>,
    shaders: Option<std::collections::HashMap<String, String>>,
    /// options per shader ("ps3/wave" → speed = 0.8), over its package's defaults (pack.toml)
    shader_options: Option<std::collections::HashMap<String, std::collections::HashMap<String, f64>>>,
}

/// Panel shaders (panel_shader.rs): one for every zfloat, and one per title on top.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ZfloatShaders {
    pub default: Option<crate::panel_shader::Choice>,
    pub by_title: std::collections::HashMap<String, crate::panel_shader::Choice>,
}

impl ZfloatShaders {
    pub fn for_title(&self, title: &str) -> Option<&crate::panel_shader::Choice> {
        self.by_title.get(title).or(self.default.as_ref())
    }
}

/// Which corner, edge or centre of the screen the desktop terminal sits at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    pub fn parse(s: &str) -> Option<Anchor> {
        Some(match s.trim().to_lowercase().replace(['_', ' '], "-").as_str() {
            "top-left" => Anchor::TopLeft,
            "top" => Anchor::Top,
            "top-right" => Anchor::TopRight,
            "left" => Anchor::Left,
            "center" | "centre" => Anchor::Center,
            "right" => Anchor::Right,
            "bottom-left" => Anchor::BottomLeft,
            "bottom" => Anchor::Bottom,
            "bottom-right" => Anchor::BottomRight,
            _ => return None,
        })
    }

    /// (left, right, top, bottom): the edges it is pinned to.
    pub fn edges(self) -> (bool, bool, bool, bool) {
        use Anchor::*;
        let left = matches!(self, TopLeft | Left | BottomLeft);
        let right = matches!(self, TopRight | Right | BottomRight);
        let top = matches!(self, TopLeft | Top | TopRight);
        let bottom = matches!(self, BottomLeft | Bottom | BottomRight);
        (left, right, top, bottom)
    }
}

/// The layer the desktop terminal lives on: under the windows (bottom), or with the wallpaper.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DesktopLayer {
    Background,
    Bottom,
}

/// How dark (or light) the panel under the desktop terminal's text is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Readability {
    /// measured from the wallpaper under it: just enough for the contrast asked
    Auto,
    /// a fixed panel opacity, 0..1
    Fixed(f32),
}

/// `[desktop]`: zash on the wallpaper (`zash --desktop`), click-through.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopConf {
    pub anchor: Anchor,
    /// logical pixels
    pub size: (u32, u32),
    /// from the pinned edges, logical pixels: [x, y]
    pub margin: (i32, i32),
    pub layer: DesktopLayer,
    /// a monitor by name (eDP-1); None: the compositor's choice
    pub output: Option<String>,
    /// what runs in it; None: the zeit session "desktop" (attach to it elsewhere to type)
    pub command: Option<String>,
    pub readability: Readability,
    /// the contrast the text keeps over the wallpaper (WCAG ratio; 7 = AAA)
    pub contrast: f32,
    /// never less panel than this, even over a plain wallpaper
    pub min_opacity: f32,
    pub font_size_pt: Option<f32>,
}

impl Default for DesktopConf {
    fn default() -> Self {
        DesktopConf {
            anchor: Anchor::BottomRight,
            size: (720, 400),
            margin: (24, 24),
            layer: DesktopLayer::Bottom,
            output: None,
            command: None,
            readability: Readability::Auto,
            contrast: 7.0,
            min_opacity: 0.12,
            font_size_pt: None,
        }
    }
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DesktopFile {
    anchor: Option<String>,
    size: Option<[u32; 2]>,
    margin: Option<[i32; 2]>,
    layer: Option<String>,
    output: Option<String>,
    command: Option<String>,
    /// "auto" or a number 0..1
    readability: Option<toml::Value>,
    contrast: Option<f32>,
    min_opacity: Option<f32>,
    size_pt: Option<f32>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FloatFile {
    size: Option<[u32; 2]>,
}

/// How a zfloat looks: `[zfloat]` in zash.toml.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZfloatLook {
    /// of its panel (0–1): lower lets the window's own background through
    pub opacity: f32,
    /// the frame: Panel (its own colour, no line), Accent (the cursor colour while it has
    /// the keyboard), None
    pub border: ZfloatBorder,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ZfloatBorder {
    Panel,
    Accent,
    None,
}

impl Default for ZfloatLook {
    fn default() -> Self {
        ZfloatLook { opacity: 0.6, border: ZfloatBorder::Panel }
    }
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PopupFile {
    opacity: Option<f32>,
    radius: Option<f32>,
    blur: Option<f32>,
    shadow: Option<bool>,
    border: Option<bool>,
    rows: Option<usize>,
}

/// How the completion popup looks: `[popup]` in zash.toml, and per shell from
/// `zstyle ':zash:popup' …` (sent with the first popup, it wins).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopupLook {
    /// of its background (the text behind shows through, blurred)
    pub opacity: f32,
    pub radius: f32,
    /// blur of what is behind it, in pixels (0: none)
    pub blur: f32,
    pub shadow: bool,
    pub border: bool,
    /// rows shown at most
    pub rows: usize,
}

impl Default for PopupLook {
    fn default() -> Self {
        PopupLook { opacity: 0.82, radius: 12.0, blur: 10.0, shadow: true, border: true, rows: 10 }
    }
}

impl PopupLook {
    /// `opacity=0.8,radius=16,blur=yes,rows=8` on top of this one (unknown keys and bad values are skipped).
    pub fn with(&self, overrides: &str) -> PopupLook {
        let mut l = *self;
        let yes = |v: &str| matches!(v, "yes" | "true" | "on" | "1");
        for kv in overrides.split(',') {
            let Some((k, v)) = kv.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "opacity" => if let Ok(x) = v.parse::<f32>() { l.opacity = x.clamp(0.0, 1.0) },
                "radius" => if let Ok(x) = v.parse::<f32>() { l.radius = x.clamp(0.0, 40.0) },
                "blur" => l.blur = v.parse::<f32>().map(|x| x.clamp(0.0, 40.0)).unwrap_or(if yes(v) { 10.0 } else { 0.0 }),
                "shadow" => l.shadow = yes(v),
                "border" => l.border = yes(v),
                "rows" => if let Ok(x) = v.parse::<usize>() { l.rows = x.clamp(1, 40) },
                _ => {}
            }
        }
        l
    }
}

pub const DEFAULT_FILE: &str = r#"# zash — settings. Every line is optional; delete one to get the default back.
# Changes apply the next time zash starts.

# The font and its size in points (as WezTerm counts them).
font = "MonaspiceNe Nerd Font"
size = 11.5
# Line height as a multiple of the font size.
line_height = 1.2
# OpenType features of the font (stylistic sets, character variants).
features = ["cv06", "cv14", "cv32", "ss04", "ss07", "ss09"]
# Space around the text, in pixels.
padding = 0
# Where the text sits when the window is not a whole number of cells: "center" shares
# the leftover space on all sides, "top-left" leaves it at the bottom and right.
align = "center"
# Moves the text right and down by this many pixels (with "top-left": a margin).
offset = [0, 0]
# Lines kept for scrolling back.
scrollback = 10000
# Scrolling glides instead of jumping a line at a time (milliseconds; 0 = off).
smooth_scroll = 80
# Background opacity (0.1–1). Leave it out to follow the shell's Glass panel.
# opacity = 0.95
# Colours: the file tinct writes for WezTerm, so zash follows the wallpaper.
colors = "~/.config/wezterm/tinct_colors.lua"

# The completion popup (zsh, while typing). A shell can change these with
# zstyle ':zash:popup' opacity 0.7 (and radius, blur, shadow, border, rows).
[popup]
# How solid its background is (0–1); what is behind shows through, blurred.
opacity = 0.82
# Corner radius and the blur of the text behind it, in pixels (blur 0 = none).
radius = 12
blur = 10
shadow = true
border = true
# Rows shown at most.
rows = 10

# The cursor.
#   animation: it glides to where it goes (milliseconds; 0 = it jumps); trail: a short smear
#   style:     "app" (what the program asks for), "block", "beam", "underline", "hollow"
#   drawn:     follow a cursor a full-screen program draws itself (one inverted cell, as
#              Claude Code does): it gets the glide and trail too
#   scope:     where the look above applies: "all", "terminal" (not in zfloats), "zfloat"
[cursor]
animation = 90
trail = true
style = "app"
drawn = true
scope = "all"

# A multiplexer's zfloats (zeit): the panel's opacity (0–1; lower lets the window show
# through) and its frame: "panel" (its own colour), "accent" (the cursor colour while it
# has the keyboard) or "none".
# shader: a GLSL file drawn as the panel (Shadertoy style: mainImage, iTime, iResolution, plus
# iPanel, iAccent, iFocus, iOrigin and iScreen for a camera-like scene, iMonth); [zfloat.shaders]
# gives one per title, e.g. Claude = "~/x.glsl", or a package's: Claude = "ps3/wave" (in
# ~/.config/zash/shaders/ps3, its options in pack.toml); [zfloat.shader_options."ps3/wave"] sets them.
[zfloat]
opacity = 0.6
border = "panel"
shader = ""
[zfloat.shaders]

# Effects, all switchable for old machines (nothing animates while they are off):
#   enabled: text effects programs ask for (OSC 7733;fx: shimmer, pulse, fade, rainbow,
#            typewriter, glow); off, their text shows plain
#   shaders: GLSL zfloat panels ([zfloat.shaders]); off, the panels are plain
#   fps:     the most frames a second either causes (15 is plenty on an old laptop); frames that
#            come out slow lower it on their own, and turn both off as a last resort A preset is a name programs can use, written like the escape:
#   done = "shimmer;color=good;speed=1.5;loop=2"
[copy]
# unwrap: "smart" joins lines a program wrapped itself (Claude Code, zeit) back into one, so a long
#         command pastes whole; lines ended on purpose (lists, short lines, code fences) stay apart.
#         "off" copies the rows as they are.
# trim:   drop spaces at line ends
# paste_guard: before several lines are pasted, ask (Enter as is · J as one line · Esc cancel):
#              "auto" when a shell is in front, "always" (inside zeit and full-screen programs too), "off".
#              Ctrl+Shift+Alt+V always pastes as one line, without asking.
unwrap = "smart"
trim = true
paste_guard = "auto"

# Drawing with the pen: Ctrl+Shift+D (the tip draws by pressure, the eraser end or the lower
# button erases, the upper button undoes; Ctrl+Z, Del clears, Esc stops). Strokes scroll with the text.
# width: the widest a stroke gets, in cell heights; color: a hex colour like #e0a050 (empty: the cursor colour)
[pen]
width = 0.35
color = ""

[fx]
enabled = true
shaders = true
fps = 30
[fx.presets]

# zash --desktop: a terminal on the wallpaper, click-through (zash --desktop --help).
# Its panel is as dark as the wallpaper under it needs for the contrast below.
# [desktop]
# anchor = "bottom-right"
# size = [720, 400]
# margin = [24, 24]
# layer = "bottom"
# command = "btop"
# readability = "auto"
# contrast = 7

# zash --float: a floating window of this size (logical pixels)
# [float]
# size = [900, 560]
"#;

impl Config {
    /// The settings file: $ZASH_CONFIG (or --config), else ~/.config/zash/zash.toml.
    pub fn file_path() -> PathBuf {
        if let Some(p) = std::env::var_os("ZASH_CONFIG").filter(|p| !p.is_empty()) {
            return PathBuf::from(p);
        }
        Self::config_dir().join("zash.toml")
    }

    pub fn config_dir() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
            .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".config"));
        base.join("zash")
    }

    /// A preset: a settings file loaded on top of the user's ($ZASH_PRESET or --preset).
    pub fn preset_path(name: &str) -> PathBuf {
        Self::config_dir().join("presets").join(format!("{name}.toml"))
    }

    fn parse(path: &std::path::Path) -> Option<File> {
        let text = std::fs::read_to_string(path).ok()?;
        match toml::from_str(&text) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("zash: {}: {e}", path.display());
                None
            }
        }
    }

    /// The defaults, then the user's file on top. A broken file is reported and ignored.
    pub fn load() -> Config {
        let mut c = Config::default();
        let path = Self::file_path();
        if !path.exists() && std::env::var_os("ZASH_CONFIG").is_none() {
            // first start: write the file, so every setting is discoverable
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, DEFAULT_FILE);
        }
        // layers: the user's file, then a preset on top (only what it sets changes)
        let f = Self::parse(&path).unwrap_or_default();
        c.apply(f, true);
        if let Some(name) = std::env::var("ZASH_PRESET").ok().filter(|n| !n.is_empty()) {
            let p = Self::preset_path(&name);
            match Self::parse(&p) {
                Some(f) => c.apply(f, false),
                None if !p.exists() => eprintln!("zash: no preset {name:?} (looked for {})", p.display()),
                None => {}
            }
        }
        // the environment still wins, for one-off runs
        if let Ok(v) = std::env::var("ZASH_FONT") { c.font_family = v; }
        if let Some(v) = std::env::var("ZASH_FONT_SIZE").ok().and_then(|s| s.parse().ok()) { c.font_size_pt = v; }
        c
    }

    /// One layer of settings: what the file sets changes, the rest stays. The first
    /// layer (`base`) also decides whether the Glass panel drives the opacity.
    fn apply(&mut self, f: File, base: bool) {
        let c = self;
        if let Some(v) = f.command { c.command = Some(v).filter(|s| !s.trim().is_empty()); }
        if let Some(v) = f.shell.filter(|s| !s.is_empty()) { c.shell = Some(v); }
        if let Some(v) = f.font { c.font_family = v; }
        if let Some(v) = f.size { c.font_size_pt = v; }
        if let Some(v) = f.line_height { c.line_height = v; }
        if let Some(v) = f.features { c.features = v.into_iter().map(|t| (t, 1)).collect(); }
        if let Some(v) = f.padding { c.padding = v; }
        if let Some(v) = f.scrollback { c.scrollback = v; }
        if let Some(v) = f.cursor_animation { c.cursor_ms = v.max(0.0); }
        if let Some(v) = f.smooth_scroll { c.smooth_scroll_ms = v.max(0.0); }
        if let Some(v) = f.cursor_trail { c.cursor_trail = v; }
        if let Some(k) = f.cursor {
            if let Some(v) = k.animation { c.cursor_ms = v.max(0.0); }
            if let Some(v) = k.trail { c.cursor_trail = v; }
            if let Some(v) = k.drawn { c.cursor.drawn = v; }
            match k.style.as_deref() {
                None => {}
                Some(s) => match CursorConf::shape(s) {
                    Ok(v) => c.cursor.style = v,
                    Err(e) => eprintln!("zash: [cursor] {e}"),
                },
            }
            match k.scope.as_deref() {
                None => {}
                Some("all") => c.cursor.scope = CursorScope::All,
                Some("terminal") => c.cursor.scope = CursorScope::Terminal,
                Some("zfloat") => c.cursor.scope = CursorScope::Zfloat,
                Some(o) => eprintln!("zash: [cursor] scope = {o:?}: use \"all\", \"terminal\" or \"zfloat\""),
            }
        }
        match f.align.as_deref() {
            Some("top-left") | Some("topleft") | Some("left") => c.center = false,
            Some("center") | Some("centre") => c.center = true,
            None => {}
            Some(other) => eprintln!("zash: align = {other:?}: use \"center\" or \"top-left\""),
        }
        if let Some([x, y]) = f.offset { c.offset = (x, y); }
        if let Some(x) = f.fx {
            if let Some(v) = x.enabled { c.fx.enabled = v; }
            if let Some(v) = x.shaders { c.fx.shaders = v; }
            if let Some(v) = x.fps { c.fx.fps = v.clamp(1.0, 120.0); }
            if let Some(p) = x.presets { c.fx.presets = p; }
        }
        if let Some(x) = f.pen {
            if let Some(w) = x.width { c.pen.width = w.clamp(0.05, 3.0); }
            match x.color.as_deref() {
                None | Some("") => {}
                Some(h) => match Rgb::hex(h) {
                    Some(rgb) => c.pen.color = Some(rgb),
                    None => eprintln!("zash: [pen] color = {h:?}: a colour like \"#e0a050\""),
                },
            }
        }
        if let Some(x) = f.copy {
            match x.unwrap.as_deref() {
                Some("smart") => c.copy.unwrap = true,
                Some("off") => c.copy.unwrap = false,
                None => {}
                Some(other) => eprintln!("zash: [copy] unwrap = {other:?}: use \"smart\" or \"off\""),
            }
            if let Some(t) = x.trim { c.copy.trim = t; }
            match x.paste_guard.as_deref() {
                Some(g @ ("auto" | "always" | "off")) => c.copy.paste_guard = g.into(),
                None => {}
                Some(other) => eprintln!("zash: [copy] paste_guard = {other:?}: use \"auto\", \"always\" or \"off\""),
            }
        }
        if let Some(d) = f.desktop {
            let k = &mut c.desktop;
            match d.anchor.as_deref().map(|a| (a, Anchor::parse(a))) {
                None => {}
                Some((_, Some(a))) => k.anchor = a,
                Some((a, None)) => eprintln!("zash: [desktop] anchor = {a:?}: use top-left, top, top-right, left, center, right, bottom-left, bottom or bottom-right"),
            }
            if let Some([w, h]) = d.size { k.size = (w.max(80), h.max(40)); }
            if let Some([x, y]) = d.margin { k.margin = (x, y); }
            match d.layer.as_deref() {
                None => {}
                Some("bottom") => k.layer = DesktopLayer::Bottom,
                Some("background") => k.layer = DesktopLayer::Background,
                Some(o) => eprintln!("zash: [desktop] layer = {o:?}: use \"bottom\" or \"background\""),
            }
            if let Some(v) = d.output { k.output = Some(v).filter(|s| !s.is_empty()); }
            if let Some(v) = d.command { k.command = Some(v).filter(|s| !s.trim().is_empty()); }
            match d.readability {
                None => {}
                Some(toml::Value::String(s)) if s == "auto" => k.readability = Readability::Auto,
                Some(toml::Value::Float(v)) => k.readability = Readability::Fixed((v as f32).clamp(0.0, 1.0)),
                Some(toml::Value::Integer(v)) => k.readability = Readability::Fixed((v as f32).clamp(0.0, 1.0)),
                Some(o) => eprintln!("zash: [desktop] readability = {o}: use \"auto\" or a number from 0 to 1"),
            }
            if let Some(v) = d.contrast { k.contrast = v.clamp(1.0, 21.0); }
            if let Some(v) = d.min_opacity { k.min_opacity = v.clamp(0.0, 1.0); }
            if let Some(v) = d.size_pt { k.font_size_pt = Some(v.clamp(4.0, 72.0)); }
        }
        if let Some([w, h]) = f.float.and_then(|f| f.size) { c.float_size = (w.max(200), h.max(120)); }
        if let Some(z) = f.zfloat {
            if let Some(v) = z.opacity { c.zfloat.opacity = v.clamp(0.0, 1.0); }
            // "ps3/wave" (a package's shader) or a path, with its options from [zfloat.shader_options]
            let all = z.shader_options.unwrap_or_default();
            let pick = |s: &str| {
                let mut o: Vec<(String, String)> = all.get(s).map(|m| m.iter().map(|(k, v)| (k.clone(), v.to_string())).collect()).unwrap_or_default();
                o.sort();
                crate::panel_shader::Choice::resolve(s, o)
            };
            c.zfloat_shaders.default = z.shader.as_deref().filter(|s| !s.is_empty()).map(pick);
            c.zfloat_shaders.by_title = z.shaders.unwrap_or_default().iter().map(|(k, v)| (k.clone(), pick(v))).collect();
            match z.border.as_deref() {
                Some("panel") | Some("same") => c.zfloat.border = ZfloatBorder::Panel,
                Some("accent") => c.zfloat.border = ZfloatBorder::Accent,
                Some("none") | Some("off") => c.zfloat.border = ZfloatBorder::None,
                None => {}
                Some(o) => eprintln!("zash: [zfloat] border = {o:?}: use \"panel\", \"accent\" or \"none\""),
            }
        }
        if let Some(p) = f.popup {
            let d = &mut c.popup;
            if let Some(v) = p.opacity { d.opacity = v.clamp(0.0, 1.0); }
            if let Some(v) = p.radius { d.radius = v.clamp(0.0, 40.0); }
            if let Some(v) = p.blur { d.blur = v.clamp(0.0, 40.0); }
            if let Some(v) = p.shadow { d.shadow = v; }
            if let Some(v) = p.border { d.border = v; }
            if let Some(v) = p.rows { d.rows = v.clamp(1, 40); }
        }
        match f.opacity {
            Some(v) => {
                c.opacity = v.clamp(0.1, 1.0);
                c.glass_file = None;
            }
            None if base => {
                let home = std::env::var("HOME").unwrap_or_default();
                // the shell's Glass panel writes zash's own file; WezTerm's until it has
                let mine = PathBuf::from(&home).join(".config/zash/glass.lua");
                let g = if mine.exists() { mine } else { PathBuf::from(&home).join(".config/wezterm/glass.lua") };
                if let Some(o) = Self::read_glass(&g) { c.opacity = o; }
                c.glass_file = Some(g);
            }
            None => {}
        }
        if let Some(v) = f.colors {
            let home = std::env::var("HOME").unwrap_or_default();
            let p = PathBuf::from(v.replacen('~', &home, 1));
            c.colors_file = p.clone();
            if let Ok(s) = std::fs::read_to_string(&p) {
                c.palette = Palette::from_tinct_lua(&s);
            }
        }
    }
}

#[cfg(test)]
mod file_tests {
    #[test]
    fn default_file_parses() {
        let f: super::File = toml::from_str(super::DEFAULT_FILE).unwrap();
        assert_eq!(f.size, Some(11.5));
        assert_eq!(f.features.unwrap().len(), 6);
    }
}

#[cfg(test)]
mod glass_tests {
    #[test]
    fn reads_glass_lua() {
        let p = std::env::temp_dir().join(format!("zash-glass-{}.lua", std::process::id()));
        std::fs::write(&p, "return { opacity = 0.95 }\n").unwrap();
        assert_eq!(super::Config::read_glass(&p), Some(0.95));
        std::fs::write(&p, "return { opacity = 5 }\n").unwrap();
        assert_eq!(super::Config::read_glass(&p), Some(1.0));
        std::fs::remove_file(&p).unwrap();
    }
}

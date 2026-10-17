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
    pub popup: PopupLook,
    /// a multiplexer's zfloats: their panel and frame (`[zfloat]`)
    pub zfloat: ZfloatLook,
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
            popup: PopupLook::default(),
            zfloat: ZfloatLook::default(),
            center: true,
            offset: (0.0, 0.0),
            palette: Palette::load(),
            colors_file: Palette::tinct_file(),
            opacity: 1.0,
            glass_file: None,
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
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ZfloatFile {
    opacity: Option<f32>,
    border: Option<String>,
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
# The cursor glides to where it goes (milliseconds; 0 = it jumps), with a short smear.
cursor_animation = 90
cursor_trail = true
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

# A multiplexer's zfloats (zeit): the panel's opacity (0–1; lower lets the window show
# through) and its frame: "panel" (its own colour), "accent" (the cursor colour while it
# has the keyboard) or "none".
[zfloat]
opacity = 0.6
border = "panel"
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
        match f.align.as_deref() {
            Some("top-left") | Some("topleft") | Some("left") => c.center = false,
            Some("center") | Some("centre") => c.center = true,
            None => {}
            Some(other) => eprintln!("zash: align = {other:?}: use \"center\" or \"top-left\""),
        }
        if let Some([x, y]) = f.offset { c.offset = (x, y); }
        if let Some(z) = f.zfloat {
            if let Some(v) = z.opacity { c.zfloat.opacity = v.clamp(0.0, 1.0); }
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

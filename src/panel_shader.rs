//! GLSL shaders as a zfloat's background (`[zfloat.shaders]` in zash.toml, or the shader zeit
//! names for it). Shadertoy style: the file defines `void mainImage(out vec4 fragColor, in vec2
//! fragCoord)` and may read `iResolution`, `iTime`, `iPanel` (the panel's colour), `iAccent`,
//! `iFocus` (1 while it has the keyboard), `iOrigin` + `iScreen` (for a scene that stays put while
//! the panel moves over it), `iMonth`, and its options. zash cuts it to the panel's rounded
//! corners; text and buttons stay on top. A shader that does not compile is reported once and the
//! panel stays plain.
//!
//! Packages: `shaders/<pack>/pack.toml` declares each shader's options (default, range) and
//! presets (a shader with values: "ps3/wave" can be scene.glsl as a wave). Options are live
//! values, not constants: switching between presets of one shader morphs every value to the new
//! one (elements leaving go first, elements coming arrive late), so its elements turn into each
//! other. `#import "lib/x"` takes in a building block (from the pack, then from shaders/), once.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

/// Options a shader can have (live values in the uniform buffer).
pub const MAX_OPTIONS: usize = 32;

/// The quad: the panel's rectangle in pixels, made clip space.
const VERTEX_WGSL: &str = r#"
struct U { rect: vec4<f32>, screen: vec4<f32>, panel: vec4<f32>, accent: vec4<f32>, misc: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var c = array<vec2<f32>, 6>(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    let p = u.rect.xy + c[i] * u.rect.zw;
    return vec4(p.x / u.screen.x * 2.0 - 1.0, 1.0 - p.y / u.screen.y * 2.0, 0.0, 1.0);
}
"#;

/// What wraps the file's code: the uniforms as Shadertoy names, then the rounded cut.
const HEAD: &str = "#version 450
layout(set = 0, binding = 0) uniform U { vec4 rect; vec4 screen; vec4 panel; vec4 accent; vec4 misc; vec4 zash_opt[8]; };
layout(location = 0) out vec4 zash_out;
vec3 iResolution;
float iTime;
vec4 iPanel;
vec4 iAccent;
float iFocus;
// the panel's bottom-left in the window (pixels, y up) and the window's size: a scene drawn at
// fragCoord + iOrigin stays put while the panel moves over it, like a camera
vec2 iOrigin;
vec2 iScreen;
// a clock at the speed the option `speed` says (1 without one): smooth when speed changes or morphs
float iPhase;
";

const TAIL: &str = "
void main() {
    iResolution = vec3(rect.zw, 1.0);
    iTime = misc.x;
    iFocus = misc.y;
    iPanel = panel;
    iAccent = accent;
    iScreen = screen.xy;
    iOrigin = vec2(rect.x, screen.y - rect.y - rect.w);
    iPhase = screen.z;
    vec2 local = gl_FragCoord.xy - rect.xy;
    vec4 c = vec4(0.0);
    mainImage(c, vec2(local.x, rect.w - local.y));
    // the panel's rounded corners
    vec2 q = abs(local - rect.zw * 0.5) - rect.zw * 0.5 + misc.w;
    float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - misc.w;
    zash_out = vec4(c.rgb, clamp(0.5 - d, 0.0, 1.0) * clamp(c.a, 0.0, 1.0) * misc.z);
}
";

/// Where and how a panel is drawn this frame.
pub struct Place {
    /// x, y, w, h in pixels
    pub rect: [f32; 4],
    pub screen: [f32; 2],
    pub panel: [f32; 4],
    pub accent: [f32; 4],
    pub focus: bool,
    pub opacity: f32,
    pub radius: f32,
}

/// A panel shader as picked: the file, the preset it is drawn as, options set in zash.toml.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Choice {
    pub path: PathBuf,
    /// a preset's values (pack.toml [presets.x])
    pub preset: Vec<(String, f32)>,
    /// name → value from zash.toml ([zfloat.shader_options."pack/shader"]), as written
    pub options: Vec<(String, String)>,
}

/// A package's pack.toml (next to its shaders): each shader's options, and presets.
#[derive(serde::Deserialize, Default, Clone)]
struct Pack {
    #[serde(default)]
    shaders: HashMap<String, PackShader>,
    #[serde(default)]
    presets: HashMap<String, Preset>,
}

#[derive(serde::Deserialize, Default, Clone)]
struct PackShader {
    #[serde(default)]
    options: BTreeMap<String, PackOption>,
}

#[derive(serde::Deserialize, Clone)]
struct PackOption {
    default: f32,
    min: Option<f32>,
    max: Option<f32>,
    /// when it changes in a morph: "early" (first 60 %), "late" (last 60 %), "even" (all of it);
    /// unset: early when its element leaves (goes to 0), late when one comes (from 0), else even
    morph: Option<String>,
}

/// A shader of the pack with values: "ps3/wave" = scene.glsl as a wave.
#[derive(serde::Deserialize, Default, Clone)]
struct Preset {
    shader: String,
    #[serde(default)]
    values: HashMap<String, f32>,
}

fn shaders_dir() -> PathBuf {
    PathBuf::from(format!("{}/.config/zash/shaders", std::env::var("HOME").unwrap_or_default()))
}

fn read_pack(dir: &Path) -> Result<Pack, String> {
    let f = dir.join("pack.toml");
    match std::fs::read_to_string(&f) {
        Ok(t) => toml::from_str(&t).map_err(|e| format!("{}: {e}", f.display())),
        Err(_) => Ok(Pack::default()),
    }
}

/// The options a file has (pack.toml, in name order: their slots).
fn options_of(pack: &Pack, path: &Path) -> Vec<(String, PackOption)> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    pack.shaders.get(stem).map(|s| s.options.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default()
}

/// "ps3/wave" → a preset of the ps3 pack, or ~/.config/zash/shaders/ps3/wave.glsl; a path as it is.
fn resolve_with(s: &str, options: Vec<(String, String)>, pack_of: &mut dyn FnMut(&Path) -> Pack) -> Choice {
    let home = std::env::var("HOME").unwrap_or_default();
    if let Some(r) = s.strip_prefix("~/") {
        return Choice { path: PathBuf::from(format!("{home}/{r}")), preset: Vec::new(), options };
    }
    if s.starts_with('/') || s.ends_with(".glsl") {
        return Choice { path: PathBuf::from(s), preset: Vec::new(), options };
    }
    let path = shaders_dir().join(format!("{s}.glsl"));
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
    match pack_of(&dir).presets.get(&name) {
        Some(p) => Choice { path: dir.join(format!("{}.glsl", p.shader)), preset: p.values.iter().map(|(k, v)| (k.clone(), *v)).collect(), options },
        None => Choice { path, preset: Vec::new(), options },
    }
}

impl Choice {
    /// Resolves a name, reading its pack.toml (Shaders::resolve keeps them).
    #[allow(dead_code)] // the tests, and callers without a Shaders
    pub fn resolve(s: &str, options: Vec<(String, String)>) -> Choice {
        resolve_with(s, options, &mut |d| read_pack(d).unwrap_or_default())
    }
}

/// Each option's value for a choice (in slot order): the default, the preset's, zash.toml's, kept
/// within the option's range.
fn values_of(opts: &[(String, PackOption)], choice: &Choice) -> Vec<f32> {
    opts.iter()
        .map(|(name, o)| {
            let set = choice.options.iter().find(|(n, _)| n == name).and_then(|(_, v)| v.parse::<f32>().ok());
            let pre = choice.preset.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
            set.or(pre).unwrap_or(o.default).clamp(o.min.unwrap_or(f32::MIN), o.max.unwrap_or(f32::MAX))
        })
        .collect()
}

/// Between two sets of values, `t` (0–1) of the way, each as its option says (`timing`: "early",
/// "late", "even"); unset, an element leaving (going to 0) goes in the first 60 %, one coming (from
/// 0) in the last 60 %, the rest over the whole time. All eased.
pub fn morph(from: &[f32], to: &[f32], t: f32, timing: &[Option<String>]) -> Vec<f32> {
    let ease = |a: f32, b: f32, t: f32| {
        let x = ((t - a) / (b - a)).clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    to.iter()
        .enumerate()
        .map(|(i, &b)| {
            let a = from.get(i).copied().unwrap_or(b);
            let k = match timing.get(i).and_then(|m| m.as_deref()) {
                Some("early") => ease(0.0, 0.6, t),
                Some("late") => ease(0.4, 1.0, t),
                Some(_) => ease(0.0, 1.0, t),
                None if b == 0.0 && a != 0.0 => ease(0.0, 0.6, t),
                None if a == 0.0 && b != 0.0 => ease(0.4, 1.0, t),
                None => ease(0.0, 1.0, t),
            };
            a + (b - a) * k
        })
        .collect()
}

/// The file with its #import "x" lines replaced by the blocks (from the file's folder, then from
/// shaders/; each once, in the order first asked for).
fn with_imports(path: &Path, code: &str, seen: &mut HashSet<PathBuf>) -> Result<String, String> {
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    imports_from(&base, path, code, seen)
}

/// with_imports, names looked up from `base` (the shader's folder), then from shaders/.
fn imports_from(base: &Path, path: &Path, code: &str, seen: &mut HashSet<PathBuf>) -> Result<String, String> {
    let mut out = String::new();
    for line in code.lines() {
        let Some(name) = line.trim().strip_prefix("#import").map(|r| r.trim().trim_matches('"')) else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        let near = base.join(format!("{name}.glsl"));
        let file = if near.exists() { near } else { shaders_dir().join(format!("{name}.glsl")) };
        if !seen.insert(file.clone()) {
            continue;
        }
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: #import \"{name}\": {e}", path.display()))?;
        out.push_str(&imports_from(base, &file, &text, seen)?);
    }
    Ok(out)
}

/// Each option as a global (filled from its live slot at the start of main; a function's parameter of
/// the same name hides it, as GLSL allows), and the month (1–12, for shaders that change with it).
fn prelude(opts: &[(String, PackOption)], path: &Path) -> Result<String, String> {
    let mut out = format!("const int iMonth = {};\n", month());
    if opts.len() > MAX_OPTIONS {
        return Err(format!("{}: {} options, at most {MAX_OPTIONS}", path.display(), opts.len()));
    }
    for (i, (name, _)) in opts.iter().enumerate() {
        if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("{}: option {name:?}: letters, digits and _ only", path.display()));
        }
        let _ = i;
        out.push_str(&format!("float {name};\n"));
    }
    Ok(out)
}

/// This month, 1–12 (UTC; days → civil date).
fn month() -> u32 {
    let days = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0) as i64;
    let z = days + 719_468;
    let doe = z - z.div_euclid(146_097) * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    (if mp < 10 { mp + 3 } else { mp - 9 }) as u32
}

/// The whole fragment shader: the uniforms, the options, the file with its imports, the cut.
fn source(opts: &[(String, PackOption)], path: &Path, code: &str) -> Result<String, String> {
    let code = with_imports(path, code, &mut HashSet::new())?;
    let fill: String = opts.iter().enumerate().map(|(i, (name, _))| format!("    {name} = zash_opt[{}].{};\n", i / 4, ["x", "y", "z", "w"][i % 4])).collect();
    let tail = TAIL.replacen("void main() {\n", &format!("void main() {{\n{fill}"), 1);
    Ok(format!("{HEAD}{}\n#line 1\n{code}\n{tail}", prelude(opts, path)?))
}

pub struct PanelShader {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

impl PanelShader {
    /// Compiles a shader file. Errors come back as text (a bad file must not take zash down).
    fn load(device: &wgpu::Device, format: wgpu::TextureFormat, path: &Path, opts: &[(String, PackOption)]) -> Result<PanelShader, String> {
        let code = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !code.contains("mainImage") {
            return Err(format!("{}: no mainImage(out vec4, in vec2)", path.display()));
        }
        let source = source(opts, path, &code)?;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("zash panel vs"), source: wgpu::ShaderSource::Wgsl(VERTEX_WGSL.into()) });
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zash panel fs"),
            source: wgpu::ShaderSource::Glsl { shader: source.into(), stage: wgpu::naga::ShaderStage::Fragment, defines: &[] },
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 80 + 4 * MAX_OPTIONS as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl, entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }] });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], ..Default::default() });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zash panel shader"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &vs, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(format!("{}: {e}", path.display()));
        }
        Ok(PanelShader { pipeline, uniform, bind })
    }

    /// The place, the time and the options' values this frame.
    pub fn update(&self, queue: &wgpu::Queue, at: &Place, time: f32, phase: f32, values: &[f32]) {
        let mut v: Vec<f32> = vec![
            at.rect[0], at.rect[1], at.rect[2], at.rect[3],
            at.screen[0], at.screen[1], phase, 0.0,
            at.panel[0], at.panel[1], at.panel[2], at.panel[3],
            at.accent[0], at.accent[1], at.accent[2], at.accent[3],
            time, f32::from(u8::from(at.focus)), at.opacity, at.radius,
        ];
        v.extend((0..MAX_OPTIONS).map(|i| values.get(i).copied().unwrap_or(0.0)));
        queue.write_buffer(&self.uniform, 0, &v.iter().flat_map(|f| f.to_ne_bytes()).collect::<Vec<u8>>());
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// A morph under way on one zfloat: its file, the values it started from and goes to, when, how long.
#[derive(Default)]
struct Morph {
    path: PathBuf,
    from: Vec<f32>,
    to: Vec<f32>,
    timing: Vec<Option<String>>,
    start: f32,
    secs: f32,
    /// iPhase: the time integrated at the option `speed` (its slot), and when it was last moved on
    speed: Option<usize>,
    phase: f32,
    last: f32,
}

impl Morph {
    fn now(&self, t: f32) -> Vec<f32> {
        if self.secs <= 0.0 { self.to.clone() } else { morph(&self.from, &self.to, ((t - self.start) / self.secs).clamp(0.0, 1.0), &self.timing) }
    }
}

/// Shaders by file (compiled on first use and again when the file or its pack.toml changes), the
/// packs, and each zfloat's morph.
#[derive(Default)]
pub struct Shaders {
    loaded: HashMap<PathBuf, (Option<SystemTime>, Option<PanelShader>)>,
    packs: HashMap<PathBuf, Pack>,
    morphs: HashMap<u64, Morph>,
    checked: Option<Instant>,
    recheck: bool,
}

impl Shaders {
    /// A shader already compiled (no loading: for drawing).
    pub fn loaded(&self, path: &Path) -> Option<&PanelShader> {
        self.loaded.get(path).and_then(|(_, s)| s.as_ref())
    }

    fn pack(&mut self, dir: &Path) -> Pack {
        if self.recheck || !self.packs.contains_key(dir) {
            let p = read_pack(dir).unwrap_or_else(|e| {
                eprintln!("zash: panel shader: {e}");
                Pack::default()
            });
            self.packs.insert(dir.to_path_buf(), p);
        }
        self.packs.get(dir).cloned().unwrap_or_default()
    }

    /// Once a frame: files and packs are looked at again at most once a second.
    pub fn tick(&mut self) {
        self.recheck = self.checked.is_none_or(|t| t.elapsed().as_secs_f32() > 1.0);
        if self.recheck {
            self.checked = Some(Instant::now());
        }
    }

    /// A name ("ps3/wave", a path) as a choice.
    pub fn resolve(&mut self, s: &str, options: Vec<(String, String)>) -> Choice {
        resolve_with(s, options, &mut |d| self.pack(d))
    }

    /// The shader for a file, if it compiles (an error is printed once per version of the file).
    pub fn get(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat, path: &Path) -> Option<&PanelShader> {
        let stale = match self.loaded.get(path) {
            None => true,
            Some((t, _)) => self.recheck && *t != stamp(path),
        };
        if stale {
            let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let opts = options_of(&self.pack(&dir), path);
            let shader = match PanelShader::load(device, format, path, &opts) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("zash: panel shader: {e}");
                    None
                }
            };
            self.loaded.insert(path.to_path_buf(), (stamp(path), shader));
        }
        self.loaded.get(path).and_then(|(_, s)| s.as_ref())
    }

    /// The options' values for zfloat `id` at time `t`: a new choice of the same file morphs to its
    /// values over `secs` (from wherever a morph under way is); another file starts as it is.
    pub fn values(&mut self, id: u64, choice: &Choice, secs: f32, t: f32) -> (Vec<f32>, f32) {
        let dir = choice.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let opts = options_of(&self.pack(&dir), &choice.path);
        let target = values_of(&opts, choice);
        let timing: Vec<Option<String>> = opts.iter().map(|(_, o)| o.morph.clone()).collect();
        match self.morphs.get_mut(&id) {
            Some(m) if m.path == choice.path => {
                if m.to != target {
                    let now = m.now(t);
                    *m = Morph { from: now, to: target, timing, start: t, secs, ..std::mem::take(m) };
                }
            }
            _ => {
                let speed = opts.iter().position(|(n, _)| n == "speed");
                self.morphs.insert(id, Morph { path: choice.path.clone(), from: target.clone(), to: target, timing, start: t, secs: 0.0, speed, phase: t * 0.0, last: t });
            }
        }
        let Some(m) = self.morphs.get_mut(&id) else { return (Vec::new(), t) };
        let v = m.now(t);
        let rate = m.speed.and_then(|i| v.get(i).copied()).unwrap_or(1.0);
        m.phase += (t - m.last).clamp(0.0, 1.0) * rate;
        m.last = t;
        (v, m.phase)
    }

    /// A morph is under way (frames must keep coming even for a still shader).
    #[allow(dead_code)]
    pub fn morphing(&self, t: f32) -> bool {
        self.morphs.values().any(|m| m.secs > 0.0 && t < m.start + m.secs)
    }
}

/// The newer of the shader's and its pack.toml's change times.
fn stamp(path: &Path) -> Option<SystemTime> {
    let t = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    t(path).max(t(&path.with_file_name("pack.toml")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shader in ~/.config/zash/shaders (and its packages) compiles the way zash compiles it
    /// (blocks in a lib/ folder are imported, not shaders of their own).
    #[test]
    fn the_users_shaders_compile() {
        let dir = shaders_dir();
        let Ok(top) = std::fs::read_dir(&dir) else { return };
        let mut files = Vec::new();
        for p in top.flatten().map(|e| e.path()) {
            if p.is_dir() {
                files.extend(std::fs::read_dir(&p).into_iter().flatten().flatten().map(|e| e.path()));
            } else {
                files.push(p);
            }
        }
        for f in files.into_iter().filter(|p| p.extension().is_some_and(|x| x == "glsl")) {
            let code = std::fs::read_to_string(&f).unwrap();
            if !code.contains("mainImage") {
                continue;
            }
            let opts = options_of(&read_pack(f.parent().unwrap()).unwrap_or_else(|e| panic!("{e}")), &f);
            let src = source(&opts, &f, &code).unwrap_or_else(|e| panic!("{e}"));
            let module = wgpu::naga::front::glsl::Frontend::default()
                .parse(&wgpu::naga::front::glsl::Options::from(wgpu::naga::ShaderStage::Fragment), &src)
                .unwrap_or_else(|e| panic!("{}: {}", f.display(), e.emit_to_string(&src)));
            wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::all())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{}: {e:?}", f.display()));
        }
    }

    #[test]
    fn presets_and_options_give_the_values_within_range() {
        let dir = std::env::temp_dir().join(format!("zash-pack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pack.toml"), "[shaders.scene.options]\nspeed = { default = 1.0, min = 0.0, max = 2.0 }\ndust = { default = 0.0 }\n[presets.calm]\nshader = \"scene\"\nvalues = { speed = 0.25, dust = 1.0 }\n").unwrap();
        let pack = read_pack(&dir).unwrap();
        let path = dir.join("scene.glsl");
        let opts = options_of(&pack, &path);
        assert_eq!(opts.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["dust", "speed"], "slots in name order");
        let plain = Choice { path: path.clone(), preset: vec![], options: vec![("speed".into(), "9".into())] };
        assert_eq!(values_of(&opts, &plain), [0.0, 2.0], "the default, and zash.toml's clamped to max");
        let calm = Choice { path: path.clone(), preset: pack.presets["calm"].values.iter().map(|(k, v)| (k.clone(), *v)).collect(), options: vec![] };
        assert_eq!(values_of(&opts, &calm), [1.0, 0.25], "the preset's");
        let p = prelude(&opts, &path).unwrap();
        assert!(p.contains("float dust;") && p.contains("float speed;"), "{p}");
        let s = source(&opts, &path, "void mainImage(out vec4 c, in vec2 f) {}").unwrap();
        assert!(s.contains("dust = zash_opt[0].x;") && s.contains("speed = zash_opt[0].y;"), "filled from the slots in name order");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_morph_lets_leaving_elements_go_first_and_coming_ones_arrive_late() {
        // band leaves (1 → 0), dust comes (0 → 1), speed changes (1 → 3)
        let (from, to) = ([1.0, 0.0, 1.0], [0.0, 1.0, 3.0]);
        let at = |t| morph(&from, &to, t, &[]);
        assert_eq!(at(0.0), from.to_vec());
        assert_eq!(at(1.0), to.to_vec());
        let m = at(0.3);
        assert!(m[0] < 0.6, "the band is mostly gone by 30 %: {m:?}");
        assert_eq!(m[1], 0.0, "the dust has not started at 30 %");
        assert!(at(0.6)[0] == 0.0 && at(0.7)[1] > 0.0 && at(0.7)[1] < 1.0, "band gone at 60 %, dust arriving at 70 %");
        assert!((at(0.5)[2] - 2.0).abs() < 1e-5, "the rest eased over the whole time");
        // the pack can say otherwise: scatter (0 → 1) early, so the band breaks up before it goes
        let m = morph(&[0.0], &[1.0], 0.3, &[Some("early".into())]);
        assert!(m[0] >= 0.5, "early: halfway at 30 %: {m:?}");
    }

    #[test]
    fn imports_come_once() {
        let dir = std::env::temp_dir().join(format!("zash-import-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib/a.glsl"), "float a() { return 1.0; }\n").unwrap();
        std::fs::write(dir.join("lib/b.glsl"), "#import \"lib/a\"\nfloat b() { return a(); }\n").unwrap();
        let main = dir.join("main.glsl");
        let out = with_imports(&main, "#import \"lib/b\"\n#import \"lib/a\"\nvoid x() {}\n", &mut HashSet::new()).unwrap();
        assert_eq!(out.matches("float a()").count(), 1);
        assert!(out.find("float a()").unwrap() < out.find("float b()").unwrap(), "a block before what uses it");
        std::fs::remove_dir_all(&dir).ok();
    }
}

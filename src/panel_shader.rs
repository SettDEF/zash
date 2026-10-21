//! GLSL shaders as a zfloat's background (`[zfloat.shaders]` in zash.toml). Shadertoy style:
//! the file defines `void mainImage(out vec4 fragColor, in vec2 fragCoord)` and may read
//! `iResolution`, `iTime`, plus `iPanel` (the panel's colour), `iAccent` and `iFocus` (1 while it
//! has the keyboard). zash cuts it to the panel's rounded corners; text and buttons stay on top.
//! A shader that does not compile is reported once and the panel stays plain.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

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
layout(set = 0, binding = 0) uniform U { vec4 rect; vec4 screen; vec4 panel; vec4 accent; vec4 misc; };
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


/// A panel shader as zash.toml picks it: the file, and options over its package's defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Choice {
    pub path: PathBuf,
    /// name → value from zash.toml ([zfloat.shader_options."pack/shader"]), as written
    pub options: Vec<(String, String)>,
}

impl Choice {
    /// "ps3/wave" → ~/.config/zash/shaders/ps3/wave.glsl; a path (~/…, /…, ….glsl) as it is.
    pub fn resolve(s: &str, options: Vec<(String, String)>) -> Choice {
        let home = std::env::var("HOME").unwrap_or_default();
        let path = if let Some(r) = s.strip_prefix("~/") {
            PathBuf::from(format!("{home}/{r}"))
        } else if s.starts_with('/') || s.ends_with(".glsl") {
            PathBuf::from(s)
        } else {
            PathBuf::from(format!("{home}/.config/zash/shaders/{s}.glsl"))
        };
        Choice { path, options }
    }
}

/// A package's pack.toml (next to its shaders): what it is, and each shader's options.
#[derive(serde::Deserialize, Default)]
struct Pack {
    #[serde(default)]
    shaders: std::collections::HashMap<String, PackShader>,
}

#[derive(serde::Deserialize, Default)]
struct PackShader {
    #[serde(default)]
    options: std::collections::BTreeMap<String, PackOption>,
}

#[derive(serde::Deserialize)]
struct PackOption {
    default: f32,
    min: Option<f32>,
    max: Option<f32>,
}

/// `const float name = value;` for each option of the shader (its pack's default, or zash.toml's
/// value within the pack's range), and the month (1–12, for shaders that change with it).
fn prelude(choice: &Choice) -> Result<String, String> {
    let mut out = format!("const int iMonth = {};\n", month());
    let pack_file = choice.path.with_file_name("pack.toml");
    let pack: Pack = match std::fs::read_to_string(&pack_file) {
        Ok(t) => toml::from_str(&t).map_err(|e| format!("{}: {e}", pack_file.display()))?,
        Err(_) => Pack::default(),
    };
    let stem = choice.path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let opts = pack.shaders.get(stem).map(|s| &s.options);
    for (name, _) in &choice.options {
        if opts.is_none_or(|o| !o.contains_key(name)) {
            eprintln!("zash: panel shader {}: no option {name} (pack.toml)", choice.path.display());
        }
    }
    for (name, o) in opts.into_iter().flatten() {
        if !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("{}: option {name:?}: letters, digits and _ only", pack_file.display()));
        }
        let set = choice.options.iter().find(|(n, _)| n == name).and_then(|(_, v)| v.parse::<f32>().ok());
        let v = set.unwrap_or(o.default).clamp(o.min.unwrap_or(f32::MIN), o.max.unwrap_or(f32::MAX));
        out.push_str(&format!("const float {name} = {v:.6};\n"));
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

/// The whole fragment shader: the uniforms, the options, the file, the rounded cut.
fn source(choice: &Choice, code: &str) -> Result<String, String> {
    Ok(format!("{HEAD}{}\n#line 1\n{code}\n{TAIL}", prelude(choice)?))
}

pub struct PanelShader {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

impl PanelShader {
    /// Compiles a shader file. Errors come back as text (a bad file must not take zash down).
    pub fn load(device: &wgpu::Device, format: wgpu::TextureFormat, choice: &Choice) -> Result<PanelShader, String> {
        let path = choice.path.as_path();
        let code = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !code.contains("mainImage") {
            return Err(format!("{}: no mainImage(out vec4, in vec2)", path.display()));
        }
        let source = source(choice, &code)?;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("zash panel vs"), source: wgpu::ShaderSource::Wgsl(VERTEX_WGSL.into()) });
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zash panel fs"),
            source: wgpu::ShaderSource::Glsl { shader: source.into(), stage: wgpu::naga::ShaderStage::Fragment, defines: &[] },
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 80, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
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

    pub fn update(&self, queue: &wgpu::Queue, at: &Place, time: f32) {
        let v: [f32; 20] = [
            at.rect[0], at.rect[1], at.rect[2], at.rect[3],
            at.screen[0], at.screen[1], 0.0, 0.0,
            at.panel[0], at.panel[1], at.panel[2], at.panel[3],
            at.accent[0], at.accent[1], at.accent[2], at.accent[3],
            time, f32::from(u8::from(at.focus)), at.opacity, at.radius,
        ];
        queue.write_buffer(&self.uniform, 0, &v.iter().flat_map(|f| f.to_ne_bytes()).collect::<Vec<u8>>());
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..6, 0..1);
    }
}

/// Shaders by choice (file + options), compiled on first use and again when the file or its
/// package's pack.toml changes.
#[derive(Default)]
pub struct Shaders {
    loaded: std::collections::HashMap<Choice, (Option<SystemTime>, Option<PanelShader>)>,
    checked: Option<Instant>,
}

impl Shaders {
    /// A shader already compiled (no loading: for drawing).
    pub fn loaded(&self, choice: &Choice) -> Option<&PanelShader> {
        self.loaded.get(choice).and_then(|(_, s)| s.as_ref())
    }

    /// The shader for a choice, if it compiles (an error is printed once per version of the file).
    pub fn get(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat, choice: &Choice) -> Option<&PanelShader> {
        // a file edited while it is shown: looked at again at most once a second
        let recheck = self.checked.is_none_or(|t| t.elapsed().as_secs_f32() > 1.0);
        if recheck {
            self.checked = Some(Instant::now());
        }
        let stamp = || stamp(choice);
        let stale = match self.loaded.get(choice) {
            None => true,
            Some((t, _)) => recheck && *t != stamp(),
        };
        if stale {
            let shader = match PanelShader::load(device, format, choice) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("zash: panel shader: {e}");
                    None
                }
            };
            self.loaded.insert(choice.clone(), (stamp(), shader));
        }
        self.loaded.get(choice).and_then(|(_, s)| s.as_ref())
    }
}

/// The newer of the shader's and its pack.toml's change times.
fn stamp(choice: &Choice) -> Option<SystemTime> {
    let t = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    t(&choice.path).max(t(&choice.path.with_file_name("pack.toml")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shader in ~/.config/zash/shaders (and its packages) compiles the way zash compiles it.
    #[test]
    fn the_users_shaders_compile() {
        let dir = std::env::var("HOME").map(|h| PathBuf::from(h).join(".config/zash/shaders")).unwrap();
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
            let choice = Choice { path: f.clone(), options: Vec::new() };
            let src = source(&choice, &std::fs::read_to_string(&f).unwrap()).unwrap_or_else(|e| panic!("{e}"));
            let module = wgpu::naga::front::glsl::Frontend::default()
                .parse(&wgpu::naga::front::glsl::Options::from(wgpu::naga::ShaderStage::Fragment), &src)
                .unwrap_or_else(|e| panic!("{}: {}", f.display(), e.emit_to_string(&src)));
            wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::all())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{}: {e:?}", f.display()));
        }
    }

    #[test]
    fn options_come_from_the_pack_within_its_range() {
        let dir = std::env::temp_dir().join(format!("zash-pack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pack.toml"), "name = \"t\"\n[shaders.wave.options]\nspeed = { default = 1.0, min = 0.0, max = 2.0 }\ncamera = { default = 0.5 }\n").unwrap();
        let path = dir.join("wave.glsl");
        let p = prelude(&Choice { path: path.clone(), options: vec![("speed".into(), "9".into())] }).unwrap();
        assert!(p.contains("const float speed = 2.000000;"), "clamped to max: {p}");
        assert!(p.contains("const float camera = 0.500000;"), "the default: {p}");
        assert!(p.contains("const int iMonth = "));
        assert_eq!(Choice::resolve("ps3/wave", vec![]).path, PathBuf::from(format!("{}/.config/zash/shaders/ps3/wave.glsl", std::env::var("HOME").unwrap())));
        std::fs::remove_dir_all(&dir).ok();
    }
}

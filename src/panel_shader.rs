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
";

const TAIL: &str = "
void main() {
    iResolution = vec3(rect.zw, 1.0);
    iTime = misc.x;
    iFocus = misc.y;
    iPanel = panel;
    iAccent = accent;
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

pub struct PanelShader {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

impl PanelShader {
    /// Compiles a shader file. Errors come back as text (a bad file must not take zash down).
    pub fn load(device: &wgpu::Device, format: wgpu::TextureFormat, path: &Path) -> Result<PanelShader, String> {
        let code = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !code.contains("mainImage") {
            return Err(format!("{}: no mainImage(out vec4, in vec2)", path.display()));
        }
        let source = format!("{HEAD}\n#line 1\n{code}\n{TAIL}");
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

/// Shaders by file, compiled on first use and again when the file changes.
#[derive(Default)]
pub struct Shaders {
    loaded: std::collections::HashMap<PathBuf, (Option<SystemTime>, Option<PanelShader>)>,
    checked: Option<Instant>,
}

impl Shaders {
    /// A shader already compiled (no loading: for drawing).
    pub fn loaded(&self, path: &Path) -> Option<&PanelShader> {
        self.loaded.get(path).and_then(|(_, s)| s.as_ref())
    }

    /// The shader for a file, if it compiles (an error is printed once per version of the file).
    pub fn get(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat, path: &Path) -> Option<&PanelShader> {
        // a file edited while it is shown: looked at again at most once a second
        let recheck = self.checked.is_none_or(|t| t.elapsed().as_secs_f32() > 1.0);
        if recheck {
            self.checked = Some(Instant::now());
        }
        let stamp = || std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let stale = match self.loaded.get(path) {
            None => true,
            Some((t, _)) => recheck && *t != stamp(),
        };
        if stale {
            let shader = match PanelShader::load(device, format, path) {
                Ok(s) => Some(s),
                Err(e) => {
                    eprintln!("zash: panel shader: {e}");
                    None
                }
            };
            self.loaded.insert(path.to_path_buf(), (stamp(), shader));
        }
        self.loaded.get(path).and_then(|(_, s)| s.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shader in ~/.config/zash/shaders compiles the way zash compiles it.
    #[test]
    fn the_users_shaders_compile() {
        let dir = std::env::var("HOME").map(|h| PathBuf::from(h).join(".config/zash/shaders")).unwrap();
        let Ok(files) = std::fs::read_dir(&dir) else { return };
        for f in files.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "glsl")) {
            let src = format!("{HEAD}{}{TAIL}", std::fs::read_to_string(&f).unwrap());
            let module = wgpu::naga::front::glsl::Frontend::default()
                .parse(&wgpu::naga::front::glsl::Options::from(wgpu::naga::ShaderStage::Fragment), &src)
                .unwrap_or_else(|e| panic!("{}: {}", f.display(), e.emit_to_string(&src)));
            wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::all())
                .validate(&module)
                .unwrap_or_else(|e| panic!("{}: {e:?}", f.display()));
        }
    }
}

//! A GLSL shader over the shadows of programs' floats (zett's popups): `[layers] shader = "melt"`
//! (a built-in) or a file (in ~/.config/zash/shaders, or a path). It runs where the shadows fall,
//! on the terminal as drawn, so it can bend, smear or recolour the text under them.
//!
//! The file defines `void mainShadow(out vec4 fragColor, in vec2 px)`, px in window pixels (y
//! down), and gets for that pixel what zash worked out from the floats:
//!   zShadow    the shadow there (0..1), the same soft rounded shape zash draws
//!   zDist      signed distance to the nearest float's edge in pixels (< 0 inside it)
//!   zDir       the way to that float (unit vector)
//!   zFloat     that float's rectangle (x, y, w, h) and zProgress how far it has come (0..1)
//!   iTime, iResolution, zBackground (the window's colour), zCell (the cell height)
//!   scene(px)  the terminal there; zPlain(px) the same without its text (cell backgrounds)
//!   zText(px)  how much a pixel is text: exact, where the two differ (any colour, any background)
//!   zInFloat(p) whether a point is on a float; textMask(c) a guess from a colour alone
//! What it returns replaces the pixel (the window's alpha is kept). Inside the floats nothing
//! is drawn; a shader that does not compile is reported once and the plain shadow is drawn.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// At most this many floats at once (the rest get no effect).
pub const MAX_FLOATS: usize = 8;

/// The shader zash has with it: the shadow melts the text, drips running down in its colour.
pub const MELT: &str = include_str!("../shaders/melt.glsl");

const VERTEX_WGSL: &str = r#"
@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var xy = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(xy[i], 0.0, 1.0);
}
"#;

const HEAD: &str = "#version 450
layout(set = 0, binding = 0) uniform U { vec4 zash_screen; vec4 zash_bg; vec4 zash_params; vec4 zash_rects[8]; vec4 zash_prog[8]; };
layout(set = 0, binding = 1) uniform texture2D zash_scene;
layout(set = 0, binding = 2) uniform sampler zash_smp;
// the same frame drawn without its text: what is under each letter
layout(set = 0, binding = 3) uniform texture2D zash_plain;
layout(location = 0) out vec4 zash_out;
vec3 iResolution;
float iTime;
vec4 zBackground;
float zCell;
float zShadow;
float zDist;
vec2 zDir;
vec4 zFloat;
float zProgress;
// the terminal as drawn, at a window pixel (y down)
vec4 scene(vec2 px) { return texture(sampler2D(zash_scene, zash_smp), px / zash_screen.xy); }
// the terminal there without its text (cell backgrounds, highlights, images)
vec4 zPlain(vec2 px) { return texture(sampler2D(zash_plain, zash_smp), px / zash_screen.xy); }
// how much a pixel is text (0..1): where the frame with text differs from the one without, so
// any colour of text counts, on any background
float zText(vec2 px) { return smoothstep(0.004, 0.06, distance(scene(px).rgb, zPlain(px).rgb)); }
// how much a colour is text rather than the window's background (0..1); zText(px) is exact
float textMask(vec4 c) { return smoothstep(0.05, 0.22, distance(c.rgb, zBackground.rgb)); }
// a soft rounded box as zash's rectangles draw it: radius, softness (the shadow's shape)
float zash_soft_box(vec2 px, vec4 r, float radius, float soft) {
    vec2 half_ = r.zw * 0.5;
    vec2 local = px - (r.xy + half_);
    float rr = min(radius + soft, min(half_.x, half_.y));
    vec2 q = abs(local) - half_ + vec2(rr);
    float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - rr;
    float s = max(soft, 0.6);
    return 1.0 - smoothstep(-s, s, d + soft);
}
// a point inside one of the floats (their own text is not the shadow's to change)
bool zInFloat(vec2 p) {
    for (int i = 0; i < 8; i++) {
        if (i >= int(zash_screen.w)) { break; }
        vec4 r = zash_rects[i];
        if (p.x >= r.x && p.x < r.x + r.z && p.y >= r.y && p.y < r.y + r.w) { return true; }
    }
    return false;
}
float zash_box_dist(vec2 px, vec4 r) {
    vec2 half_ = r.zw * 0.5;
    vec2 q = abs(px - (r.xy + half_)) - half_;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0);
}
#line 1
";

const TAIL: &str = "
void main() {
    vec2 px = gl_FragCoord.xy;
    iResolution = vec3(zash_screen.xy, 1.0);
    iTime = zash_screen.z;
    zBackground = zash_bg;
    zCell = zash_params.x;
    float strength = zash_params.y;
    int n = int(zash_screen.w);
    float ch = zash_params.x;
    float around = floor(ch * 0.25 + 0.5);
    float soft = ch * 0.9;
    float drop = ch * 0.4;
    // the shadows of all the floats together, and the nearest float
    float clear = 1.0;
    float best = 1e9;
    int near = 0;
    bool inside = false;
    for (int i = 0; i < 8; i++) {
        if (i >= n) { break; }
        vec4 r = zash_rects[i];
        float a = zash_prog[i].x;
        if (px.x >= r.x && px.x < r.x + r.z && px.y >= r.y && px.y < r.y + r.w) { inside = true; }
        float amb = 0.12 * strength * a * zash_soft_box(px, vec4(r.xy - 2.0 * around, r.zw + 4.0 * around), 2.0, around);
        float cast = 0.45 * strength * a * zash_soft_box(px, vec4(r.x - soft * 1.5, r.y + drop - soft * 1.5, r.z + soft * 3.0, r.w + soft * 3.0), 2.0, soft);
        clear *= (1.0 - amb) * (1.0 - cast);
        float d = zash_box_dist(px, r);
        if (d < best) { best = d; near = i; }
    }
    if (inside || n == 0) { discard; }
    zShadow = 1.0 - clear;
    if (zShadow < 0.003) { discard; }
    zFloat = zash_rects[near];
    zProgress = zash_prog[near].x;
    zDist = best;
    vec2 c = zFloat.xy + zFloat.zw * 0.5;
    vec2 q = clamp(px, zFloat.xy, zFloat.xy + zFloat.zw);
    vec2 to = q - px;
    zDir = length(to) > 0.001 ? normalize(to) : normalize(c - px + vec2(0.0001));
    vec4 col = vec4(0.0);
    mainShadow(col, px);
    zash_out = vec4(col.rgb, scene(px).a);
}
";

/// The compiled shader, from which file and when (it is compiled again when the file changes).
pub struct LayerFx {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    pub source: String,
    stamp: Option<SystemTime>,
}

/// `melt` (built in), a name in ~/.config/zash/shaders (with or without .glsl), or a path.
pub fn resolve(name: &str) -> Option<PathBuf> {
    if name == "melt" {
        return None;
    }
    let p = PathBuf::from(name);
    if p.is_absolute() {
        return Some(p);
    }
    let dir = crate::config::Config::config_dir().join("shaders");
    let with = if name.ends_with(".glsl") { dir.join(name) } else { dir.join(format!("{name}.glsl")) };
    Some(with)
}

fn stamp(p: Option<&Path>) -> Option<SystemTime> {
    p.and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
}

impl LayerFx {
    pub fn load(device: &wgpu::Device, format: wgpu::TextureFormat, name: &str) -> Result<LayerFx, String> {
        let path = resolve(name);
        let code = match &path {
            None => MELT.to_string(),
            Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
        };
        if !code.contains("mainShadow") {
            return Err(format!("{name}: no mainShadow(out vec4, in vec2)"));
        }
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("zash layer fx vs"), source: wgpu::ShaderSource::Wgsl(VERTEX_WGSL.into()) });
        let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("zash layer fx"),
            source: wgpu::ShaderSource::Glsl { shader: format!("{HEAD}{code}\n{TAIL}").into(), stage: wgpu::naga::ShaderStage::Fragment, defines: &[] },
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], ..Default::default() });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("zash layer fx"),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &vs, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState { module: &fs, entry_point: Some("main"), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(format!("{name}: {e}"));
        }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, ..Default::default() });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (3 + 2 * MAX_FLOATS as u64) * 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        Ok(LayerFx { pipeline, layout, sampler, uniform, source: name.to_string(), stamp: stamp(path.as_deref()) })
    }

    /// The file changed since it was compiled.
    pub fn stale(&self) -> bool {
        let p = resolve(&self.source);
        p.is_some() && stamp(p.as_deref()) != self.stamp
    }

    /// The floats (pixels, how far each has come), the window, the time; drawn over `target`
    /// from `scene` (the frame as it was drawn), only inside `scissor`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, device: &wgpu::Device, queue: &wgpu::Queue, enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, scene: &wgpu::TextureView, plain: &wgpu::TextureView, floats: &[([f32; 4], f32)], screen: [f32; 2], time: f32, bg: [f32; 4], cell: f32, strength: f32, scissor: [u32; 4]) {
        let n = floats.len().min(MAX_FLOATS);
        let mut v: Vec<f32> = vec![screen[0], screen[1], time, n as f32, bg[0], bg[1], bg[2], bg[3], cell, strength, 0.0, 0.0];
        for i in 0..MAX_FLOATS {
            v.extend(floats.get(i).map(|f| f.0).unwrap_or([0.0; 4]));
        }
        for i in 0..MAX_FLOATS {
            v.extend([floats.get(i).map(|f| f.1).unwrap_or(0.0), 0.0, 0.0, 0.0]);
        }
        queue.write_buffer(&self.uniform, 0, &v.iter().flat_map(|f| f.to_ne_bytes()).collect::<Vec<u8>>());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(scene) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(plain) },
            ],
        });
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("zash layer fx"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let [x, y, w, h] = scissor;
        pass.set_scissor_rect(x, y, w.max(1), h.max(1));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

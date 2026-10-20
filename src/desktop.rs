//! `zash --desktop`: a terminal on the wallpaper. A layer-shell surface (no window), pinned to a
//! corner, click-through, drawn at the output's fractional scale. Its panel is only as opaque as
//! the wallpaper under it needs for the text to keep its contrast, measured again when the
//! wallpaper or the colours change. Hyprland blurs what is behind it (`layerrule … zash-desktop`).

use crate::config::{Config, DesktopConf, DesktopLayer, Readability};
use crate::render::{Frame, PaneView, Renderer};
use crate::session::{Grid, Session, Sink};
use alacritty_terminal::event::Event;
use std::os::fd::{AsFd, AsRawFd};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_callback, wl_compositor, wl_output, wl_region, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

/// The layer surface's namespace: what Hyprland's layer rules match.
pub const NAMESPACE: &str = "zash-desktop";

#[derive(Default)]
struct Output {
    name: String,
    /// physical pixels of the current mode
    mode: (i32, i32),
    /// logical size (xdg-output): what the screen is in the units surfaces are placed in
    logical: (i32, i32),
    scale: i32,
}

impl Output {
    /// Its scale before a surface is told one: the mode over the logical size (2560 / 1600 =
    /// 1.6), else wl_output's whole-number scale.
    fn scale(&self) -> f32 {
        if self.logical.0 > 0 && self.mode.0 > 0 {
            return self.mode.0 as f32 / self.logical.0 as f32;
        }
        self.scale.max(1) as f32
    }

    /// The screen in logical pixels: xdg-output's size, else the mode over the scale.
    fn screen(&self, scale: f32) -> (f32, f32) {
        if self.logical.0 > 0 && self.logical.1 > 0 {
            return (self.logical.0 as f32, self.logical.1 as f32);
        }
        ((self.mode.0 as f32 / scale).round(), (self.mode.1 as f32 / scale).round())
    }
}

#[derive(Default)]
struct State {
    outputs: Vec<(wl_output::WlOutput, Output)>,
    /// logical size from the last configure, not yet drawn at
    configured: Option<(u32, u32)>,
    /// preferred scale × 120 (wp_fractional_scale)
    scale120: Option<u32>,
    frame_done: bool,
    closed: bool,
}

/// `probe`: set it up but never draw (it stays unmapped, invisible), print what it would do, exit.
pub fn run(mut cfg: Config, cmd_override: Option<Vec<String>>, probe: bool) -> Result<(), String> {
    let d = cfg.desktop.clone();
    if let Some(pt) = d.font_size_pt {
        cfg.font_size_pt = pt;
    }
    let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland display: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let compositor: wl_compositor::WlCompositor = globals.bind(&qh, 4..=6, ()).map_err(|_| "the compositor has no wl_compositor")?;
    let shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).map_err(|_| "the compositor has no layer shell (wlr-layer-shell)")?;
    let fractional: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    let viewporter: Option<wp_viewporter::WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let mut state = State::default();
    for g in globals.contents().clone_list().into_iter().filter(|g| g.interface == "wl_output") {
        let o: wl_output::WlOutput = globals.registry().bind(g.name, g.version.min(4), &qh, ());
        state.outputs.push((o, Output { scale: 1, ..Default::default() }));
    }
    // the screens' logical sizes: the fractional scale is only known once a surface shows
    let xdg: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1> = globals.bind(&qh, 2..=3, ()).ok();
    let _xdg_outputs: Vec<zxdg_output_v1::ZxdgOutputV1> = xdg.iter().flat_map(|m| state.outputs.iter().enumerate().map(|(i, (o, _))| m.get_xdg_output(o, &qh, i)).collect::<Vec<_>>()).collect();
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    let output = match &d.output {
        Some(n) => match state.outputs.iter().position(|(_, o)| o.name == *n) {
            Some(i) => Some(i),
            None => return Err(format!("no monitor {n:?} (there are: {})", state.outputs.iter().map(|(_, o)| o.name.as_str()).collect::<Vec<_>>().join(", "))),
        },
        None => None,
    };

    let surface = compositor.create_surface(&qh, ());
    // click-through: an empty input region
    let region = compositor.create_region(&qh, ());
    surface.set_input_region(Some(&region));
    let layer = match d.layer {
        DesktopLayer::Background => zwlr_layer_shell_v1::Layer::Background,
        DesktopLayer::Bottom => zwlr_layer_shell_v1::Layer::Bottom,
    };
    let ls = shell.get_layer_surface(&surface, output.map(|i| &state.outputs[i].0), layer, NAMESPACE.into(), &qh, ());
    place(&ls, &d);
    ls.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
    let _frac = fractional.as_ref().map(|f| f.get_fractional_scale(&surface, &qh, ()));
    let viewport = viewporter.as_ref().map(|v| v.get_viewport(&surface, &qh, ()));
    surface.commit();
    while state.configured.is_none() && !state.closed {
        queue.blocking_dispatch(&mut state).map_err(|e| e.to_string())?;
    }
    if state.closed {
        return Ok(());
    }
    if probe {
        // the preferred scale comes with the first configure, or just after
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        let out = output.map(|i| &state.outputs[i].1).or(state.outputs.first().map(|o| &o.1));
        let scale = state.scale120.map(|v| v as f32 / 120.0).unwrap_or_else(|| out.map_or(1.0, Output::scale));
        let logical = state.configured.unwrap_or(d.size);
        let screen = out.map(|o| o.screen(scale));
        let wall = wallpaper();
        let region = wall.as_ref().zip(screen).and_then(|(w, s)| region_on_wallpaper(w, s, logical, &d));
        let panel = measure_panel(&cfg, &d, logical, out, scale);
        println!(
            "{}",
            serde_json::json!({
                "output": out.map(|o| o.name.clone()), "screen": screen, "scale": scale,
                "size": logical, "pixels": px(logical, scale),
                "wallpaper": wall, "region": region, "panel_opacity": panel,
                "fractional": fractional.is_some(), "viewporter": viewporter.is_some(),
            })
        );
        return Ok(());
    }

    // the GPU surface on the Wayland one
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let gpu_surface = {
        use raw_window_handle::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
        let disp = std::ptr::NonNull::new(conn.backend().display_ptr() as *mut _).ok_or("no display pointer")?;
        let win = std::ptr::NonNull::new(surface.id().as_ptr() as *mut _).ok_or("no surface pointer")?;
        // SAFETY: the connection and the surface live until run returns, after the GPU surface
        unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(disp))),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(win)),
            })
        }
        .map_err(|e| e.to_string())?
    };
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { compatible_surface: Some(&gpu_surface), power_preference: wgpu::PowerPreference::LowPower, ..Default::default() })).map_err(|e| format!("no GPU adapter: {e}"))?;
    let (device, queue_gpu) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).map_err(|e| e.to_string())?;
    let caps = gpu_surface.get_capabilities(&adapter);
    let format = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
    let alpha = [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::PostMultiplied].into_iter().find(|m| caps.alpha_modes.contains(m)).unwrap_or(caps.alpha_modes[0]);
    // never blocks while the compositor holds frames back (covered by a fullscreen window)
    let present = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) { wgpu::PresentMode::Mailbox } else { wgpu::PresentMode::Fifo };

    let scale_of = |s: &State| s.scale120.map(|v| v as f32 / 120.0).unwrap_or_else(|| output.map(|i| &s.outputs[i].1).or(s.outputs.first().map(|o| &o.1)).map_or(1.0, Output::scale));
    let mut scale = scale_of(&state);
    let mut logical = state.configured.take().unwrap_or(d.size);
    let mut renderer = Renderer::new(&device, &queue_gpu, format, &cfg, scale);
    let mut size_px = px(logical, scale);
    let mut sc = wgpu::SurfaceConfiguration { usage: wgpu::TextureUsages::RENDER_ATTACHMENT, format, width: size_px.0, height: size_px.1, present_mode: present, desired_maximum_frame_latency: 2, alpha_mode: alpha, view_formats: vec![], color_space: wgpu::SurfaceColorSpace::Auto };
    set_scale(&surface, viewport.as_ref(), logical, scale);
    gpu_surface.configure(&device, &sc);

    // the session, its events through a pipe the loop can wait on
    let (wake_r, wake_w) = pipe()?;
    let (tx, rx) = mpsc::channel::<Event>();
    let (fwd_tx, fwd_rx) = mpsc::channel::<Event>();
    {
        let w = wake_w;
        std::thread::spawn(move || {
            while let Ok(e) = rx.recv() {
                if fwd_tx.send(e).is_err() {
                    return;
                }
                unsafe { libc::write(w, b"t".as_ptr().cast(), 1) };
            }
        });
    }
    let grid = grid_of(&renderer, size_px);
    let cmd = cmd_override.unwrap_or_else(|| vec!["sh".into(), "-c".into(), d.command.clone().unwrap_or_else(default_command)]);
    let mut session = Session::spawn(Some(cmd), None, grid, cfg.scrollback, Sink::Channel(tx)).map_err(|e| e.to_string())?;

    // the wallpaper and the colours, followed: one stat every two seconds
    let watched = watched_files(&cfg);
    watch(watched, wake_w);
    let mut panel = measure_panel(&cfg, &d, logical, output.map(|i| &state.outputs[i].1).or(state.outputs.first().map(|o| &o.1)), scale);
    renderer.opacity = panel;

    let mut dirty = true;
    state.frame_done = true;
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let fd = conn.as_fd().as_raw_fd();
    loop {
        // a new size or scale from the compositor
        if let Some(l) = state.configured.take() {
            logical = l;
        }
        let s = scale_of(&state);
        let want = px(logical, s);
        if want != size_px || (s - scale).abs() > f32::EPSILON {
            if (s - scale).abs() > f32::EPSILON {
                scale = s;
                renderer.set_font(cfg.font_size_pt, cfg.line_height, cfg.padding, scale);
            }
            size_px = want;
            sc.width = size_px.0;
            sc.height = size_px.1;
            set_scale(&surface, viewport.as_ref(), logical, scale);
            gpu_surface.configure(&device, &sc);
            session.resize(grid_of(&renderer, size_px));
            dirty = true;
        }
        // draw: when something changed, the compositor wants a frame, and not faster than 60/s
        let wait_ms = if dirty && state.frame_done {
            let since = last_draw.elapsed();
            if since >= Duration::from_millis(16) {
                draw(&mut renderer, &device, &queue_gpu, &gpu_surface, &sc, &session, &cfg, size_px, &surface, &qh);
                state.frame_done = false;
                dirty = false;
                last_draw = Instant::now();
                -1
            } else {
                (16 - since.as_millis() as i32).max(1)
            }
        } else {
            -1
        };
        // an effect wants its next frame
        let wait_ms = match renderer.fx.due {
            Some(t) if wait_ms < 0 => {
                dirty = true;
                t.saturating_duration_since(Instant::now()).as_millis().min(1000) as i32
            }
            _ => wait_ms,
        };
        let _ = conn.flush();
        let guard = queue.prepare_read();
        let mut fds = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }, libc::pollfd { fd: wake_r, events: libc::POLLIN, revents: 0 }];
        if guard.is_some() {
            unsafe { libc::poll(fds.as_mut_ptr(), 2, wait_ms) };
        }
        if let Some(g) = guard {
            if fds[0].revents & libc::POLLIN != 0 {
                let _ = g.read();
            }
        }
        queue.dispatch_pending(&mut state).map_err(|e| e.to_string())?;
        if state.closed {
            return Ok(());
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 64];
            let n = unsafe { libc::read(wake_r, buf.as_mut_ptr().cast(), buf.len()) };
            if buf[..n.max(0) as usize].contains(&b'w') {
                // the wallpaper or the colours changed
                if let Ok(s) = std::fs::read_to_string(&cfg.colors_file) {
                    cfg.palette = crate::config::Palette::from_tinct_lua(&s);
                }
                panel = measure_panel(&cfg, &d, logical, output.map(|i| &state.outputs[i].1).or(state.outputs.first().map(|o| &o.1)), scale);
                renderer.opacity = panel;
                renderer.invalidate();
                dirty = true;
            }
        }
        while let Ok(e) = fwd_rx.try_recv() {
            match e {
                Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => dirty = true,
                Event::PtyWrite(s) => session.write(s.into_bytes()),
                Event::TextAreaSizeRequest(f) => session.write(f(session.grid.window_size()).into_bytes()),
                Event::ChildExit(_) | Event::Exit => return Ok(()),
                _ => {}
            }
        }
    }
}

/// zeit's "desktop" session, made the first time; `zeit attach -t desktop` in a window types in it.
fn default_command() -> String {
    let zeit = format!("{}/.local/bin/zeit", std::env::var("HOME").unwrap_or_default());
    format!("unset ZEIT ZEIT_PANE; {zeit} attach -t desktop 2>/dev/null || exec {zeit} new -s desktop")
}

/// Anchors, size and margins for the configured corner.
fn place(ls: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, d: &DesktopConf) {
    use zwlr_layer_surface_v1::Anchor as A;
    let (l, r, t, b) = d.anchor.edges();
    let mut a = A::empty();
    for (on, bit) in [(l, A::Left), (r, A::Right), (t, A::Top), (b, A::Bottom)] {
        if on {
            a |= bit;
        }
    }
    ls.set_anchor(a);
    ls.set_size(d.size.0, d.size.1);
    let (mx, my) = d.margin;
    ls.set_margin(if t { my } else { 0 }, if r { mx } else { 0 }, if b { my } else { 0 }, if l { mx } else { 0 });
    // keep out of the bar's space, but take none
    ls.set_exclusive_zone(0);
}

fn px(logical: (u32, u32), scale: f32) -> (u32, u32) {
    (((logical.0 as f32) * scale).round().max(1.0) as u32, ((logical.1 as f32) * scale).round().max(1.0) as u32)
}

/// Buffers at the real pixel size: a viewport maps them back (fractional), else a buffer scale.
fn set_scale(surface: &wl_surface::WlSurface, viewport: Option<&wp_viewport::WpViewport>, logical: (u32, u32), scale: f32) {
    match viewport {
        Some(v) => {
            surface.set_buffer_scale(1);
            v.set_destination(logical.0 as i32, logical.1 as i32);
        }
        None => surface.set_buffer_scale(scale.round().max(1.0) as i32),
    }
}

fn grid_of(r: &Renderer, size: (u32, u32)) -> Grid {
    let (cols, rows) = r.grid_in(size.0 as f32, size.1 as f32);
    Grid { cols, rows, cell_w: r.cell_w, cell_h: r.cell_h }
}

#[allow(clippy::too_many_arguments)]
fn draw(r: &mut Renderer, device: &wgpu::Device, queue: &wgpu::Queue, surface: &wgpu::Surface, sc: &wgpu::SurfaceConfiguration, session: &Session, cfg: &Config, size: (u32, u32), wl: &wl_surface::WlSurface, qh: &QueueHandle<State>) {
    let tex = match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            surface.configure(device, sc);
            return;
        }
        _ => return,
    };
    let frame = {
        let t = session.term.lock();
        let marks = session.marks.lock().unwrap();
        let pal = if session.reverse.load(std::sync::atomic::Ordering::Relaxed) { cfg.palette.reversed() } else { cfg.palette.clone() };
        let mut f = Frame::extract(&t, &pal).with_prompts(&t, &marks).with_rows(&t, &session.ui_rows.lock().unwrap()).with_images(&t, &session.graphics.lock().unwrap());
        f.apply_cursor(&cfg.cursor, cfg.palette.cursor_fg);
        f
    };
    let view = tex.texture.create_view(&Default::default());
    let pane = PaneView { id: 0, x: 0.0, y: 0.0, w: size.0 as f32, h: size.1 as f32, frame: &frame, focused: true };
    // the frame callback rides on the commit the present makes
    wl.frame(qh, ());
    r.render(device, queue, &view, size.0, size.1, &[pane]);
    queue.present(tex);
}

fn pipe() -> Result<(i32, i32), String> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
        return Err("no pipe".into());
    }
    Ok((fds[0], fds[1]))
}

fn state_file() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"));
    base.join("quickshell/user/generated/wallpaper/path.txt")
}

/// The wallpaper the shell shows (its path file names it).
fn wallpaper() -> Option<PathBuf> {
    let p = std::fs::read_to_string(state_file()).ok()?;
    Some(PathBuf::from(p.trim())).filter(|p| p.exists())
}

fn watched_files(cfg: &Config) -> Vec<PathBuf> {
    let mut v = vec![cfg.colors_file.clone(), state_file()];
    v.extend(wallpaper());
    v
}

fn watch(paths: Vec<PathBuf>, wake: i32) {
    std::thread::spawn(move || {
        let stamp = |paths: &[PathBuf]| paths.iter().map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok()).collect::<Vec<_>>();
        let mut paths = paths;
        let mut last = stamp(&paths);
        loop {
            std::thread::sleep(Duration::from_secs(2));
            // a new wallpaper is a new file: follow it
            if let Some(w) = wallpaper().filter(|w| !paths.contains(w)) {
                paths.truncate(2);
                paths.push(w);
            }
            let now = stamp(&paths);
            if now != last {
                last = now;
                unsafe { libc::write(wake, b"w".as_ptr().cast(), 1) };
            }
        }
    });
}

// ── readability ──

/// The panel opacity for the wallpaper under the surface (or the fixed one).
fn measure_panel(cfg: &Config, d: &DesktopConf, logical: (u32, u32), out: Option<&Output>, scale: f32) -> f32 {
    let Readability::Auto = d.readability else {
        let Readability::Fixed(v) = d.readability else { unreachable!() };
        return v;
    };
    let fallback = 0.6f32.max(d.min_opacity);
    let (Some(wall), Some(out)) = (wallpaper(), out) else { return fallback };
    let screen = out.screen(scale);
    let Some(region) = region_on_wallpaper(&wall, screen, logical, d) else { return fallback };
    match histogram(&wall, region) {
        Some(h) => panel_opacity(&h, cfg.palette.fg.f32(), cfg.palette.bg.f32(), d.contrast, d.min_opacity),
        None => fallback,
    }
}

/// The part of the wallpaper image that can be behind the surface on any workspace: the shell
/// zooms the image to cover the screen and slides it with the workspaces (parallax), so the
/// region spans every position it slides through.
fn region_on_wallpaper(wall: &std::path::Path, screen: (f32, f32), logical: (u32, u32), d: &DesktopConf) -> Option<(u32, u32, u32, u32)> {
    region(probe(wall)?, screen, logical, d, shell_zoom())
}

/// region_on_wallpaper for an image of this size: x, y, w, h in image pixels.
fn region((iw, ih): (u32, u32), screen: (f32, f32), logical: (u32, u32), d: &DesktopConf, zoom: f32) -> Option<(u32, u32, u32, u32)> {
    let cover = (screen.0 / iw as f32).max(screen.1 / ih as f32);
    // undersized images are only covered; larger ones zoomed, capped at native pixels
    let s = if iw as f32 <= screen.0 || ih as f32 <= screen.1 { cover } else { (cover * zoom).min(1.0f32.max(cover)) };
    let (dw, dh) = (iw as f32 * s, ih as f32 * s);
    let (slide_x, slide_y) = ((dw - screen.0).max(0.0), (dh - screen.1).max(0.0));
    // the surface on the screen, logical pixels
    let (w, h) = (logical.0 as f32, logical.1 as f32);
    let (l, r, t, b) = d.anchor.edges();
    let (mx, my) = (d.margin.0 as f32, d.margin.1 as f32);
    let x = if l { mx } else if r { screen.0 - w - mx } else { (screen.0 - w) / 2.0 };
    let y = if t { my } else if b { screen.1 - h - my } else { (screen.1 - h) / 2.0 };
    // in the displayed image: from the leftmost slide position to the rightmost
    let (x0, y0, x1, y1) = (x, y, x + w + slide_x, y + h + slide_y);
    let k = 1.0 / s;
    let clamp = |v: f32, max: u32| (v * k).clamp(0.0, max as f32) as u32;
    let (ax, ay, bx, by) = (clamp(x0, iw), clamp(y0, ih), clamp(x1, iw), clamp(y1, ih));
    (bx > ax && by > ay).then_some((ax, ay, bx - ax, by - ay))
}

/// The shell's parallax zoom (illogical-impulse config), else its default.
fn shell_zoom() -> f32 {
    let path = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/illogical-impulse/config.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["background"]["parallax"]["workspaceZoom"].as_f64())
        .map(|z| z as f32)
        .unwrap_or(1.07)
}

fn tinct() -> std::process::Command {
    let local = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/tinct");
    std::process::Command::new(if local.exists() { local } else { PathBuf::from("tinct") })
}

fn probe(img: &std::path::Path) -> Option<(u32, u32)> {
    let out = tinct().args(["image", "probe"]).arg(img).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// The luminance histogram (256 bins) of a region of an image.
fn histogram(img: &std::path::Path, (x, y, w, h): (u32, u32, u32, u32)) -> Option<Vec<u64>> {
    let tmp = std::env::temp_dir().join(format!("zash-desktop-{}.png", std::process::id()));
    let ok = tinct().args(["image", "crop"]).arg(img).arg(format!("{w}x{h}+{x}+{y}")).arg(&tmp).status().ok()?.success();
    if !ok {
        return None;
    }
    let out = tinct().args(["image", "histogram"]).arg(&tmp).output().ok();
    let _ = std::fs::remove_file(&tmp);
    serde_json::from_slice::<Vec<u64>>(&out?.stdout).ok().filter(|h| h.len() == 256)
}

/// sRGB 0..1 to relative luminance.
fn lum(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn ratio(a: f32, b: f32) -> f32 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The value at a fraction of the histogram's pixels (0..1), as 0..1.
fn percentile(h: &[u64], p: f32) -> f32 {
    let total: u64 = h.iter().sum();
    let want = (total as f32 * p) as u64;
    let mut seen = 0;
    for (i, n) in h.iter().enumerate() {
        seen += n;
        if seen > want {
            return i as f32 / 255.0;
        }
    }
    1.0
}

/// The least panel opacity (the theme's background over the wallpaper) that keeps the text's
/// contrast at `target` over the wallpaper's lightest and darkest parts (2nd and 98th percentile).
/// A busy wallpaper (wide spread) gets a little more: the blur does not flatten it all.
pub(crate) fn panel_opacity(hist: &[u64], fg: [f32; 4], bg: [f32; 4], target: f32, min: f32) -> f32 {
    let grey = |c: [f32; 4]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let (f, b) = (lum(grey(fg)), grey(bg));
    let ends = [percentile(hist, 0.02), percentile(hist, 0.98)];
    let ok = |a: f32| ends.iter().all(|w| ratio(f, lum(w * (1.0 - a) + b * a)) >= target);
    let mut a = min.clamp(0.0, 0.97);
    while a < 0.97 && !ok(a) {
        a += 0.01;
    }
    let spread = ends[1] - ends[0];
    (a + 0.08 * spread).clamp(min, 0.97)
}

// ── Wayland plumbing ──

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(s: &mut Self, o: &wl_output::WlOutput, e: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Some((_, out)) = s.outputs.iter_mut().find(|(x, _)| x == o) else { return };
        match e {
            wl_output::Event::Name { name } => out.name = name,
            wl_output::Event::Scale { factor } => out.scale = factor,
            wl_output::Event::Mode { flags, width, height, .. } => {
                if flags.into_result().is_ok_and(|f| f.contains(wl_output::Mode::Current)) {
                    out.mode = (width, height);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, usize> for State {
    fn event(s: &mut Self, _: &zxdg_output_v1::ZxdgOutputV1, e: zxdg_output_v1::Event, i: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let (zxdg_output_v1::Event::LogicalSize { width, height }, Some((_, out))) = (e, s.outputs.get_mut(*i)) {
            out.logical = (width, height);
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(s: &mut Self, ls: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, e: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ls.ack_configure(serial);
                if width > 0 && height > 0 {
                    s.configured = Some((width, height));
                } else if s.configured.is_none() {
                    s.configured = Some((1, 1));
                }
            }
            zwlr_layer_surface_v1::Event::Closed => s.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for State {
    fn event(s: &mut Self, _: &wp_fractional_scale_v1::WpFractionalScaleV1, e: wp_fractional_scale_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = e {
            s.scale120 = Some(scale);
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn event(s: &mut Self, _: &wl_callback::WlCallback, e: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = e {
            s.frame_done = true;
        }
    }
}

macro_rules! no_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(zxdg_output_manager_v1::ZxdgOutputManagerV1, wl_compositor::WlCompositor, wl_surface::WlSurface, wl_region::WlRegion, zwlr_layer_shell_v1::ZwlrLayerShellV1, wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, wp_viewporter::WpViewporter, wp_viewport::WpViewport);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Anchor;

    fn flat(v: u8) -> Vec<u64> {
        let mut h = vec![0; 256];
        h[v as usize] = 1000;
        h
    }
    const WHITE: [f32; 4] = [0.97, 0.97, 0.95, 1.0];
    const DARK: [f32; 4] = [0.16, 0.16, 0.21, 1.0];

    #[test]
    fn a_dark_wallpaper_needs_only_the_minimum() {
        assert!((panel_opacity(&flat(10), WHITE, DARK, 7.0, 0.12) - 0.12).abs() < 0.02);
    }

    #[test]
    fn a_light_wallpaper_gets_enough_panel_for_the_contrast() {
        let a = panel_opacity(&flat(240), WHITE, DARK, 7.0, 0.12);
        let w = 240.0 / 255.0;
        let b = 0.2126 * DARK[0] + 0.7152 * DARK[1] + 0.0722 * DARK[2];
        assert!(ratio(lum(0.97), lum(w * (1.0 - a) + b * a)) >= 7.0, "{a}");
        assert!(a > 0.6 && a < 0.97, "{a}");
    }

    #[test]
    fn a_busy_wallpaper_gets_a_little_more() {
        let mut busy = vec![0; 256];
        busy[20] = 500;
        busy[120] = 500;
        let calm = panel_opacity(&flat(120), WHITE, DARK, 4.5, 0.0);
        assert!(panel_opacity(&busy, WHITE, DARK, 4.5, 0.0) > calm);
    }

    #[test]
    fn the_region_spans_the_parallax_slide() {
        let d = DesktopConf { anchor: Anchor::BottomRight, size: (400, 200), margin: (20, 20), ..Default::default() };
        // a wide image on a 1600x1000 screen: zoomed to cover (×1.07), it slides sideways
        let s = (1600.0f32 / 3555.0).max(1000.0 / 1920.0) * 1.07;
        let (x, y, w, h) = region((3555, 1920), (1600.0, 1000.0), (400, 200), &d, 1.07).unwrap();
        // as wide as the surface plus the slide, in image pixels
        let slide = 3555.0 * s - 1600.0;
        assert!(((w as f32) * s - (400.0 + slide)).abs() < 3.0, "w {w}");
        assert!(((x as f32) * s - (1600.0 - 400.0 - 20.0)).abs() < 3.0, "x {x}");
        assert!(y > 0 && h > 0 && x + w <= 3555 && y + h <= 1920);
    }

    #[test]
    fn a_small_image_is_only_covered() {
        let d = DesktopConf { anchor: Anchor::TopLeft, size: (100, 100), margin: (0, 0), ..Default::default() };
        // 800x500 on 1600x1000: scaled ×2, no zoom, no slide
        assert_eq!(region((800, 500), (1600.0, 1000.0), (100, 100), &d, 1.07), Some((0, 0, 50, 50)));
    }
}

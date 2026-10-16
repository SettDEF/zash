//! Headless: run a command in a real pty, then print the screen as text and,
//! optionally, render it to a PNG offscreen. No window, no display needed —
//! this is how zash is tested (and how it can be checked while you use the
//! machine for something else).
//!
//!   zash --snapshot [out.png] [--size 100x30] [--wait MS] -- CMD ARGS…

use crate::config::Config;
use crate::render::{Frame, Renderer};
use crate::session::{Grid, Session, Sink};
use alacritty_terminal::event::Event;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct Opts {
    pub png: Option<String>,
    pub cols: usize,
    pub rows: usize,
    pub wait: Duration,
    pub cmd: Vec<String>,
    /// keys typed into the program once it has started (input::encode_notation)
    pub keys: Option<String>,
    pub workdir: Option<std::path::PathBuf>,
    /// search the scrollback for this, `older` steps back (tests)
    pub search: Option<String>,
    pub older: usize,
}

/// Runs the command until it exits (or `wait` passes) and returns the screen.
pub fn capture(o: &Opts, cfg: &Config) -> std::io::Result<Frame> {
    let (tx, rx) = mpsc::channel();
    let grid = Grid { cols: o.cols, rows: o.rows, cell_w: 9.0, cell_h: 18.0 };
    // no command: the login shell, with zash's shell integration (as in the window)
    let cmd = if o.cmd.is_empty() { None } else { Some(o.cmd.clone()) };
    let session = Session::spawn(cmd, o.workdir.clone(), grid, cfg.scrollback, Sink::Channel(tx))?;
    if let Some(keys) = &o.keys {
        // give the program a moment to start, then type at a human-ish pace
        std::thread::sleep(Duration::from_millis(400));
        let app_cursor = session.term.lock().mode().contains(alacritty_terminal::term::TermMode::APP_CURSOR);
        for b in crate::input::encode_notation(keys, app_cursor) {
            session.write(b);
            std::thread::sleep(Duration::from_millis(8));
        }
    }
    let deadline = Instant::now() + o.wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(Event::ChildExit(_)) | Ok(Event::Exit) => {
                // let the last output land in the grid
                std::thread::sleep(Duration::from_millis(60));
                break;
            }
            Ok(Event::PtyWrite(s)) => session.write(s.into_bytes()),
            Ok(Event::TextAreaSizeRequest(f)) => session.write(f(alacritty_terminal::event::WindowSize { num_lines: o.rows as u16, num_cols: o.cols as u16, cell_width: 9, cell_height: 18 }).into_bytes()),
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let marks = session.marks.lock().unwrap().clone();
    let mut bar = None;
    if let Some(q) = &o.search {
        let mut s = crate::search::Search::default();
        let mut t = session.term.lock();
        s.set_query(&mut t, q);
        for _ in 0..o.older {
            s.step(&mut t, alacritty_terminal::index::Direction::Left);
        }
        eprintln!("zash: search {q:?}: match at line {:?} of the history", s.current_line(&t));
        bar = Some(s.bar());
    }
    let pal = if session.reverse.load(std::sync::atomic::Ordering::Relaxed) { cfg.palette.reversed() } else { cfg.palette.clone() };
    // test hook: ZASH_TEST_SCROLL="LINES,PX" scrolls back LINES and freezes a glide at PX pixels
    let glide = std::env::var("ZASH_TEST_SCROLL").ok().and_then(|v| {
        let (l, p) = v.split_once(',')?;
        Some((l.trim().parse::<i32>().ok()?, p.trim().parse::<f32>().ok()?))
    });
    if let Some((lines, _)) = glide {
        session.term.lock().scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
    }
    let mut frame = { let t = session.term.lock(); Frame::extract(&t, &pal).with_prompts(&t, &marks) };
    if let Some((_, px)) = glide {
        let t = session.term.lock();
        frame = frame.with_scroll(&t, &pal, px, cfg_cell_h(cfg));
    }
    frame.bar = bar;
    frame.popup = session.popup.lock().unwrap().clone();
    if !marks.is_empty() {
        let list: Vec<String> = marks.iter().map(|m| match m.exit { Some(e) => format!("{}{}={e}", m.kind, m.line), None => format!("{}{}", m.kind, m.line) }).collect();
        eprintln!("zash: shell marks: {}", list.join(" "));
    }
    Ok(frame)
}

/// Renders a frame offscreen and writes a PNG.
pub fn render_png(frame: &Frame, cfg: &Config, path: &str) -> Result<(u32, u32), String> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).map_err(|e| format!("no GPU adapter: {e}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).map_err(|e| e.to_string())?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut r = Renderer::new(&device, &queue, format, cfg, 1.0);
    let width = (frame.cols as f32 * r.cell_w + 2.0 * r.pad).ceil() as u32;
    let height = (frame.rows.len() as f32 * r.cell_h + 2.0 * r.pad).ceil() as u32;
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("zash snapshot"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    r.cursor_ms = 0.0;
    let pane = crate::render::PaneView { id: 0, x: 0.0, y: 0.0, w: width as f32, h: height as f32, frame, focused: true };
    r.render(&device, &queue, &view, width, height, &[pane]);

    // read back, rows padded to 256 bytes
    let row = (width * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (row * height) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(height) } },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit(Some(enc.finish()));
    let slice = buf.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| { let _ = tx.send(res); });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    let data = slice.get_mapped_range().map_err(|e| format!("{e:?}"))?;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(&data[y * row as usize..y * row as usize + width as usize * 4]);
    }
    drop(data);
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut w| w.write_image_data(&pixels)).map_err(|e| e.to_string())?;
    Ok((width, height))
}

/// The cell height the offscreen renderer will use (scale 1), for test hooks.
fn cfg_cell_h(cfg: &Config) -> f32 {
    (cfg.font_size_pt * 96.0 / 72.0 * cfg.line_height).round()
}

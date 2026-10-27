//! zash — the zett terminal. See docs/TERMINAL.md.
//!
//!   zash                          a window with your shell
//!   zash -e CMD ARGS…             a window running CMD
//!   zash --snapshot [out.png] [--size 100x30] [--wait MS] -- CMD…
//!                                 headless: run CMD, print the screen, render a PNG

// std's eprintln! panics when stderr is a closed pipe (started by a launcher
// that went away): zash died on its first message. This one drops the message.
macro_rules! eprintln {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($t)*);
    }};
}

mod app;
mod watchdog;
mod cli;
mod config;
mod copy;
mod anim;
mod ink;
mod layers;
mod ui;
mod layer_fx;
mod pen;
mod desktop;
mod dnd;
mod graphics;
mod web;
mod input;
mod keys;
mod layout;
mod fx;
mod fx_draw;
mod panel_shader;
mod links;
mod nvim;
mod search;
mod render;
mod session;
mod shell;
mod snapshot;

use std::time::Duration;

fn main() {
    session::scrub_environment();
    watchdog::start();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // --preset NAME and --config FILE: as variables, so the shells inside see them too
    for (flag, var) in [("--preset", "ZASH_PRESET"), ("--config", "ZASH_CONFIG")] {
        if let Some(i) = args.iter().position(|a| a == flag) {
            if i + 1 < args.len() {
                let v = args.remove(i + 1);
                args.remove(i);
                unsafe { std::env::set_var(var, v) };
            }
        }
    }
    if matches!(args.first().map(String::as_str), Some("-V" | "--version" | "version")) {
        println!("zash {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if matches!(args.first().map(String::as_str), Some("-h" | "--help" | "help")) {
        cli::print(&cli::HELP.replace("{version}", env!("CARGO_PKG_VERSION")));
        return;
    }
    // zash open ADDRESS (also --web): a window that is the page
    if matches!(args.first().map(String::as_str), Some("open" | "--web")) {
        if args.iter().any(|a| a == "-h" || a == "--help") {
            return cli::print(&format!("Open a web page in a window of its own.\n\n{}\n\n  <address>   a URL, a host (wikipedia.org) or words to search", cli::OPEN_USAGE));
        }
        let typed = args[1..].join(" ");
        let url = if typed.trim().is_empty() { "about:blank".to_string() } else { web::address(&typed) };
        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "zash".into());
        let cmd = vec![exe, "web".into(), "--fill".into(), url];
        if let Err(e) = app::run(config::Config::load(), Some(cmd), None, None) {
            eprintln!("zash: {e}");
            std::process::exit(1);
        }
        return;
    }
    // zash web ADDRESS: a web page here (inside zeit: a zeit pane)
    if args.first().map(String::as_str) == Some("web") {
        let mut rows = 30usize;
        let (mut fill, mut beside) = (false, false);
        let mut words = Vec::new();
        let mut it = args[1..].iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "-h" | "--help" => return cli::print(cli::WEB_HELP),
                "-r" | "--rows" => match it.next().and_then(|v| v.parse::<usize>().ok()) {
                    Some(r) if (4..=200).contains(&r) => rows = r,
                    _ => cli::fail("-r wants a number of rows from 4 to 200", cli::WEB_USAGE, "zash web --help"),
                },
                "-s" | "--split" => beside = true,
                "--fill" => fill = true,
                o if o.starts_with('-') && o.len() > 1 => cli::fail(&format!("unknown option '{o}'"), cli::WEB_USAGE, "zash web --help"),
                _ => words.push(a.clone()),
            }
        }
        if words.is_empty() {
            cli::fail("an address is needed", cli::WEB_USAGE, "zash web --help");
        }
        let url = web::address(&words.join(" "));
        // inside zeit the page is a zeit pane: it follows the layout, windows and detaching
        if std::env::var_os("ZEIT_PANE").is_some() && !fill {
            let mut c = std::process::Command::new("zeit");
            c.arg("web");
            if beside {
                c.arg("-h");
            }
            let ok = c.arg(&url).status().is_ok_and(|s| s.success());
            std::process::exit(if ok { 0 } else { 1 });
        }
        if std::env::var("TERM_PROGRAM").ok().as_deref() != Some("zash") {
            eprintln!("zash web: this terminal cannot show web pages. Try: zash open {url}");
            std::process::exit(1);
        }
        if fill {
            // the whole window: the page from the top, and this program stays so the window does
            let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
            unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) };
            rows = (ws.ws_row as usize).max(4);
            print!("\x1b[?25l\x1b[H\x1b[2J\x1b]2;{url}\x07\x1b]7733;web;rows={rows};focus=1;url={url}\x07");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        print!("\x1b]7733;web;rows={rows};url={url}\x07{}", "\n".repeat(rows));
        let _ = std::io::Write::flush(&mut std::io::stdout());
        return;
    }
    let cfg = config::Config::load();
    // the configured shell, for zash and for everything started in it (zeit, scripts)
    if let Some(s) = &cfg.shell {
        unsafe { std::env::set_var("SHELL", s) };
    }
    // -d DIR: start in that folder
    let mut workdir = None;
    if let Some(i) = args.iter().position(|a| a == "-d" || a == "--dir") {
        if i + 1 < args.len() {
            workdir = Some(std::path::PathBuf::from(args.remove(i + 1)));
            args.remove(i);
        }
    }

    // --desktop: on the wallpaper, click-through ([desktop] in the settings); -- CMD runs CMD in it
    // zash ui demo [--plain]: a mock app screen drawn with the UI hints (ui.rs)
    if matches!(args.first().map(String::as_str), Some("ui")) {
        match args.get(1).map(String::as_str) {
            Some("demo") => {
                // the screen's size; on a terminal it stays until Enter, then the cursor comes back
                let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
                let (c, r) = if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 { (ws.ws_col as usize, ws.ws_row as usize) } else { (80, 24) };
                print!("{}", ui::demo(args.iter().any(|a| a == "--plain"), c, r));
                use std::io::Write;
                let _ = std::io::stdout().flush();
                if unsafe { libc::isatty(0) } == 1 {
                    let _ = std::io::stdin().read_line(&mut String::new());
                    print!("\x1b[H\x1b[2J\x1b[?25h");
                }
                std::process::exit(0);
            }
            _ => { eprintln!("zash ui demo [--plain]"); std::process::exit(2) }
        }
    }
    // zash anim check FILE · zash anim play FILE [--rows N] [--loop N] [--speed X] [--color RRGGBB=name]…
    if matches!(args.first().map(String::as_str), Some("anim")) {
        let (sub, file) = (args.get(1).map(String::as_str), args.get(2));
        let code = match (sub, file) {
            (Some("check"), Some(f)) => {
                let json: serde_json::Value = match std::fs::read(f).ok().and_then(|d| serde_json::from_slice(&d).ok()) {
                    Some(j) => j,
                    None => { eprintln!("zash: {f}: not a Lottie file (JSON)"); std::process::exit(1) }
                };
                match anim::load(f) {
                    Err(e) => { eprintln!("zash: {e}"); 1 }
                    Ok(c) => {
                        println!("{f}: {}×{}, {} frames at {} a second ({:.1} s)", c.width, c.height, (c.frames.end - c.frames.start) as i64, c.frame_rate, (c.frames.end - c.frames.start) / c.frame_rate.max(1.0));
                        let issues = anim::check(&json);
                        for i in &issues { println!("  · {i}"); }
                        if issues.is_empty() { println!("  everything in it plays"); }
                        0
                    }
                }
            }
            (Some("play"), Some(f)) => {
                let scrub = args[3..].iter().any(|a| a == "--scrub");
                let args: Vec<String> = args.iter().filter(|a| *a != "--scrub").cloned().collect();
                // the escape for this terminal: zash draws it; elsewhere it is simply skipped
                let path = std::fs::canonicalize(f).map(|p| p.display().to_string()).unwrap_or(f.clone());
                let mut body = format!("file={path}");
                let mut it = args[3..].iter();
                while let Some(a) = it.next() {
                    let v = it.next().cloned().unwrap_or_default();
                    match a.as_str() {
                        "--rows" => body.push_str(&format!(";rows={v}")),
                        "--cols" => body.push_str(&format!(";cols={v}")),
                        "--loop" => body.push_str(&format!(";loop={v}")),
                        "--speed" => body.push_str(&format!(";speed={v}")),
                        "--color" => if let Some((from, to)) = v.split_once('=') { body.push_str(&format!(";color.{}={to}", from.trim_start_matches('#'))) },
                        other => { eprintln!("zash anim play: unknown {other}"); std::process::exit(2) }
                    }
                }
                if !body.contains(";rows=") { body.push_str(";rows=4"); }
                if scrub {
                    match anim::load(f) {
                        Ok(c) => anim_scrub(&body, c.frame_rate.max(1.0), (c.frames.end - c.frames.start).max(1.0)),
                        Err(e) => { eprintln!("zash: {e}"); 1 }
                    }
                } else {
                    print!("\x1b]7733;anim;{body}\x07");
                    0
                }
            }
            _ => { eprintln!("zash anim check FILE · zash anim play FILE [--rows N] [--cols N] [--loop N] [--speed X] [--color RRGGBB=accent]"); 2 }
        };
        std::process::exit(code);
    }
    if matches!(args.first().map(String::as_str), Some("--desktop" | "desktop")) {
        if args.iter().any(|a| a == "-h" || a == "--help") {
            return cli::print(cli::DESKTOP_HELP);
        }
        let cmd = args.iter().position(|a| a == "--").map(|i| args[i + 1..].to_vec()).filter(|c| !c.is_empty());
        let probe = args.iter().any(|a| a == "--probe");
        if let Err(e) = desktop::run(cfg, cmd, probe) {
            eprintln!("zash --desktop: {e}");
            std::process::exit(1);
        }
        return;
    }
    // --float [WxH]: a floating window of that size ([float] size; Hyprland floats "zash-float")
    let mut float = None;
    if let Some(i) = args.iter().position(|a| a == "--float") {
        args.remove(i);
        let size = args.get(i).and_then(|s| s.split_once('x')).and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)));
        if size.is_some() {
            args.remove(i);
        }
        float = Some(size.unwrap_or(cfg.float_size));
    }

    // zash shader-snap NAME OUT.png [--size WxH] [--time S] [--panel RRGGBB] [--accent RRGGBB] [--set k=v]:
    // one frame of a panel shader, drawn offscreen (to see a shader without a window)
    if args.first().map(String::as_str) == Some("shader-snap") {
        let (mut name, mut out, mut size, mut time) = (None, None, (720u32, 450u32), 3.0f32);
        let (mut panel, mut accent, mut set) = ([0.07, 0.07, 0.09, 1.0], [0.45, 0.62, 1.0, 1.0], Vec::new());
        let hex = |s: &str| -> Option<[f32; 4]> {
            let v = u32::from_str_radix(s.trim_start_matches('#'), 16).ok()?;
            Some([((v >> 16) & 255) as f32 / 255.0, ((v >> 8) & 255) as f32 / 255.0, (v & 255) as f32 / 255.0, 1.0])
        };
        let mut it = args.into_iter().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--size" => {
                    if let Some((w, h)) = it.next().and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))) {
                        size = (w, h);
                    }
                }
                "--time" => time = it.next().and_then(|s| s.parse().ok()).unwrap_or(time),
                "--panel" => panel = it.next().as_deref().and_then(hex).unwrap_or(panel),
                "--accent" => accent = it.next().as_deref().and_then(hex).unwrap_or(accent),
                "--set" => {
                    if let Some((k, v)) = it.next().and_then(|s| s.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))) {
                        set.push((k, v));
                    }
                }
                _ if name.is_none() => name = Some(a),
                _ => out = Some(a),
            }
        }
        let (Some(name), Some(out)) = (name, out) else {
            eprintln!("usage: zash shader-snap NAME OUT.png [--size WxH] [--time S] [--panel RRGGBB] [--accent RRGGBB] [--set option=value]");
            std::process::exit(2);
        };
        match panel_shader::snap(&name, &out, size.0, size.1, time, panel, accent, set) {
            Ok(()) => println!("wrote {out}"),
            Err(e) => {
                eprintln!("zash: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if matches!(args.first().map(String::as_str), Some("--snapshot" | "snapshot")) {
        let mut o = snapshot::Opts { png: None, cols: 100, rows: 30, wait: Duration::from_secs(5), cmd: Vec::new(), keys: None, workdir: workdir.clone(), search: None, older: 0, nvim: false };
        let mut it = args.into_iter().skip(1);
        while let Some(a) = it.next() {
            match a.as_str() {
                "--size" => {
                    if let Some((c, r)) = it.next().and_then(|s| s.split_once('x').map(|(a, b)| (a.to_string(), b.to_string()))) {
                        o.cols = c.parse().unwrap_or(100);
                        o.rows = r.parse().unwrap_or(30);
                    }
                }
                "--keys" => o.keys = it.next(),
                "--nvim" => o.nvim = true,
                "--search" => o.search = it.next(),
                "--older" => o.older = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
                "--wait" => o.wait = Duration::from_millis(it.next().and_then(|s| s.parse().ok()).unwrap_or(5000)),
                "--" => {
                    o.cmd = it.by_ref().collect();
                    break;
                }
                p if o.png.is_none() && !p.starts_with('-') => o.png = Some(p.to_string()),
                other => {
                    eprintln!("zash: unknown option {other}");
                    std::process::exit(2);
                }
            }
        }
        match snapshot::capture(&o, &cfg) {
            Ok(frame) => {
                println!("{}", frame.text());
                if let Some(p) = &o.png {
                    match snapshot::render_png(&frame, &cfg, p) {
                        Ok((w, h)) => eprintln!("zash: wrote {p} ({w}×{h})"),
                        Err(e) => {
                            eprintln!("zash: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("zash: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let cmd = match args.first().map(String::as_str) {
        Some("-e") | Some("--") => {
            let rest = args[1..].to_vec();
            // `-e 'one command line'` (as many launchers call a terminal): through the shell
            if rest.len() == 1 && rest[0].contains(' ') {
                Some(vec!["sh".into(), "-c".into(), rest[0].clone()])
            } else {
                Some(rest).filter(|c| !c.is_empty())
            }
        }
        // `zash PROGRAM ARGS…`
        Some(p) if !p.starts_with('-') => Some(args.clone()),
        Some("--exec") => Some(args[1..].to_vec()).filter(|c| !c.is_empty()),
        Some(o) if o.starts_with('-') => cli::fail(&format!("unknown option '{o}'"), "**Usage:** `zash` [<options>] [-- <command> …]", "zash --help"),
        _ => None,
    };
    // a plain `zash` runs the configured command (command = "zeit new"), else the shell
    let cmd = cmd.or_else(|| cfg.command.clone().filter(|c| !c.trim().is_empty()).map(|c| vec!["sh".into(), "-c".into(), format!("exec {c}")]));
    if let Err(e) = app::run(cfg, cmd, workdir, float) {
        eprintln!("zash: {e}");
        std::process::exit(1);
    }
}

/// `zash anim play FILE --scrub`: one held frame, moved with the keys (← → a frame, PgUp PgDn a
/// second, Home End, q). Each step is the same named escape again, so it replaces itself in place.
fn anim_scrub(body: &str, fps: f64, frames: f64) -> i32 {
    use std::io::{Read, Write};
    let name = format!("scrub{}", std::process::id());
    let raw = |on: bool| { let _ = std::process::Command::new("stty").arg(if on { "raw" } else { "sane" }).args(if on { &["-echo"][..] } else { &[][..] }).stdin(std::process::Stdio::inherit()).status(); };
    let mut n: f64 = 0.0;
    let mut out = std::io::stdout();
    let show = |n: f64, out: &mut std::io::Stdout| {
        let _ = write!(out, "\x1b]7733;anim;{body};name={name};at={:.4}\x07\r\x1b[K frame {}/{}  {:.2} s   ← → frame · PgUp PgDn second · Home End · q", n / fps, n as i64, frames as i64 - 1, n / fps);
        let _ = out.flush();
    };
    raw(true);
    show(n, &mut out);
    let mut buf = [0u8; 8];
    let mut stdin = std::io::stdin();
    while let Ok(k) = stdin.read(&mut buf) {
        let last = frames - 1.0;
        n = match &buf[..k] {
            b"q" | b"\x03" | b"\x1b" => break,
            b"\x1b[C" => (n + 1.0).min(last),
            b"\x1b[D" => (n - 1.0).max(0.0),
            b"\x1b[5~" => (n + fps).min(last),
            b"\x1b[6~" => (n - fps).max(0.0),
            b"\x1b[H" | b"\x1b[1~" => 0.0,
            b"\x1b[F" | b"\x1b[4~" => last,
            _ => continue,
        };
        show(n, &mut out);
    }
    raw(false);
    println!();
    0
}

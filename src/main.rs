//! zash — the zett terminal. See docs/TERMINAL.md.
//!
//!   zash                          a window with your shell
//!   zash -e CMD ARGS…             a window running CMD
//!   zash --snapshot [out.png] [--size 100x30] [--wait MS] -- CMD…
//!                                 headless: run CMD, print the screen, render a PNG

mod app;
mod config;
mod input;
mod render;
mod session;
mod snapshot;

use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = config::Config::default();

    if args.first().map(String::as_str) == Some("--snapshot") {
        let mut o = snapshot::Opts { png: None, cols: 100, rows: 30, wait: Duration::from_secs(5), cmd: Vec::new(), keys: None };
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
        if o.cmd.is_empty() {
            eprintln!("zash --snapshot needs a command after --");
            std::process::exit(2);
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
        Some("-e") => Some(args[1..].to_vec()).filter(|c| !c.is_empty()),
        Some("-h") | Some("--help") => {
            println!("zash — the zett terminal\n\n  zash               your shell in a window\n  zash -e CMD …      run CMD in a window\n  zash --snapshot [out.png] [--size 100x30] [--wait MS] -- CMD …\n                     headless: run CMD, print the screen, optionally render a PNG\n\nKeys: Ctrl+Shift+C/V copy/paste · Ctrl+= / Ctrl+- / Ctrl+0 font size · Shift+PageUp/PageDown scroll");
            return;
        }
        _ => None,
    };
    if let Err(e) = app::run(cfg, cmd) {
        eprintln!("zash: {e}");
        std::process::exit(1);
    }
}

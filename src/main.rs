//! zash — the zett terminal. See docs/TERMINAL.md.
//!
//!   zash                          a window with your shell
//!   zash -e CMD ARGS…             a window running CMD
//!   zash --snapshot [out.png] [--size 100x30] [--wait MS] -- CMD…
//!                                 headless: run CMD, print the screen, render a PNG

mod app;
mod config;
mod dnd;
mod input;
mod keys;
mod layout;
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

    if args.first().map(String::as_str) == Some("--snapshot") {
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
        Some("-h") | Some("--help") => {
            println!("zash — the zett terminal\n\n  zash               your shell in a window\n  zash -e CMD …      run CMD in a window (also: zash CMD …, zash -e 'a command line')\n  zash -d DIR        start in DIR (command = '…' in the settings: what a plain zash runs)
  zash --preset NAME a preset on top of your settings (~/.config/zash/presets/NAME.toml; also $ZASH_PRESET)
  zash --config FILE another settings file (also $ZASH_CONFIG)\n  zash --snapshot [out.png] [--size 100x30] [--wait MS] [-- CMD …]   (no CMD: your shell)\n                     headless: run CMD, print the screen, optionally render a PNG\n\nKeys: Ctrl+Shift+C/V copy/paste · Ctrl+Shift+Up/Down previous/next prompt · Ctrl+Shift+O copy the last output · Ctrl+Shift+F search · Ctrl+click open a link · Ctrl+Shift+T/W new/close tab · Ctrl+Tab next tab · Ctrl+= / Ctrl+- / Ctrl+0 font size · Shift+PageUp/PageDown scroll\n\nSettings: ~/.config/zash/zash.toml (written on first start, with comments)");
            return;
        }
        _ => None,
    };
    // a plain `zash` runs the configured command (command = "zeit new"), else the shell
    let cmd = cmd.or_else(|| cfg.command.clone().filter(|c| !c.trim().is_empty()).map(|c| vec!["sh".into(), "-c".into(), format!("exec {c}")]));
    if let Err(e) = app::run(cfg, cmd, workdir) {
        eprintln!("zash: {e}");
        std::process::exit(1);
    }
}

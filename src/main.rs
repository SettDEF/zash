//! zash — the zett terminal. See docs/TERMINAL.md.
//!
//!   zash                          a window with your shell
//!   zash -e CMD ARGS…             a window running CMD
//!   zash --snapshot [out.png] [--size 100x30] [--wait MS] -- CMD…
//!                                 headless: run CMD, print the screen, render a PNG

mod app;
mod watchdog;
mod cli;
mod config;
mod dnd;
mod graphics;
mod web;
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
        if let Err(e) = app::run(config::Config::load(), Some(cmd), None) {
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
    if let Err(e) = app::run(cfg, cmd, workdir) {
        eprintln!("zash: {e}");
        std::process::exit(1);
    }
}

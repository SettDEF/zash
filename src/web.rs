//! Web pages in the terminal: a headless Chrome per zash, a tab per web area, its frames
//! drawn in the area's cells, input passed back (the DevTools protocol over a pipe).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// One web area: its tab in Chrome, the latest frame, where it is.
pub struct View {
    pub id: u32,
    session: Mutex<Option<String>>,
    target: Mutex<Option<String>>,
    /// commands that wait for the tab to exist
    queued: Mutex<Vec<(String, Value)>>,
    pub frame: Mutex<Option<crate::graphics::Image>>,
    /// bumped with every frame (the renderer updates its texture then)
    pub generation: AtomicU64,
    pub size: Mutex<(u32, u32)>,
    pub url: Mutex<String>,
    wake: Box<dyn Fn() + Send + Sync>,
}

enum Pend {
    Create(Arc<View>),
    Attach(Arc<View>),
}

struct Host {
    w: Mutex<File>,
    next: AtomicU64,
    views: Mutex<HashMap<String, Arc<View>>>,
    pending: Mutex<HashMap<u64, Pend>>,
    _child: Mutex<std::process::Child>,
}

static HOST: OnceLock<Option<Arc<Host>>> = OnceLock::new();

/// The display's scale (pixels per CSS pixel): pages render at the screen's real resolution.
static SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

pub fn set_scale(s: f32) {
    SCALE.store(s.max(0.5).to_bits(), Ordering::Relaxed);
}

fn scale() -> f32 {
    f32::from_bits(SCALE.load(Ordering::Relaxed))
}

fn chrome() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    ["google-chrome-stable", "google-chrome", "chromium", "chromium-browser", "brave", "microsoft-edge-stable"]
        .iter()
        .find(|b| std::env::split_paths(&path).any(|d| d.join(b).is_file()))
        .map(|s| s.to_string())
}

/// The browser profile: shared (logins stay) unless another zash has it, then a private one.
fn profile() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state")).join("zash/web");
    let main = base.join("profile");
    let busy = std::fs::read_link(main.join("SingletonLock")).ok().and_then(|t| t.to_string_lossy().rsplit('-').next().and_then(|p| p.parse::<i32>().ok())).is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0);
    if busy { base.join(format!("profile-{}", std::process::id())) } else { main }
}

fn host() -> Option<Arc<Host>> {
    HOST.get_or_init(|| {
        let bin = chrome()?;
        // two pipes: Chrome reads commands on fd 3, writes answers on fd 4
        let mut a = [0i32; 2];
        let mut b = [0i32; 2];
        unsafe {
            if libc::pipe2(a.as_mut_ptr(), libc::O_CLOEXEC) != 0 || libc::pipe2(b.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
                return None;
            }
        }
        let (cmd_r, cmd_w, out_r, out_w) = (a[0], a[1], b[0], b[1]);
        let dir = profile();
        let _ = std::fs::create_dir_all(&dir);
        let mut c = std::process::Command::new(bin);
        c.args(["--headless=new", "--remote-debugging-pipe", "--no-first-run", "--no-default-browser-check", "--disable-features=Translate,MediaRouter", "--hide-crash-restore-bubble"])
            .arg(format!("--user-data-dir={}", dir.display()))
            .arg("about:blank")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        unsafe {
            use std::os::unix::process::CommandExt;
            c.pre_exec(move || {
                // the browser goes when zash goes
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                if libc::dup2(cmd_r, 3) < 0 || libc::dup2(out_w, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = c.spawn().ok()?;
        unsafe {
            libc::close(cmd_r);
            libc::close(out_w);
        }
        let host = Arc::new(Host { w: Mutex::new(unsafe { File::from_raw_fd(cmd_w) }), next: AtomicU64::new(1), views: Mutex::default(), pending: Mutex::default(), _child: Mutex::new(child) });
        let h = host.clone();
        std::thread::Builder::new().name("zash web".into()).spawn(move || read_loop(unsafe { File::from_raw_fd(out_r) }, h)).ok()?;
        Some(host)
    })
    .clone()
}

impl Host {
    fn send(&self, session: Option<&str>, method: &str, params: Value) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut m = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            m["sessionId"] = json!(s);
        }
        if let Ok(mut w) = self.w.lock() {
            let mut bytes = m.to_string().into_bytes();
            bytes.push(0);
            let _ = w.write_all(&bytes);
        }
        id
    }
}

fn read_loop(mut r: File, host: Arc<Host>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 1 << 16];
    loop {
        let n = match r.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        while let Some(end) = buf.iter().position(|&b| b == 0) {
            let msg: Vec<u8> = buf.drain(..=end).collect();
            let Ok(v) = serde_json::from_slice::<Value>(&msg[..msg.len() - 1]) else { continue };
            handle(&host, v);
        }
    }
}

fn handle(host: &Arc<Host>, v: Value) {
    // answers to our commands: making a tab, attaching to it
    if let Some(id) = v["id"].as_u64() {
        let p = host.pending.lock().unwrap().remove(&id);
        match p {
            Some(Pend::Create(view)) => {
                if let Some(t) = v["result"]["targetId"].as_str() {
                    *view.target.lock().unwrap() = Some(t.to_string());
                    let id = host.send(None, "Target.attachToTarget", json!({ "targetId": t, "flatten": true }));
                    host.pending.lock().unwrap().insert(id, Pend::Attach(view));
                }
            }
            Some(Pend::Attach(view)) => {
                if let Some(s) = v["result"]["sessionId"].as_str() {
                    host.views.lock().unwrap().insert(s.to_string(), view.clone());
                    *view.session.lock().unwrap() = Some(s.to_string());
                    let (w, h) = *view.size.lock().unwrap();
                    host.send(Some(s), "Page.enable", json!({}));
                    let k = scale();
                    host.send(Some(s), "Emulation.setDeviceMetricsOverride", json!({ "width": (w as f32 / k).round(), "height": (h as f32 / k).round(), "deviceScaleFactor": k, "mobile": false }));
                    host.send(Some(s), "Page.startScreencast", json!({ "format": "png", "everyNthFrame": 1 }));
                    for (m, p) in std::mem::take(&mut *view.queued.lock().unwrap()) {
                        host.send(Some(s), &m, p);
                    }
                }
            }
            None => {}
        }
        return;
    }
    // events of a tab
    let Some(s) = v["sessionId"].as_str() else { return };
    let Some(view) = host.views.lock().unwrap().get(s).cloned() else { return };
    match v["method"].as_str() {
        Some("Page.screencastFrame") => {
            let p = &v["params"];
            host.send(Some(s), "Page.screencastFrameAck", json!({ "sessionId": p["sessionId"] }));
            let data = crate::graphics::base64(p["data"].as_str().unwrap_or(""));
            if let Ok(img) = crate::graphics::decode_png(data) {
                *view.frame.lock().unwrap() = Some(img);
                view.generation.fetch_add(1, Ordering::Relaxed);
                (view.wake)();
            }
        }
        Some("Page.frameNavigated") => {
            let f = &v["params"]["frame"];
            if f["parentId"].is_null() {
                *view.url.lock().unwrap() = f["url"].as_str().unwrap_or("").to_string();
            }
        }
        _ => {}
    }
}

impl View {
    /// A page in an area of `w`×`h` pixels; `wake` asks for a redraw when a frame comes.
    pub fn open(id: u32, url: &str, w: u32, h: u32, wake: Box<dyn Fn() + Send + Sync>) -> Option<Arc<View>> {
        let host = host()?;
        let view = Arc::new(View { id, session: Mutex::default(), target: Mutex::default(), queued: Mutex::default(), frame: Mutex::default(), generation: AtomicU64::new(0), size: Mutex::new((w.max(64), h.max(64))), url: Mutex::new(url.to_string()), wake });
        view.queued.lock().unwrap().push(("Page.navigate".into(), json!({ "url": url })));
        let rid = host.send(None, "Target.createTarget", json!({ "url": "about:blank" }));
        host.pending.lock().unwrap().insert(rid, Pend::Create(view.clone()));
        Some(view)
    }

    fn send(&self, method: &str, params: Value) {
        let s = self.session.lock().unwrap().clone();
        match (s, host()) {
            (Some(s), Some(h)) => {
                h.send(Some(&s), method, params);
            }
            _ => self.queued.lock().unwrap().push((method.to_string(), params)),
        }
    }

    /// `kind`: pressed, released or moved; `button`: left, middle, right or none.
    pub fn mouse(&self, kind: &str, x: f32, y: f32, button: &str, clicks: u32) {
        // pixels in the area to the page's CSS pixels
        let (x, y) = (x / scale(), y / scale());
        let t = match kind {
            "pressed" => "mousePressed",
            "released" => "mouseReleased",
            _ => "mouseMoved",
        };
        self.send("Input.dispatchMouseEvent", json!({ "type": t, "x": x, "y": y, "button": button, "clickCount": clicks }));
    }

    pub fn wheel(&self, x: f32, y: f32, dy: f32) {
        let (x, y) = (x / scale(), y / scale());
        self.send("Input.dispatchMouseEvent", json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": dy }));
    }

    pub fn text(&self, s: &str) {
        self.send("Input.insertText", json!({ "text": s }));
    }

    /// A named key (Enter, Backspace, Tab, Escape, ArrowLeft …) or a letter with
    /// Ctrl/Alt (modifiers: 1 Alt, 2 Ctrl, 4 Meta, 8 Shift).
    pub fn key(&self, key: &str, modifiers: u32) {
        let (code, vk) = match key {
            "Enter" => ("Enter", 13),
            "Backspace" => ("Backspace", 8),
            "Tab" => ("Tab", 9),
            "Escape" => ("Escape", 27),
            "Delete" => ("Delete", 46),
            "ArrowLeft" => ("ArrowLeft", 37),
            "ArrowUp" => ("ArrowUp", 38),
            "ArrowRight" => ("ArrowRight", 39),
            "ArrowDown" => ("ArrowDown", 40),
            "Home" => ("Home", 36),
            "End" => ("End", 35),
            "PageUp" => ("PageUp", 33),
            "PageDown" => ("PageDown", 34),
            k if k.chars().count() == 1 => {
                let c = k.chars().next().unwrap().to_ascii_uppercase();
                return self.send_key(k, &format!("Key{c}"), c as u32, modifiers, None);
            }
            _ => return,
        };
        let text = match key {
            "Enter" => Some("\r"),
            _ => None,
        };
        self.send_key(key, code, vk, modifiers, text);
    }

    fn send_key(&self, key: &str, code: &str, vk: u32, modifiers: u32, text: Option<&str>) {
        let mut down = json!({ "type": if text.is_some() { "keyDown" } else { "rawKeyDown" }, "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers });
        if let Some(t) = text {
            down["text"] = json!(t);
        }
        self.send("Input.dispatchKeyEvent", down);
        self.send("Input.dispatchKeyEvent", json!({ "type": "keyUp", "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": modifiers }));
    }

    pub fn back(&self) {
        self.send("Runtime.evaluate", json!({ "expression": "history.back()" }));
    }

    pub fn reload(&self) {
        self.send("Page.reload", json!({}));
    }

    pub fn navigate(&self, url: &str) {
        self.send("Page.navigate", json!({ "url": url }));
    }

    /// The area changed size (the window, the font).
    pub fn resize(&self, w: u32, h: u32) {
        let (w, h) = (w.max(64), h.max(64));
        if *self.size.lock().unwrap() == (w, h) {
            return;
        }
        *self.size.lock().unwrap() = (w, h);
        let k = scale();
        self.send("Emulation.setDeviceMetricsOverride", json!({ "width": (w as f32 / k).round(), "height": (h as f32 / k).round(), "deviceScaleFactor": k, "mobile": false }));
    }

    pub fn close(&self) {
        if let (Some(t), Some(h)) = (self.target.lock().unwrap().clone(), host()) {
            h.send(None, "Target.closeTarget", json!({ "targetId": t }));
        }
    }
}

/// `rows=N;url=…` (OSC 7733;web): the area's height in rows and the page.
pub fn parse(body: &str) -> Option<(usize, String)> {
    let mut rows = 24;
    let mut rest = body;
    while let Some((kv, more)) = rest.split_once(';') {
        if let Some(v) = kv.strip_prefix("rows=") {
            rows = v.parse().unwrap_or(24);
            rest = more;
        } else {
            break;
        }
    }
    let url = rest.strip_prefix("url=")?.to_string();
    (!url.is_empty()).then_some((rows.clamp(4, 200), url))
}

/// What a typed address means: a URL, a host, or a search.
pub fn address(s: &str) -> String {
    let s = s.trim();
    if s.contains("://") || s.starts_with("about:") || s.starts_with("file:") {
        s.to_string()
    } else if s.contains('.') && !s.contains(' ') {
        format!("https://{s}")
    } else {
        format!("https://duckduckgo.com/?q={}", s.replace(' ', "+"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn web_messages_and_addresses() {
        assert_eq!(super::parse("rows=30;url=https://a.b/?x=1;y"), Some((30, "https://a.b/?x=1;y".into())));
        assert_eq!(super::parse("url=https://x.y"), Some((24, "https://x.y".into())));
        assert_eq!(super::address("wikipedia.org"), "https://wikipedia.org");
        assert_eq!(super::address("rust traits"), "https://duckduckgo.com/?q=rust+traits");
    }
}

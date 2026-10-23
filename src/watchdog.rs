//! A freeze or a crash leaves a line in ~/.local/state/zash/zash.log: what zash was doing.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
/// when the current piece of work began (ms since start; 0: idle)
static BUSY_SINCE: AtomicU64 = AtomicU64::new(0);
static WHAT: Mutex<&'static str> = Mutex::new("");

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

pub(crate) fn log(line: &str) {
    let Some(dir) = state_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("zash.log")) {
        let _ = writeln!(f, "{t} pid {} {line}", std::process::id());
    }
}

/// Marks a piece of work (an event being handled); idle again when the guard drops.
pub struct Busy;
impl Drop for Busy {
    fn drop(&mut self) {
        BUSY_SINCE.store(0, Ordering::Relaxed);
    }
}
pub fn busy(what: &'static str) -> Busy {
    if let Ok(mut w) = WHAT.lock() {
        *w = what;
    }
    BUSY_SINCE.store(now_ms(), Ordering::Relaxed);
    Busy
}

/// The panic hook and the thread that notices a stall (more than 3 s inside one piece of work).
pub fn start() {
    now_ms();
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let what = WHAT.lock().map(|w| *w).unwrap_or("");
        log(&format!("panic while {what}: {info}"));
        default(info);
    }));
    std::thread::spawn(|| {
        let mut reported = 0u64;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let since = BUSY_SINCE.load(Ordering::Relaxed);
            if since != 0 && since != reported && now_ms() - since > 3000 {
                reported = since;
                let what = WHAT.lock().map(|w| *w).unwrap_or("");
                log(&format!("stalled {} s while {what}", (now_ms() - since) / 1000));
                stacks();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_stall_is_logged() {
        // its own folder (not HOME: the other tests, running alongside, read HOME)
        let home = std::env::temp_dir().join(format!("zash-watchdog-{}", std::process::id()));
        unsafe { std::env::set_var("ZASH_STATE_DIR", home.join(".local/state/zash")) };
        super::start();
        {
            let _b = super::busy("testing a stall");
            std::thread::sleep(std::time::Duration::from_millis(9000));
        }
        let log = std::fs::read_to_string(home.join(".local/state/zash/zash.log")).unwrap_or_default();
        let stacks = std::fs::read_to_string(home.join(format!(".local/state/zash/stall-{}.txt", std::process::id()))).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&home);
        assert!(log.contains("stalled") && log.contains("testing a stall"), "{log:?}");
        // with gdb about, the stacks show the stalled thread asleep in this test
        if std::path::Path::new("/usr/bin/gdb").exists() {
            assert!(stacks.contains("a_stall_is_logged"), "no stacks: {}", &stacks[..stacks.len().min(400)]);
        }
    }
}

/// Where every thread is, written beside the log (gdb, if there is one): the next freeze
/// tells its own cause. ptrace is limited to parents here, so gdb is allowed in just for this.
fn stacks() {
    let Some(dir) = state_dir() else { return };
    let out = dir.join(format!("stall-{}.txt", std::process::id()));
    const PR_SET_PTRACER: libc::c_int = 0x5961_6d61;
    unsafe { libc::prctl(PR_SET_PTRACER, libc::c_ulong::MAX, 0, 0, 0) };
    let r = std::process::Command::new("gdb")
        .args(["-p", &std::process::id().to_string(), "-batch", "-ex", "thread apply all bt 30"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    unsafe { libc::prctl(PR_SET_PTRACER, 0, 0, 0, 0) };
    if let Ok(o) = r {
        let _ = std::fs::write(&out, o.stdout);
        log(&format!("thread stacks: {}", out.display()));
    }
}

/// ~/.local/state/zash (ZASH_STATE_DIR instead, for tests).
fn state_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("ZASH_STATE_DIR").map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state/zash")))
}

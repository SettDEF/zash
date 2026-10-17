//! Files dropped on the window (Wayland). winit handles drops on X11 but not on
//! Wayland, so zash listens itself: a second event queue on winit's own Wayland
//! connection, the seat's data device, and `text/uri-list` offers read through
//! a pipe. The paths and where they were dropped go to the window as an event.
//!
//! Dragging out of zash will start from the same data device (a data source).

use std::io::Read;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_data_device, wl_data_device_manager, wl_data_offer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

/// What a drop delivers: the files, and the point (window pixels) they were let go at.
pub type OnDrop = Box<dyn Fn(Vec<PathBuf>, f64, f64) + Send>;

struct State {
    on_drop: OnDrop,
    offer: Option<wl_data_offer::WlDataOffer>,
    at: (f64, f64),
    conn: Connection,
}

type Mimes = Arc<Mutex<Vec<String>>>;

const URIS: &str = "text/uri-list";

/// Starts listening on winit's Wayland display (`display` is its `wl_display` pointer).
pub fn listen(display: *mut std::ffi::c_void, on_drop: OnDrop) -> Result<(), String> {
    let backend = unsafe { wayland_backend::client::Backend::from_foreign_display(display.cast()) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let ddm: wl_data_device_manager::WlDataDeviceManager = globals.bind(&qh, 1..=3, ()).map_err(|e| e.to_string())?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).map_err(|e| e.to_string())?;
    let _device = ddm.get_data_device(&seat, &qh, ());
    let mut state = State { on_drop, offer: None, at: (0.0, 0.0), conn: conn.clone() };
    std::thread::Builder::new()
        .name("zash dnd".into())
        .spawn(move || {
            // the objects live as long as this thread
            let _keep = (ddm, seat, _device);
            while queue.blocking_dispatch(&mut state).is_ok() {}
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for State {
    fn event(_: &mut Self, _: &wl_data_device_manager::WlDataDeviceManager, _: wl_data_device_manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_data_offer::WlDataOffer, Mimes> for State {
    fn event(_: &mut Self, _: &wl_data_offer::WlDataOffer, ev: wl_data_offer::Event, mimes: &Mimes, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_data_offer::Event::Offer { mime_type } = ev {
            mimes.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for State {
    fn event(st: &mut Self, _: &wl_data_device::WlDataDevice, ev: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_data_device::Event::Enter { serial, x, y, id, .. } => {
                st.at = (x, y);
                st.offer = id;
                if let Some(o) = &st.offer {
                    let has = o.data::<Mimes>().is_some_and(|m| m.lock().unwrap().iter().any(|t| t == URIS));
                    // say what we take, so the source shows the right cursor
                    o.accept(serial, has.then(|| URIS.to_string()));
                    if o.version() >= 3 {
                        o.set_actions(wl_data_device_manager::DndAction::Copy, wl_data_device_manager::DndAction::Copy);
                    }
                }
            }
            wl_data_device::Event::Motion { x, y, .. } => st.at = (x, y),
            wl_data_device::Event::Leave => {
                if let Some(o) = st.offer.take() {
                    o.destroy();
                }
            }
            wl_data_device::Event::Drop => {
                let Some(o) = st.offer.take() else { return };
                let has = o.data::<Mimes>().is_some_and(|m| m.lock().unwrap().iter().any(|t| t == URIS));
                if has {
                    if let Some(paths) = receive(&st.conn, &o) {
                        if !paths.is_empty() {
                            (st.on_drop)(paths, st.at.0, st.at.1);
                        }
                    }
                }
                if o.version() >= 3 {
                    o.finish();
                }
                o.destroy();
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, wl_data_device::WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (wl_data_offer::WlDataOffer, Mimes::default()),
    ]);
}

/// Reads the offer's file list through a pipe.
fn receive(conn: &Connection, o: &wl_data_offer::WlDataOffer) -> Option<Vec<PathBuf>> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    let (r, w) = unsafe { (std::fs::File::from_raw_fd_checked(fds[0]), std::fs::File::from_raw_fd_checked(fds[1])) };
    o.receive(URIS.to_string(), w.as_fd());
    drop(w);
    let _ = conn.flush();
    let mut text = String::new();
    let mut r = r;
    r.read_to_string(&mut text).ok()?;
    Some(parse_uri_list(&text))
}

trait FromRawChecked {
    unsafe fn from_raw_fd_checked(fd: i32) -> std::fs::File;
}

impl FromRawChecked for std::fs::File {
    unsafe fn from_raw_fd_checked(fd: i32) -> std::fs::File {
        use std::os::fd::FromRawFd;
        unsafe { std::fs::File::from_raw_fd(fd) }
    }
}

/// `text/uri-list`: one `file://` URI per line (percent-encoded), `#` lines are comments.
pub fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // file://host/path: the host part is empty or localhost
        .map(|rest| rest.strip_prefix("localhost").unwrap_or(rest))
        .map(|p| PathBuf::from(percent_decode(p)))
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn uri_lists() {
        let l = "# from a file manager\r\nfile:///home/me/My%20File.png\r\nfile://localhost/tmp/a.txt\nhttps://example.com\n";
        let p = super::parse_uri_list(l);
        assert_eq!(p, vec![std::path::PathBuf::from("/home/me/My File.png"), std::path::PathBuf::from("/tmp/a.txt")]);
    }
}

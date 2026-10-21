//! The pen (Wayland tablet protocol): winit only sees a pen as a mouse, without pressure, so zash
//! binds zwp_tablet_v2 itself on a second queue of winit's connection (as dnd.rs does). Only while
//! drawing mode is on: a client holding a tablet seat no longer gets the pen as a pointer, so
//! outside drawing mode the pen clicks and selects as before.

use std::sync::{Arc, Mutex};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2 as manager, zwp_tablet_pad_group_v2 as group, zwp_tablet_pad_ring_v2 as ring, zwp_tablet_pad_strip_v2 as strip,
    zwp_tablet_pad_v2 as pad, zwp_tablet_seat_v2 as tseat, zwp_tablet_tool_v2 as tool, zwp_tablet_v2 as tablet,
};

/// The pen at the end of one of its frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pen {
    /// near the window (in proximity of this surface)
    pub near: bool,
    pub down: bool,
    /// the eraser end
    pub eraser: bool,
    /// window position, logical pixels
    pub x: f64,
    pub y: f64,
    /// 0–1
    pub pressure: f32,
    /// the lower and upper side buttons
    pub b1: bool,
    pub b2: bool,
}

pub type OnPen = Box<dyn Fn(Pen) + Send>;

struct State {
    on_pen: OnPen,
    pen: Pen,
}

/// What the window keeps: switches the tablet seat on (drawing mode) and off.
pub struct PenCtl {
    conn: Connection,
    qh: QueueHandle<State>,
    manager: manager::ZwpTabletManagerV2,
    seat: wl_seat::WlSeat,
    tablet_seat: Mutex<Option<tseat::ZwpTabletSeatV2>>,
}

impl PenCtl {
    pub fn set(&self, on: bool) {
        let mut ts = self.tablet_seat.lock().unwrap();
        match (on, ts.is_some()) {
            (true, false) => *ts = Some(self.manager.get_tablet_seat(&self.seat, &self.qh, ())),
            (false, true) => {
                if let Some(t) = ts.take() {
                    t.destroy();
                }
            }
            _ => {}
        }
        let _ = self.conn.flush();
    }
}

/// Starts listening on winit's display; nothing reaches `on_pen` until `PenCtl::set(true)`.
pub fn listen(display: *mut std::ffi::c_void, on_pen: OnPen) -> Result<Arc<PenCtl>, String> {
    let backend = unsafe { wayland_backend::client::Backend::from_foreign_display(display.cast()) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let manager: manager::ZwpTabletManagerV2 = globals.bind(&qh, 1..=1, ()).map_err(|_| "the compositor has no tablet protocol".to_string())?;
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=7, ()).map_err(|e| e.to_string())?;
    let ctl = Arc::new(PenCtl { conn: conn.clone(), qh, manager, seat, tablet_seat: Mutex::new(None) });
    let mut state = State { on_pen, pen: Pen::default() };
    std::thread::Builder::new()
        .name("zash pen".into())
        .spawn(move || while queue.blocking_dispatch(&mut state).is_ok() {})
        .map_err(|e| e.to_string())?;
    Ok(ctl)
}

impl Dispatch<tool::ZwpTabletToolV2, ()> for State {
    fn event(st: &mut Self, _: &tool::ZwpTabletToolV2, ev: tool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        use wayland_client::WEnum;
        let p = &mut st.pen;
        match ev {
            tool::Event::Type { tool_type: WEnum::Value(t) } => p.eraser = t == tool::Type::Eraser,
            tool::Event::ProximityIn { .. } => p.near = true,
            tool::Event::ProximityOut => {
                *p = Pen { eraser: p.eraser, ..Pen::default() };
                (st.on_pen)(*p);
            }
            tool::Event::Down { .. } => p.down = true,
            tool::Event::Up => p.down = false,
            tool::Event::Motion { x, y } => (p.x, p.y) = (x, y),
            tool::Event::Pressure { pressure } => p.pressure = pressure as f32 / 65535.0,
            tool::Event::Button { button, state, .. } => {
                let held = state == WEnum::Value(tool::ButtonState::Pressed);
                match button {
                    0x14b => p.b1 = held,
                    0x14c => p.b2 = held,
                    _ => {}
                }
            }
            tool::Event::Frame { .. } => (st.on_pen)(*p),
            _ => {}
        }
    }
}

impl Dispatch<tseat::ZwpTabletSeatV2, ()> for State {
    fn event(_: &mut Self, _: &tseat::ZwpTabletSeatV2, _: tseat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    wayland_client::event_created_child!(State, tseat::ZwpTabletSeatV2, [
        tseat::EVT_TABLET_ADDED_OPCODE => (tablet::ZwpTabletV2, ()),
        tseat::EVT_TOOL_ADDED_OPCODE => (tool::ZwpTabletToolV2, ()),
        tseat::EVT_PAD_ADDED_OPCODE => (pad::ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<pad::ZwpTabletPadV2, ()> for State {
    fn event(_: &mut Self, _: &pad::ZwpTabletPadV2, _: pad::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    wayland_client::event_created_child!(State, pad::ZwpTabletPadV2, [
        pad::EVT_GROUP_OPCODE => (group::ZwpTabletPadGroupV2, ()),
    ]);
}

impl Dispatch<group::ZwpTabletPadGroupV2, ()> for State {
    fn event(_: &mut Self, _: &group::ZwpTabletPadGroupV2, _: group::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    wayland_client::event_created_child!(State, group::ZwpTabletPadGroupV2, [
        group::EVT_RING_OPCODE => (ring::ZwpTabletPadRingV2, ()),
        group::EVT_STRIP_OPCODE => (strip::ZwpTabletPadStripV2, ()),
    ]);
}

macro_rules! quiet {
    ($($t:ty => $e:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: $e, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
quiet!(
    tablet::ZwpTabletV2 => tablet::Event,
    ring::ZwpTabletPadRingV2 => ring::Event,
    strip::ZwpTabletPadStripV2 => strip::Event,
    manager::ZwpTabletManagerV2 => manager::Event,
    wl_seat::WlSeat => wl_seat::Event
);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

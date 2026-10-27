//! `zwlr_virtual_pointer_v1` (E6.11): remote desktops (wayvnc) and automation (ydotool-style tools) move, click and
//! scroll through the same path as a mouse: pointer assist, the shell's scrollers, then the app. Like the virtual
//! keyboard: not for sandboxed apps, others only once allowed, and never while locked (perms.rs).

use std::sync::Mutex;

use smithay::{
    backend::input::{Axis, AxisSource, ButtonState},
    output::Output,
    reexports::{
        wayland_protocols_wlr::virtual_pointer::v1::server::{
            zwlr_virtual_pointer_manager_v1::{self, ZwlrVirtualPointerManagerV1},
            zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, WEnum,
            backend::{ClientId, GlobalId},
            protocol::{wl_output::WlOutput, wl_pointer},
        },
    },
    utils::{Logical, Point, Rectangle},
};

use crate::state::State;

pub fn global(dh: &DisplayHandle) -> GlobalId {
    dh.create_global::<State, ZwlrVirtualPointerManagerV1, _>(2, ())
}

/// One virtual pointer: the display its absolute motion maps onto (all of them when `None`), and the scroll being
/// gathered until `frame`, and the buttons it holds (released for it when it goes away mid-drag).
pub struct Pointer {
    output: Option<Output>,
    scroll: Mutex<Scroll>,
    held: Mutex<Vec<u32>>,
}

/// Keeps `held` as the set of buttons down.
fn track(held: &mut Vec<u32>, button: u32, state: ButtonState) {
    held.retain(|&b| b != button);
    if state == ButtonState::Pressed {
        held.push(button);
    }
}

#[derive(Default)]
struct Scroll {
    source: Option<AxisSource>,
    amount: [Option<f64>; 2],
    v120: [Option<f64>; 2],
    time: u32,
}

/// `x / x_extent` across `area`, as the protocol scales absolute motion.
fn absolute(x: u32, y: u32, x_extent: u32, y_extent: u32, area: Rectangle<i32, Logical>) -> Option<Point<f64, Logical>> {
    (x_extent > 0 && y_extent > 0).then(|| {
        let f = |v: u32, extent: u32, len: i32| f64::from(v.min(extent)) / f64::from(extent) * f64::from(len);
        area.loc.to_f64() + Point::from((f(x, x_extent, area.size.w), f(y, y_extent, area.size.h)))
    })
}

fn source(source: wl_pointer::AxisSource) -> Option<AxisSource> {
    Some(match source {
        wl_pointer::AxisSource::Wheel => AxisSource::Wheel,
        wl_pointer::AxisSource::Finger => AxisSource::Finger,
        wl_pointer::AxisSource::Continuous => AxisSource::Continuous,
        wl_pointer::AxisSource::WheelTilt => AxisSource::WheelTilt,
        _ => return None,
    })
}

/// 0 horizontal, 1 vertical (`State::pointer_axis`).
fn axis(axis: WEnum<wl_pointer::Axis>) -> Option<usize> {
    let axis = Axis::try_from(axis.into_result().ok()?).ok()?;
    Some(usize::from(axis == Axis::Vertical))
}

impl GlobalDispatch<ZwlrVirtualPointerManagerV1, ()> for State {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<ZwlrVirtualPointerManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }

    fn can_view(client: Client, _: &()) -> bool {
        crate::protocols::unsandboxed(&client)
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        _: &ZwlrVirtualPointerManagerV1,
        request: zwlr_virtual_pointer_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        let (id, output): (_, Option<WlOutput>) = match request {
            zwlr_virtual_pointer_manager_v1::Request::CreateVirtualPointer { id, .. } => (id, None),
            zwlr_virtual_pointer_manager_v1::Request::CreateVirtualPointerWithOutput { id, output, .. } => (id, output),
            _ => return,
        };
        state.may_control(client);
        init.init(id, Pointer { output: output.as_ref().and_then(Output::from_resource), scroll: Mutex::default(), held: Mutex::default() });
    }
}

impl Dispatch<ZwlrVirtualPointerV1, Pointer> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        _: &ZwlrVirtualPointerV1,
        request: zwlr_virtual_pointer_v1::Request,
        data: &Pointer,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        use zwlr_virtual_pointer_v1::Request;
        if !state.may_control(client) {
            return;
        }
        let mut scroll = data.scroll.lock().unwrap();
        match request {
            Request::Motion { time, dx, dy } => {
                state.idle_activity();
                state.relative_motion((dx, dy).into(), u64::from(time) * 1000);
            }
            Request::MotionAbsolute { time, x, y, x_extent, y_extent } => {
                let area = match &data.output {
                    Some(o) => state.space.output_geometry(o),
                    None => state.space.outputs().filter_map(|o| state.space.output_geometry(o)).reduce(|a, b| a.merge(b)),
                };
                if let Some(to) = area.and_then(|a| absolute(x, y, x_extent, y_extent, a)) {
                    state.idle_activity();
                    state.pointer_motion(to, u64::from(time) * 1000);
                    state.schedule_redraw();
                }
            }
            Request::Button { time, button, state: pressed } => {
                if let Some(pressed) = pressed.into_result().ok().and_then(|s| ButtonState::try_from(s).ok()) {
                    state.idle_activity();
                    track(&mut data.held.lock().unwrap(), button, pressed);
                    state.assisted_button(button, pressed, time);
                }
            }
            Request::Axis { time, axis: a, value } => {
                if let Some(i) = axis(a) {
                    scroll.time = time;
                    scroll.amount[i] = Some(value);
                }
            }
            Request::AxisDiscrete { time, axis: a, value, discrete } => {
                if let Some(i) = axis(a) {
                    scroll.time = time;
                    scroll.amount[i] = Some(value);
                    scroll.v120[i] = Some(f64::from(discrete) * 120.0);
                }
            }
            Request::AxisStop { time, axis: a } => {
                if let Some(i) = axis(a) {
                    scroll.time = time;
                    scroll.amount[i] = Some(0.0);
                }
            }
            Request::AxisSource { axis_source } => scroll.source = axis_source.into_result().ok().and_then(source),
            Request::Frame => {
                let Scroll { source, amount, v120, time } = std::mem::take(&mut *scroll);
                drop(scroll);
                if amount.iter().any(Option::is_some) {
                    state.idle_activity();
                    state.pointer_axis(source.unwrap_or(AxisSource::Wheel), time, amount, v120);
                }
            }
            _ => {}
        }
    }

    /// Covers `destroy` and a disconnect: release what it still holds (pointer_button sends the frame).
    fn destroyed(state: &mut Self, _: ClientId, _: &ZwlrVirtualPointerV1, data: &Pointer) {
        let time = state.now_msec();
        for button in std::mem::take(&mut *data.held.lock().unwrap()) {
            state.assisted_button(button, ButtonState::Released, time);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_motion_scales_onto_the_area() {
        let area = Rectangle::new((1920, 0).into(), (2560, 1440).into());
        assert_eq!(absolute(0, 0, 100, 100, area), Some((1920.0, 0.0).into()));
        assert_eq!(absolute(50, 25, 100, 100, area), Some((3200.0, 360.0).into()));
        assert_eq!(absolute(200, 100, 100, 100, area), Some((4480.0, 1440.0).into()), "clamped to the extent");
        assert_eq!(absolute(1, 1, 0, 100, area), None);
    }

    #[test]
    fn held_buttons_track_press_and_release() {
        let mut held = vec![];
        track(&mut held, 272, ButtonState::Pressed);
        track(&mut held, 273, ButtonState::Pressed);
        track(&mut held, 272, ButtonState::Pressed);
        assert_eq!(held, [273, 272], "a repeated press is held once");
        track(&mut held, 273, ButtonState::Released);
        track(&mut held, 274, ButtonState::Released);
        assert_eq!(held, [272]);
    }
}

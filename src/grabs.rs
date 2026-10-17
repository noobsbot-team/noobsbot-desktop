//! Interactive move and resize (E1.2): pointer grabs that apply the rules in wm.rs to a live window.

use smithay::{
    desktop::Window,
    input::pointer::{
        AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
        GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
        RelativeMotionEvent,
    },
    reexports::{wayland_protocols::xdg::shell::server::xdg_toplevel, wayland_server::protocol::wl_surface::WlSurface},
    utils::{Logical, Point, Size},
    wayland::{compositor::with_states, shell::xdg::SurfaceCachedState},
};

use crate::{
    state::State,
    wm::{self, Edges, Rect, ResizeMods},
};

/// The pointer-event methods both grabs pass straight through.
macro_rules! forward_pointer_events {
    () => {
        fn relative_motion(
            &mut self,
            data: &mut State,
            handle: &mut PointerInnerHandle<'_, State>,
            focus: Option<(WlSurface, Point<f64, Logical>)>,
            event: &RelativeMotionEvent,
        ) {
            handle.relative_motion(data, focus, event);
        }
        fn axis(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, details: AxisFrame) {
            handle.axis(data, details);
        }
        fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
            handle.frame(data);
        }
        fn gesture_swipe_begin(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureSwipeBeginEvent) {
            handle.gesture_swipe_begin(data, event);
        }
        fn gesture_swipe_update(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureSwipeUpdateEvent) {
            handle.gesture_swipe_update(data, event);
        }
        fn gesture_swipe_end(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureSwipeEndEvent) {
            handle.gesture_swipe_end(data, event);
        }
        fn gesture_pinch_begin(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GesturePinchBeginEvent) {
            handle.gesture_pinch_begin(data, event);
        }
        fn gesture_pinch_update(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GesturePinchUpdateEvent) {
            handle.gesture_pinch_update(data, event);
        }
        fn gesture_pinch_end(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GesturePinchEndEvent) {
            handle.gesture_pinch_end(data, event);
        }
        fn gesture_hold_begin(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureHoldBeginEvent) {
            handle.gesture_hold_begin(data, event);
        }
        fn gesture_hold_end(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &GestureHoldEndEvent) {
            handle.gesture_hold_end(data, event);
        }
        fn start_data(&self) -> &GrabStartData<State> {
            &self.start_data
        }
        fn button(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, event: &ButtonEvent) {
            handle.button(data, event);
            // The grab lives exactly as long as a button is held.
            if handle.current_pressed().is_empty() {
                handle.unset_grab(self, data, event.serial, event.time, true);
            }
        }
    };
}

pub struct MoveGrab {
    pub start_data: GrabStartData<State>,
    pub window: Window,
    /// The window's visible rectangle when the grab began.
    pub start: Rect,
    pub started: bool,
}

impl PointerGrab<State> for MoveGrab {
    fn motion(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, _focus: Option<(WlSurface, Point<f64, Logical>)>, event: &MotionEvent) {
        // No app gets pointer focus while a window is being dragged.
        handle.motion(data, None, event);
        let delta = event.location - self.start_data.location;
        if !self.started && delta.x.hypot(delta.y) < wm::DRAG_THRESHOLD {
            return;
        }
        self.started = true;

        let rect = Rect::new(self.start.loc + delta.to_i32_round(), self.start.size);
        let others: Vec<Rect> = data.space.elements().filter(|w| **w != self.window).filter_map(|w| data.space.element_geometry(w)).collect();
        let Some(area) = data.work_area() else { return };
        let loc = wm::move_to(rect, area, &others, data.config.windows.magnetic_edges && !data.modifiers().ctrl);
        data.set_visible_loc(&self.window, loc);
    }

    fn unset(&mut self, _data: &mut State) {}

    forward_pointer_events!();
}

/// A resize in progress. Lives in `State` (not the grab) because commits finish it after the button is released.
pub struct Resize {
    pub window: Window,
    pub edges: Edges,
    pub start: Rect,
    pub symmetric: bool,
    /// The size the pointer asks for.
    pub desired: Size<i32, Logical>,
    pub sent: Option<Size<i32, Logical>>,
    /// A configure is out; the next one waits for the app's new buffer, so a slow app is never flooded.
    pub waiting: bool,
    pub released: bool,
}

pub struct ResizeGrab {
    pub start_data: GrabStartData<State>,
}

impl PointerGrab<State> for ResizeGrab {
    fn motion(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>, _focus: Option<(WlSurface, Point<f64, Logical>)>, event: &MotionEvent) {
        handle.motion(data, None, event);
        let mods = data.modifiers();
        let Some(resize) = data.resize.as_ref() else { return };
        let Some(surface) = resize.window.toplevel().map(|t| t.wl_surface().clone()) else { return };
        let (min, max) = with_states(&surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let current = cached.current();
            (current.min_size, current.max_size)
        });
        let resize_mods = ResizeMods { symmetric: mods.alt, keep_aspect: mods.shift };
        let (start, edges) = (resize.start, resize.edges);
        let mut size = wm::resize_size(start, edges, event.location - self.start_data.location, resize_mods, min, max);

        // The dragged edge is magnetic too (Ctrl turns it off); not while growing from the centre.
        if data.config.windows.magnetic_edges
            && !mods.ctrl
            && !mods.alt
            && let Some(area) = data.work_area()
        {
            let others: Vec<Rect> = data.space.elements().filter(|w| **w != resize.window).filter_map(|w| data.space.element_geometry(w)).collect();
            let xs: Vec<i32> = [area.loc.x, area.loc.x + area.size.w].into_iter().chain(others.iter().flat_map(|o| [o.loc.x, o.loc.x + o.size.w])).collect();
            let ys: Vec<i32> = [area.loc.y, area.loc.y + area.size.h].into_iter().chain(others.iter().flat_map(|o| [o.loc.y, o.loc.y + o.size.h])).collect();
            let (right, bottom) = (start.loc.x + start.size.w, start.loc.y + start.size.h);
            if edges.right {
                size.w = wm::stick(start.loc.x + size.w, 0, &xs) - start.loc.x;
            } else if edges.left {
                size.w = right - wm::stick(right - size.w, 0, &xs);
            }
            if edges.bottom {
                size.h = wm::stick(start.loc.y + size.h, 0, &ys) - start.loc.y;
            } else if edges.top {
                size.h = bottom - wm::stick(bottom - size.h, 0, &ys);
            }
        }

        if let Some(resize) = data.resize.as_mut() {
            resize.desired = size;
            resize.symmetric = resize_mods.symmetric;
        }
        data.flush_resize();
    }

    fn unset(&mut self, data: &mut State) {
        if let Some(resize) = data.resize.as_mut() {
            resize.released = true;
            // Force one last configure that drops the Resizing state.
            resize.sent = None;
        }
        data.flush_resize();
    }

    forward_pointer_events!();
}

pub fn edges_from(edge: xdg_toplevel::ResizeEdge) -> Edges {
    let bits: u32 = edge.into();
    Edges { top: bits & 1 != 0, bottom: bits & 2 != 0, left: bits & 4 != 0, right: bits & 8 != 0 }
}

impl State {
    /// Starts resizing `window` by `edges` with the pointer (an app's own edge or one of our zones).
    pub fn start_resize(&mut self, window: Window, edges: Edges, start_data: GrabStartData<State>, serial: smithay::utils::Serial) {
        let Some(pointer) = self.seat.get_pointer() else { return };
        let Some(start) = self.space.element_geometry(&window) else { return };
        self.resize = Some(Resize { window, edges, start, symmetric: false, desired: start.size, sent: Some(start.size), waiting: false, released: false });
        pointer.set_grab(self, ResizeGrab { start_data }, serial, smithay::input::pointer::Focus::Clear);
    }

    /// A press in a resize zone: a double-click on the same edge grows the window to the work area that way only,
    /// anything else starts a resize.
    pub fn zone_press(&mut self, window: Window, edges: Edges, start_data: GrabStartData<State>, serial: smithay::utils::Serial) {
        let double = self.zones.last_press.take().is_some_and(|(at, w, e)| at.elapsed() < wm::DOUBLE_CLICK && w == window && e == edges);
        if !double {
            self.zones.last_press = Some((std::time::Instant::now(), window.clone(), edges));
            return self.start_resize(window, edges, start_data, serial);
        }
        let (Some(start), Some(area)) = (self.space.element_geometry(&window), self.work_area()) else { return };
        let desired = wm::grow_to(start, area, edges).size;
        // A released resize: one configure, then the commit pins the window to the opposite edge.
        self.resize = Some(Resize { window, edges, start, symmetric: false, desired, sent: None, waiting: false, released: true });
        self.flush_resize();
    }

    /// Sends the app the size the pointer asks for, unless a configure is still unanswered.
    pub fn flush_resize(&mut self) {
        let Some(resize) = self.resize.as_mut() else { return };
        if resize.waiting || resize.sent == Some(resize.desired) {
            return;
        }
        let Some(toplevel) = resize.window.toplevel() else { return };
        let (size, released) = (resize.desired, resize.released);
        toplevel.with_pending_state(|state| {
            state.size = Some(size);
            if released {
                state.states.unset(xdg_toplevel::State::Resizing);
            } else {
                state.states.set(xdg_toplevel::State::Resizing);
            }
        });
        toplevel.send_pending_configure();
        resize.sent = Some(size);
        resize.waiting = true;
    }

    /// Called on every commit: keeps a resizing window pinned to its anchor at whatever size the app drew.
    pub fn resize_commit(&mut self, root: &WlSurface) {
        let Some(resize) = self.resize.as_mut() else { return };
        if resize.window.toplevel().is_none_or(|t| t.wl_surface() != root) {
            return;
        }
        resize.waiting = false;
        let window = resize.window.clone();
        let loc = wm::resize_anchor(resize.start, resize.edges, resize.symmetric, window.geometry().size);
        let done = resize.released && resize.sent == Some(resize.desired);
        self.set_visible_loc(&window, loc);
        if done {
            self.resize = None;
        } else {
            self.flush_resize();
        }
    }
}

---
title: "Helpers must read state.pointer_at, never lock the pointer: smithay holds that lock during grabs"
author: Anil
date: 2026-10-27
branch: feat/parity
cost: { minutes: 1100, tokens: 3752116 }
id: l-4c50af18
scope: repo
status: active
src: agent
touches:
  - src/backend/winit.rs
  - src/displays.rs
  - src/pointer_assist.rs
  - src/protocols.rs
  - src/shell/dock.rs
  - src/shell/mod.rs
  - src/shell/screenshot.rs
  - src/shortcuts.rs
  - src/spaces.rs
  - src/state.rs
---

## Decided
- `State.pointer_at` (src/state.rs) is the pointer's location, set wherever the pointer moves (protocols.rs pointer_motion and set_location, displays.rs apply_displays). Helpers read it instead of `seat.get_pointer().current_location()`.
- smithay's `PointerHandle::motion`/`button` lock the pointer's internal Mutex and call the active grab (MoveGrab, ResizeGrab in src/grabs.rs, smithay's DnD grab, which calls our ClientDndGrabHandler/ServerDndGrabHandler) with it held. Anything they reach that calls `current_location()`, `is_grabbed()`, `current_focus()` or `with_grab()` waits on itself forever.

## Tried and ruled out
- The freeze in v1.0.0-beta.1 looked like Dolphin, the clipboard, IME or GPU at first. The watchdog's SIGUSR1 backtrace (src/main.rs `stuck`, read with addr2line against a scripts/dev-install.sh build) showed MoveGrab::motion → pointer_output → current_location → futex wait.

## Watch out
- `output_area` reaches `dock_outputs` → `pointer_output`, so nearly every window-placement helper used to lock the pointer.
- Locking reads are fine only in top-level input handlers (pointer_motion, pointer_button, pointer_axis, relative_motion) before they call into smithay, and in render code that runs from the loop.

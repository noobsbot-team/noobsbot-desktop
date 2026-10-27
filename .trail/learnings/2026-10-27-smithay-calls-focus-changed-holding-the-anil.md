---
title: "smithay calls focus_changed holding the keyboard's lock: defer keyboard work to an idle"
author: Anil
date: 2026-10-27
branch: parity/audit-lock
id: l-960222af
scope: repo
status: active
src: agent
touches:
  - "src/ime.rs#layout_focus"
  - "src/state.rs#focus_changed"
---

## Watch out
smithay's `KeyboardHandle::set_focus` locks the keyboard's internal Mutex and calls our `SeatHandler::focus_changed`
(src/state.rs) with it held. Anything reached from there that calls `with_xkb_state`, `current_focus`, `modifier_state`
or `set_focus` on the keyboard waits on itself and the watchdog restarts the desktop. `layout_focus` (src/ime.rs) did
that with "Remember the input source per window" on and two or more layouts; it now sets the layout on the next idle,
and only if that surface still has focus. The text-input handle has its own lock, so `text_input_focus` is fine there.

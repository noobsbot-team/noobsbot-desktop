---
title: "A surface's state lock isn't reentrant: never call bar_height or content_size inside with_pending_state"
author: Anil
date: 2026-10-27
branch: feat/parity
cost: { minutes: 42, tokens: 185291 }
id: l-7ae7ea76
scope: repo
status: active
src: agent
touches:
  - src/state.rs
  - src/render/menubar.rs
  - src/pointer_assist.rs
  - src/config.rs
  - README.md
  - src/shell/desktop.rs
  - src/render/dock.rs
  - src/shell/dock.rs
  - src/shell/dock_fx.rs
  - src/apps.rs
---

## Watch out
smithay's `with_pending_state` / `with_states` hold the surface's mutex for the closure. `decorations::bar_height`
(and so `State::frame`, `State::content_size`) take that same lock, so calling them inside such a closure on the same
window deadlocks the event loop; the watchdog then restarts the desktop after 10 s ("no heartbeat"). It happened in
`State::untile` (src/state.rs) when dragging a tiled window off its tile. Work the value out first, then set it.

## Decided
To read a watchdog "event loop is stuck in" stack: the installed binary under
/usr/lib/noobsbot-desktop/versions/<v>/ keeps debug info, so
`addr2line -e <binary> -f -C -i -p <offsets>` with the `(+0x…)` offsets from session.log.old names the lines.

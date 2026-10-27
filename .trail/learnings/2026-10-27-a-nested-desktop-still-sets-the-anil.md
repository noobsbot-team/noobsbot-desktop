---
title: "A --nested desktop still sets the real panel's brightness"
author: Anil
date: 2026-10-27
branch: feat/parity
cost: { minutes: 8, tokens: 130276 }
id: l-39d2d15b
scope: repo
status: active
src: agent
touches:
  - src/services/brightness.rs
  - src/services/mod.rs
  - data/config.toml
  - data/system/usr/share/backgrounds/noobsbot-desktop/purple-crystal.png
  - src/config.rs
  - src/perms.rs
  - src/bell.rs
  - src/sync.rs
  - src/shell/force_quit.rs
  - src/render/wallpaper.rs
---

## Watch out
- `--nested` runs keep the real system bus, so the Control Center brightness slider and the brightness keys in a nested desktop call logind SetBrightness on the real backlight (src/services/brightness.rs `set_backlight`). Test brightness logic with unit tests, not by dragging in a nested session.

## Decided
- Slider drags are coalesced by a counter (`brightness::PENDING`, incremented in `Services::send`): queued `Command::Brightness` skip to the newest, and `send_current` stays silent while any is pending. Without it each pixel's logind write and its udev event sent older levels back and the knob snapped back under the finger (f50dc64).

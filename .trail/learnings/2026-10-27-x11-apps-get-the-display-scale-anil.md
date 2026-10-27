---
title: X11 apps get the display scale only through our patched xwayland-satellite
author: Anil
date: 2026-10-27
branch: feat/parity
cost: { minutes: 47, tokens: 458635 }
id: l-aa4db17f
scope: repo
status: active
src: agent
touches:
  - README.md
  - scripts/dev-install.sh
  - ".github/workflows/release.yml"
  - install.sh
  - src/xwayland.rs
  - scripts/build-xwayland-satellite.sh
  - vendor/xwayland-satellite/scale-from-output-size.patch
  - src/main.rs
  - src/sheets.rs
---

## Decided
- X11 apps run through xwayland-satellite v0.8.3 (install.sh XWS_REV) with our patch,
  vendor/xwayland-satellite/scale-from-output-size.patch, built by scripts/build-xwayland-satellite.sh. It ships
  beside the desktop in /usr/lib/noobsbot-desktop/versions/<v>/, and src/xwayland.rs starts that copy before PATH's.
- The patch takes an output's fractional scale from its mode size over its xdg-output logical size (on Done). A
  window's preferred scale no longer overrides that.

## Tried and ruled out
- Unpatched, an output's fractional scale comes only from a surface's wp_fractional_scale preferred_scale, which
  arrives once an X11 window is on that output. Until then Xft.dpi/XSETTINGS use the integer wl_output scale
  (ceil(1.25) = 2), so the first X11 apps of a session came up at 200% on a 125% display.
- An upstream PR with a similar fix was not built: unreviewed fork code. Our own small patch on the pinned tag instead.

## Watch out
- Bumping XWS_REV: check the patch still applies (the build script fails if not) and rerun satellite's tests.
- Check: `DISPLAY=:<n> xrdb -query` shows Xft.dpi 120 at 125% right after login, before any X11 app.

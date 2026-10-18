# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Entries are written by release-plz from Conventional Commits; don't edit released sections by hand.

## [Unreleased]

## [1.0.0-alpha.1](https://github.com/anil0403/noobsbot-desktop/releases/tag/v1.0.0-alpha.1) - 2026-10-18

### Added

- *(release)* ship the release signing public key
- *(backend)* real-screen udev backend, shared frame assembly (E6.1-E6.3)
- *(lock)* lock screen, session-lock, idle timers and logind sleep
- *(screenshot)* Super+Shift+3/4/5 screenshots, wlr-screencopy and screen recording permission
- *(settings)* System Settings app with sidebar, search and grouped pages (E7.1)
- *(update)* notify when a newer release is out, Details opens its page (E6.13)
- *(ime)* input-method-v2 for fcitx5, candidate popup under the cursor, layout menu and Super+Ctrl+Space (E6.12)
- *(anim)* D10 springs and window open/close/minimise/restore/zoom animations (E5.5)
- *(input)* touchpad system gestures and hot corners (E5.3, E5.4)
- *(session)* start xdg-user-dirs with the session target (E6.7)
- *(session)* roll back an update that keeps crashing, export DISPLAY to the session (E6.13, E6.9)
- *(xwayland)* start and supervise xwayland-satellite, DISPLAY for launched apps (E6.9)
- *(spaces)* Spaces per output, Mission Control and App Exposé (E5.1, E5.2)
- *(dock)* right-click menu, drag to reorder/pin/unpin, file drops, Ctrl+right-click settings
- *(apps)* fill %f/%F/%u/%U with dropped files
- *(launchpad)* full-screen app grid with search, paging and dock icon (E4.4)
- *(dock)* Plank bounces, urgent glow, hide slide, zoom easing, LauncherEntry badges, minimised tiles, Plank importer (E3.1)
- *(spotlight)* Super+Space search and launcher (E4.3)
- *(symbols)* magnifying glass and calculator symbols
- *(switcher)* real app icons in the switcher and Force Quit list
- *(bluetooth)* PIN and passkey entry alert for legacy pairing (E2.3)
- *(disks)* automount USB drives over UDisks2 with connected and safe-to-remove notifications
- *(network)* NetworkManager secret agent answered by the Wi-Fi password alert (E2.3)
- *(control)* Wi-Fi password alert with a secure field for unknown secured networks (E2.3)
- *(notifications)* notification server, banners and Notification Center (E4.1, E4.2)
- *(text)* left-aligned wrapped text with a line cap
- *(shell)* app switcher and Force Quit window with not-responding detection (E4.5, E4.7)
- *(ui)* D8 component helpers, text field and --show gallery (E2.1)
- *(dock)* Plank Transparent-Tight dock with pinned and running apps, zoom and intelligent hide
- *(apps)* app index from desktop entries with icon-theme icons
- *(quake)* F12 shows and hides noobsbot --quake above fullscreen apps (E2.8)
- *(cli)* --version prints version, codename, build date and commit
- *(update)* daily Software Update check against GitHub releases
- *(install)* install.sh for a fresh Arch system, with --dry-run and a staging test (E6.10)
- *(session)* Helium Wayland and keyring flags (F2)
- *(session)* UPower critical-battery drop-in and portal-wlr screen chooser (E6.5, E6.8)
- *(session)* greetd login config and lock-screen PAM service (E6.6)
- *(session)* login session wrapper, wayland-sessions entry and session target (E6.7)
- *(tray)* StatusNotifierItem tray icons with dbusmenu menus (E2.2)
- *(bluetooth)* BlueZ pairing agent with confirm and code alerts
- *(control)* Dark Mode toggle rewrites the app theme when theme_apps is on
- *(theme)* noobsbot-desktop theme --write generates the Noobsbot Glass app theme (E2.9)
- *(config)* default browser binding is Helium
- *(control)* Wi-Fi network list with join
- *(config)* [appearance] theme_apps key and an XDG dir helper
- *(control)* Bluetooth device list with connect and disconnect
- *(control)* sound output picker in the Sound module (E2.3)
- *(alert)* Restart, Shut Down and Log Out ask first, with a 60 s countdown (E2.2)
- *(control)* Control Center panel (E2.3)
- *(osd)* on-screen display for volume, brightness, mute and media keys (E2.7)
- *(menubar)* logo and app menus (E2.2)
- *(calendar)* calendar popover from the menu-bar clock (E2.5)
- *(menubar)* glass menu bar with app name, status symbols and clock; windows stay below it (E2.2, E2.4)
- *(ui)* tabular digits for labels and a one-colour Noobsbot mark
- *(input)* volume, mute, mic-mute, brightness and media keys (E2.7, without the OSD)
- *(ui)* Phosphor symbols rasterised by resvg and tinted, cached (E2.1)
- *(services)* sound output list and default-output switch for the picker (E2.3)
- *(services)* MPRIS Now Playing on the session bus (E2.3)
- *(services)* PipeWire sound via pw-dump monitor and wpctl (E2.3)
- *(services)* backlight brightness via sysfs and logind SetBrightness (E2.3)
- *(services)* power-profiles-daemon power mode (E2.3)
- *(services)* BlueZ adapter and connected devices; one shared follow loop for every client (E2.3)
- *(services)* NetworkManager primary connection and Wi-Fi bars (E2.3)
- *(services)* zbus services thread with UPower battery and timedated time zone (E2.3, E2.6)
- *(clock)* menu-bar clock text and minute-aligned ticks (E2.4)
- *(calendar)* month-grid math for the calendar popover (E2.5)
- *(render)* --test-scene material gallery (E1.4)
- *(protocols)* explicit sync and dmabuf fence waits
- *(protocols)* advertise text-input-v3
- *(protocols)* linux-dmabuf feedback from the EGL render node
- *(frame)* frame clock draws as late as safely possible before vblank, falls back after 3 misses (E1.10, E1.11)
- *(decorations)* hovering any traffic light shows × – ⤢ on all three (E1.3, D2)
- *(wm)* new windows fit the work area: bounds hint, and oversized ones are shrunk (E1.2)
- *(decorations)* our macOS title bar with traffic lights, server-side via xdg-decoration and KDE (E1.3)
- *(desktop)* wallpaper with built-in opal default (E1.5)
- *(render)* glass, rounded-corner and shadow shaders (E1.4)
- *(text)* cosmic-text labels with truncation and an LRU cache (E1.3)
- *(protocols)* layer-shell, presentation-time, pointer constraints and the rest of E1.1/E1.7
- *(wm)* no focus stealing, new windows need the user or a valid activation token (E1.2)
- *(wm)* double-click an edge zone grows the window to the work area that way (E1.2)
- *(input)* resize cursors in zones, app cursors on the host window (E1.2)
- *(wm)* resize zones outside every window edge (E1.2)
- *(main)* panic hook logs message, location and backtrace before abort (E1.11)
- *(input)* keyboard layout from config or the system, re-applied on reload (E1.6)
- *(config)* settings file with live reload, shortcuts and window settings from it (E1.8)
- *(input)* desktop shortcuts with spawn, close, quit, minimise, hide and cycle (E1.6)
- *(wm)* zoom and fullscreen with restore; dragging a zoomed window un-zooms it (E1.2)
- *(wm)* Super + drag anywhere moves a window (E1.2)
- *(wm)* move and resize windows from the app's title bar and edges (E1.2)
- *(wm)* placement, magnetic edges and resize geometry rules (E1.2)
- *(compositor)* xdg-activation, xdg-foreign, xdg-dialog, viewporter; draw only on change
- *(clipboard)* keep the clipboard after the copying app quits (E1.1)
- *(compositor)* nested Smithay compositor core (E1.1)

### Fixed

- *(capture)* map read-back pixels after the swap and mark nested frames y-inverted
- *(screenshot)* keep toolbar mode symbols regular when chosen
- *(control)* save the Dark Mode toggle to config.toml
- *(launchpad)* Launchpad rocket icon in the dock
- *(notify)* whole notification icons, looked up like dock icons (WhiteSur first)
- *(switcher)* visible selection highlight on light hud glass
- *(theme)* find user-installed WhiteSur, one folder for both variants; light toolbar without it
- *(text)* apply the label colour's alpha, not just glyph coverage
- *(render)* let shadows fade out to 3 sigma
- *(clipboard)* cap transfer threads at 32 in flight
- vendor wayland-backend with a fix for a client-triggered panic
- *(decorations)* opaque title bars, so a window behind never shows its title through (D2 contrast)
- *(wm)* window move can't panic on a tiny window or output; note why ClientState lookup is unreachable (E1.11)
- *(wm)* window rules take effect at a window's first configure, float_above survives placement (E1.9)

### Other

- *(release)* one-time signing key and release-plz setup script
- install the udev backend's system libraries (libseat, libinput, libdrm, gbm)
- Merge real-screen udev backend and shared frame assembly (E6.1-E6.3)
- Merge screenshots and screen-recording permission (E6.8, E6.11)
- Merge branch 'worktree-agent-ad7685751a956af8f' into worktree-m1-compositor-core
- Merge lock screen, idle and power (E6.4, E6.5)
- Merge session, XWayland and update glue (E6.6, E6.7, E6.9, E6.10, E6.13)
- Merge input methods (E6.12)
- Merge window animations and D10 springs (E5.5)
- *(dock)* drop stale not-built comment
- Merge Spaces and Mission Control (E5.1, E5.2)
- Merge Launchpad (E4.4)
- Merge dock right-click menu, drag to reorder and pin, file drops (E3.1)
- Merge dock bounces, glow, hide slide, badges and Plank importer (E3.1)
- Merge Spotlight (E4.3)
- cargo fmt
- Merge branch 'worktree-agent-a10dcbb40ae486498' into worktree-m1-compositor-core
- *(disks)* debug-log drive notices
- cargo fmt
- Merge app switcher and Force Quit (E4.5, E4.7)
- Merge notifications and Notification Center (E4.1, E4.2)
- Merge component gallery (E2.1)
- Merge branch 'worktree-agent-a9d5639b2a99fcf87' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-a690678761ce88817' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-a5b0ca15a4c880144' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-a826046ad1d4f97aa' into worktree-m1-compositor-core
- *(dock)* skip the dock lookups for pointer motion away from it
- *(plan)* UPower drop-in, verified package names, data/system layout
- *(release)* release-plz version bump, changelog and tagging
- *(release)* build, minisign-sign and attach the release tarball
- Merge branch 'worktree-agent-a62a8d33f3b28b590' into worktree-m1-compositor-core
- fmt, clippy, test and release build on push and pull request
- *(plan)* Helium replaces Brave as the default browser
- Phosphor 2.1.1 symbols (regular + fill) used by the shell, with the MIT license
- *(perf)* M2 memory and idle with all service clients
- *(perf)* M2 services thread memory and idle wakeups
- *(plan)* fix the October 2026 examples (the 17th is a Saturday)
- *(perf)* 9-slice shadows measured slower, keep per-pixel
- *(perf)* record the E1.4 glass stress test
- add PERF.md for M1, measure RAM as USS
- add a wire-level fuzzing client (E1.11)
- *(text)* load and warm fonts on a worker thread at startup
- *(wm)* apply window rules at first configure, on app_id and title change (E1.9)
- run fmt, clippy and tests on every push
- set up Trail-code
- use a placeholder name in mockups
- license under MIT
- add Noobsbot Desktop v1 (Opal) design plan

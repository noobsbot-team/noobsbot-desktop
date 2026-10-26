# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Entries are written by release-plz from Conventional Commits; don't edit released sections by hand.

## [Unreleased]

## [1.0.0-alpha.8](https://github.com/noobsbot-team/noobsbot-desktop/compare/v1.0.0-alpha.7...v1.0.0-alpha.8) - 2026-10-26

### Added

- *(update)* noobsbot-desktop --update runs Software Update from a terminal (E6.13)

### Other

- Merge pull request #12 from noobsbot-team/chore/noobsbot-team-urls
- repo URLs point to noobsbot-team after the move from anil0403

## [1.0.0-alpha.7](https://github.com/noobsbot-team/noobsbot-desktop/compare/v1.0.0-alpha.6...v1.0.0-alpha.7) - 2026-10-26

### Added

- *(install)* Debian 13 support, with Auto and Advanced modes

### Fixed

- *(install)* allow staged test runs as root
- *(update)* name the release tarball after the CPU arch
- *(apps)* find icons in a theme dir that has no index.theme
- *(idle)* new idle times apply without waiting for input
- *(lock)* darken the wallpaper on every display while locked
- *(displays)* bring back a window left mostly on an unplugged display

### Other

- *(debian)* install dbus-daemon for the polkit agent test
- *(vm)* stop the chipset watchdog resetting the guest after suspend
- *(vm)* keep the GPU's framebuffers across suspend (no-soft-reset PCIe port)
- *(vm)* relative mouse moves and drags across displays
- *(vm)* a second display the tests can plug and unplug (HEADS=2, head.py)

## [1.0.0-alpha.6](https://github.com/noobsbot-team/noobsbot-desktop/compare/v1.0.0-alpha.5...v1.0.0-alpha.6) - 2026-10-20

### Other

- *(config)* wait for both files the save thread writes

## [1.0.0-alpha.5](https://github.com/noobsbot-team/noobsbot-desktop/compare/v1.0.0-alpha.4...v1.0.0-alpha.5) - 2026-10-20

### Added

- *(settings)* Displays page with arrangement, per-display settings and keep-or-revert prompt
- *(dock)* import Plank's dock on first start, with every hide mode and behaviour setting
- *(dock)* draw Plank's hover lighten, click darken, two dots, tooltip, dragged icon and poof
- *(dock)* behave like Plank
- *(display)* per-display settings ([[display.outputs]]) applied live, displays.json for Settings
- *(drawer)* track recent apps from Spotlight and Launchpad; README
- *(drawer)* app drawer with categories, search, favorites, recent apps and session buttons
- *(apps)* read Comment, GenericName, Keywords, Categories and the entry's path

### Fixed

- *(ui)* Launchpad, Spotlight and the desktop show the placeholder for an iconless app
- *(dock)* README says what the dock does by default, how to re-import Plank, where the drawer's state lives
- *(input)* repeat keys held in the lock screen, Spotlight, switcher and other shell surfaces
- *(dock)* a nested run doesn't import Plank into the real config
- *(dock)* import Plank's pre-dconf settings keyfile too
- *(displays)* revert unkept display settings from the compositor
- *(dock)* brackets inside quotes or comments don't end a multi-line items array
- *(drawer)* "Add to Desktop" leaves an existing launcher alone
- *(drawer)* a category shows once the pointer rests on it
- *(drawer)* the scroll never runs past a list that shrank
- *(drawer)* no gap for a hidden dock
- *(drawer)* sidebar rows tighten on short screens before a category is dropped
- *(dock)* zoomed icons sit on whole pixels
- *(apps)* run an app in its Path= directory
- *(displays)* round custom scales to 1/120 steps
- *(idle)* turn real screens off at the Off stage
- *(text)* pad labels to a whole logical size at fractional scales
- *(dock)* no bounce or Recent entry when an app fails to start
- *(theme)* let GTK follow dark mode and make auto follow the clock
- *(drawer)* a non-UTF-8 byte in favorites or recent-apps no longer empties it
- *(dock)* unpinning drops every entry naming the app
- *(anim)* close and minimise ghosts on the window's own display
- *(dock)* one icon size ceiling for config and Ctrl+scroll
- *(ui)* keep overflowing desktop icons on the screen
- *(apps)* watch ~/.local/share/applications before it exists
- *(drawer)* search ignores accents and matches untranslated keywords too
- *(ui)* cap alert text so the buttons stay on screen
- *(gestures)* no hot corner where another display meets the screen
- *(ui)* keep menus on screen and scroll ones taller than it
- *(dock)* dragging a builtin off the dock no longer unpins it
- *(input)* map touchscreens and tablets onto their own display
- *(dock)* drop cached icons when the desktop files change
- *(dock)* a grey placeholder when the theme has no fallback icon either
- *(displays)* refit zoomed and fullscreen windows when displays change
- *(drawer)* a wheel notch scrolls one row
- *(displays)* bring the pointer and windows of a gone display onto the ones left
- *(apps)* honour OnlyShowIn, NotShowIn and TryExec
- *(ui)* shrink crowded Spaces bar cells so + stays visible
- *(spaces)* switching Spaces moves only the active display's windows
- *(drawer)* "Add to Dock" pins after every icon, builtins included
- *(ui)* keep the menu bar's app name clear of the status items
- *(displays)* each window gets the scale of the output it is on
- *(dock)* a fullscreen window on another output no longer hides the dock
- *(dock)* intellihide compares dock items, not pids
- *(apps)* find a plain desktop file by a reverse-DNS app id
- *(dock)* name windows after their program, resolve each window's item once per app load
- *(ui)* cap Control Center lists to the screen and scroll them
- *(displays)* re-snap saved positions after a scale, mode or rotation change
- *(ical)* alarm math saturates, no allocation from the TZif header
- *(tray)* cap registered items at 64
- *(polkit)* cancel only the request it names, drop every duplicate user
- *(lock)* passwords are wiped for real, PAM and greetd can't hold the field forever
- *(notifications)* bound the icon cache
- *(render)* cap the animation snapshot at GL_MAX_TEXTURE_SIZE
- *(install)* pin the terminal installer by commit, root's tar drops the archive's owner
- SIGTERM log panic, 0 Hz modes, symlink loops, capture sets
- hostile calendar values and a CR in capped text no longer panic
- *(dock)* keep pins when config.toml doesn't parse, titles with İ, Plank's uint32 values
- *(services)* untrusted paths and ids from apps
- *(pam)* answer hidden prompts only, refuse an empty user, fall back to system-auth
- *(lock)* no client lock over ours, unlock only by the confirmed locker, every output covered
- *(render)* give labels their whole buffer as source so fractional scales don't crop them
- *(render)* labels, desktop names and the screenshot thumbnail drawn whole at fractional scales
- *(spotlight)* allow the large App variant of Hit
- *(apps)* allow the large App variant of Entry

### Other

- *(polkit)* the dedup case holds when the tests run as root
- *(shell)* dock, drawer and desktop saves go through the config-save thread
- Merge branch 'worktree-agent-a6aacb8353d78706c' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-ad18c34a38d2be719' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-a629462e06967ad2f' into worktree-m1-compositor-core
- *(anim)* sample animations at the frame clock's target vblank
- *(symbols)* evict rasterised symbols with the text cache's LRU
- *(wallpaper)* decode no bigger than the largest output, refuse > 8192 px
- ignore __pycache__
- *(wallpaper)* one opal gradient per output size
- *(text)* cache a label that fits under one key, whatever its width limit
- *(config)* save settings on a worker, coalesced, and skip reloading our own writes
- *(apps)* index lookup keys once per load
- *(icons)* rasterise app and tray icons off the frame thread
- ext-session-lock client against a nested desktop
- Merge branch 'worktree-agent-a34439c8618597825' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-a2cb395c4642cd52c' into worktree-m1-compositor-core
- *(readme)* per-display settings in the settings table

## [1.0.0-alpha.4](https://github.com/noobsbot-team/noobsbot-desktop/compare/v1.0.0-alpha.3...v1.0.0-alpha.4) - 2026-10-18

### Added

- *(display)* [display] scale setting, applied live; Print and Shift+Print take screenshots

### Other

- cargo fmt
- README covering what it is, how it works and differs, install, use, settings, updates and development

## [1.0.0-alpha.3](https://github.com/noobsbot-team/noobsbot-desktop/compare/v1.0.0-alpha.2...v1.0.0-alpha.3) - 2026-10-18

### Fixed

- *(install)* --greeter's last line no longer says text login screen
- *(session)* keep the session's log in ~/.local/state/noobsbot-desktop/session.log
- *(update)* drop the update notice once the update is installed
- *(install)* a failing AUR theme build no longer aborts the whole install

### Other

- *(vm)* scripts for a throwaway QEMU test VM (install.sh, greeter, polkit, Software Update)

## [1.0.0-alpha.2](https://github.com/noobsbot-team/noobsbot-desktop/compare/v1.0.0-alpha.1...v1.0.0-alpha.2) - 2026-10-18

### Added

- *(weather)* Now shows the current conditions; weather toggle, city and units in System Settings
- *(settings)* follow config.toml changes made elsewhere; no caret wakeups at idle
- *(desktop)* desktop icons for the Desktop folder (E7.2, L12)
- *(text)* centred line-capped wrapping; draw icon themes' embedded PNGs
- *(weather)* Open-Meteo Weather widget in Notification Center, off by default
- *(polkit)* own authentication agent with the password in our alert (E7.2)
- *(update)* background signed download, Restart to Update, beta channel (E6.13)
- *(update)* minisign-verified privileged install helper (E6.13)
- *(greeter)* graphical login screen as a greetd greeter (E7.2)
- *(capture)* read the screen back on the real screen too, any output transform
- *(calendar)* .ics events with Up Next, event dots and reminders (E7.2)
- *(ime)* list and switch fcitx5 input methods in the input menu
- *(gestures)* Show Desktop on a 4-finger spread and a show-desktop action
- *(motion)* fade popovers, the OSD and banners out; lay the bar out before its menus
- *(emoji)* emoji picker popover with search, Recent row and category tabs (E7.2)

### Fixed

- *(ime)* send text-input enter/leave without an input method
- *(greeter)* ring hugs the picked user's picture
- *(update)* Restart to Update needs the installed helper
- *(emoji)* draw picker emoji in the colour emoji font
- *(greeter)* drop an unused type alias

### Other

- *(release)* read the last version from git tags (git_only)
- *(polkit)* identities test also holds when run as root (CI)
- *(plan)* mark the E7.2 extras done; L6, L12 and Open-5 settled
- cargo fmt after the E7.2 merges
- Merge branch 'worktree-agent-ab2253e4d740cdaca' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-a901a3602a0188690' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-ab4a766d91649324a' into worktree-m1-compositor-core
- Merge branch 'worktree-m1-compositor-core' into worktree-agent-ab2253e4d740cdaca
- Merge branch 'worktree-agent-ae436951edddb572f' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-a54b0dc818ff649e0' into worktree-m1-compositor-core
- Merge branch 'worktree-agent-a6fe217bbe95274d4' into worktree-m1-compositor-core
- *(plan)* graphical login screen done as an opt-in greetd greeter (E7.2)
- Merge branch 'worktree-agent-a69b3d6465af9848a' into worktree-m1-compositor-core
- Merge worktree-m1-compositor-core into the polkit agent branch
- Merge branch 'worktree-agent-aa182b612f71235ff' into worktree-m1-compositor-core
- Merge worktree-m1-compositor-core into the greeter branch

## [1.0.0-alpha.1](https://github.com/noobsbot-team/noobsbot-desktop/releases/tag/v1.0.0-alpha.1) - 2026-10-18

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

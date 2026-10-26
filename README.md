# Noobsbot Desktop

A complete macOS-style desktop for Linux, with a glassy theme, written in Rust on Wayland.
It turns a fresh Arch Linux or Debian 13 install into a full desktop: windows with traffic lights, a menu bar, a dock, Spotlight,
Launchpad, Notification Center, Control Center, Spaces, Mission Control, touchpad gestures, a lock screen, a login
screen and System Settings.

Version 1 codename: **Opal**, a gem that looks like glass with soft light inside but isn't see-through, like the
design: glassy, yet mostly solid.

Made by the Noobsbot Team. Sister project: [noobsbot-terminal](https://github.com/noobsbot-team/noobsbot-terminal), the
default terminal.

> **Status: early releases (0.x).** Every planned part is built and tested in a nested window and in a virtual
> machine (install, real-screen session, lock, login screen, password prompts, updates and rollback). It has not been
> run on enough real hardware yet (Intel, AMD, NVIDIA, laptops with two GPUs, several monitors), so expect rough
> edges there. Bug reports are welcome.

## Contents
- [What it is](#what-it-is)
- [How it is different](#how-it-is-different)
- [How it works](#how-it-works)
- [Requirements](#requirements)
- [Install](#install)
- [First login](#first-login)
- [Using it](#using-it)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Touchpad gestures](#touchpad-gestures)
- [Settings](#settings)
- [Updates](#updates)
- [Privacy and security](#privacy-and-security)
- [Apps that come with it](#apps-that-come-with-it)
- [Troubleshooting](#troubleshooting)
- [Uninstalling](#uninstalling)
- [Building and developing](#building-and-developing)
- [Known limits](#known-limits)
- [Project layout](#project-layout)
- [License](#license)

## What it is
Noobsbot Desktop is everything you see after you log in. One program, `noobsbot-desktop`, is the Wayland
compositor, the window manager and the whole shell: it draws the windows, the menu bar, the dock and every popover
itself. Apps (the terminal, browser, file manager, editor) are ordinary separate programs.

- **Looks and behaves like macOS:** traffic-light title bars, a global menu bar with the clock, a magnifying dock,
  Spotlight (Super+Space), Launchpad, Mission Control, Spaces, three- and four-finger gestures, Notification Center
  with a calendar, Control Center with sound, Wi-Fi, Bluetooth, brightness and Dark Mode.
- **Glassy, without blur:** surfaces are mostly solid glass with soft light and edges, in light and dark mode, with
  the eight macOS accent colours. No blur keeps it sharp and fast.
- **Fast and light:** it aims for 0% CPU when nothing changes, under 100 MB of memory for the whole desktop and
  under half a second to start. Measurements are in [`PERF.md`](PERF.md).
- **Self-contained:** it installs on a fresh Arch or Debian 13 system and needs no other desktop. Nothing from any other desktop
  is installed (Dolphin and Kate bring some Qt libraries, which load only when they're open).

## How it is different
| | Noobsbot Desktop | Traditional desktops | Tiling compositors | macOS themes on another desktop |
|---|---|---|---|---|
| Look | macOS, designed as one system (Part D of `plan.md`) | their own | minimal, you build the bar and launcher yourself | a skin over a different layout and behaviour |
| Shell | built into the compositor: one process | many processes (shell, panels, daemons) | compositor plus separate bars and launchers | the host desktop's |
| Behaviour | macOS: menu bar, dock, Spotlight, Spaces, Mission Control, gestures, hot corners | their own | tiling, keyboard first | the host desktop's |
| Setup | one script on a fresh Arch or Debian install | distro packages | hand-written configs | theme packs and extensions |
| Config | one commented TOML file, applied as you save, plus a Settings app | GUI settings and dconf/KConfig | text files | several places |
| Updates | signed releases, installed with "Restart to Update", automatic rollback | distro packages | distro packages | — |

Things it does on purpose:
- **One process** for compositor and shell. Popovers, menus and animations never wait on another process, and
  there is nothing to keep in sync.
- **Its own small widget set** instead of GTK or Qt inside the shell, so every element follows the same design and
  nothing redraws unless it changed.
- **No polling:** services (sound, network, battery, Bluetooth) push changes over D-Bus. Idle means idle.
- **Keyboard model:** Super is the desktop's key and Ctrl stays with the apps, so desktop shortcuts never fight app
  shortcuts. An optional "Mac keys" mode makes Super act as Cmd (Super+C copies).

## How it works
```
 ┌──────────────────────────────────────────────────────────────────────┐
 │  APPS: noobsbot-terminal · Helium · Dolphin · Kate · Settings · …     │  separate programs
 ├──────────────────────────────────────────────────────────────────────┤
 │  noobsbot-desktop  (one program)                                      │
 │   ├ compositor + window manager (owns the screen, draws everything)   │
 │   ├ shell: menu bar · dock · Spotlight · Launchpad · Notification     │
 │   │        Center · Control Center · app switcher · Mission Control · │
 │   │        Spaces · gestures · lock screen · OSD · alerts             │
 │   └ system glue: talks to the services below over D-Bus              │
 ├──────────────────────────────────────────────────────────────────────┤
 │  SERVICES: PipeWire · NetworkManager · BlueZ · UPower · udisks2 ·     │  standard Linux
 │            logind · polkit · portals                                  │  background services
 ├──────────────────────────────────────────────────────────────────────┤
 │  KERNEL + DRIVERS + FIRMWARE (linux, linux-firmware, mesa)            │  from Arch or Debian
 └──────────────────────────────────────────────────────────────────────┘
```
- **Compositor:** built on [Smithay](https://github.com/Smithay/smithay). On a real screen it drives the GPU directly
  (DRM/KMS, libinput, logind seat); during development it runs as a window inside another desktop ("nested").
- **Drawing:** OpenGL ES 3 with its own shaders for glass, rounded corners and shadows. Only the parts of the screen
  that changed are redrawn, and fullscreen video and games go straight to the screen (direct scanout).
- **Text:** [cosmic-text](https://github.com/pop-os/cosmic-text) with Inter, JetBrains Mono and Noto fonts.
- **System services:** [zbus](https://github.com/dbus2/zbus) talks to PipeWire, NetworkManager, BlueZ, UPower,
  udisks2, logind, timedated and power-profiles-daemon on a background thread, so the screen never waits on them.
- **Old X11 apps:** run through `xwayland-satellite`.
- **Settings:** one file, `~/.config/noobsbot-desktop/config.toml`. Saving it applies the change at once. System
  Settings (`noobsbot-desktop-settings`, built with [iced](https://github.com/iced-rs/iced)) edits the same file.
- **Login:** greetd starts the session, either through `tuigreet` (text, the default) or through Noobsbot Desktop's
  own graphical login screen (`--greeter`).
- **Session:** a small wrapper (`noobsbot-desktop-session`) starts the desktop under systemd's user session, keeps a
  log, and rolls back a broken update.

The full design, with the reasoning behind every choice, is in [`plan.md`](plan.md).

## Requirements
- **Arch Linux** or **Debian 13 (trixie)**, freshly installed or existing. Other distributions (and other Debian
  versions) aren't supported by the installer. The prebuilt release is for x86_64; on other CPUs (arm64) the
  installer builds it from source.
- A normal user account with `sudo` (in the `wheel` group on Arch, `sudo` on Debian). Don't run the installer as root.
- An internet connection during install.
- A GPU with OpenGL ES 3: Intel, AMD, or NVIDIA with `nvidia-open` (driver 555 or newer). Virtual machines with
  virtio-gpu work too, with or without 3D. On Debian, NVIDIA cards use the open nouveau driver unless you choose
  NVIDIA's driver (550, older than the 555 the desktop is tested with) in an Advanced install.
- About 2–3 GB of disk for the desktop, apps, fonts and themes.

## Install
On a fresh Arch install, logged in as your user:
```sh
sudo pacman -S --needed git
git clone https://github.com/noobsbot-team/noobsbot-desktop.git
cd noobsbot-desktop
./install.sh
```
On a fresh Debian 13 install, logged in as your user, it is the same with `sudo apt install git` first. Tip: in the
Debian installer's "Software selection" step, choose only **standard system utilities** (no desktop); the script
installs everything else. If you set a root password there, your user has no `sudo` yet: run
`su -c "apt install sudo && usermod -aG sudo $USER"`, then log out and in again.

Then reboot.

At the start, the script asks how to install:
- **Auto** (recommended): the recommended choices below, no more questions. This is also what runs without a
  terminal, with `--update`, and with `--auto`.
- **Advanced**: asks each question first, shows a summary to confirm, then runs by itself. Your answers are kept in
  `~/.config/noobsbot-desktop/installer.conf`, so later repairs and updates keep them.

| Question | Auto's answer | Other answers |
|---|---|---|
| Debian's non-free firmware sources (Debian, only if missing) | add them | free software only |
| Another login manager is enabled (GDM, SDDM, …) | keep it (pick Noobsbot Desktop in its session menu) | switch to greetd |
| Login screen | text (tuigreet) | graphical (`--greeter`) |
| Browser | Helium | Firefox (ESR on Debian), Chromium, none |
| NVIDIA driver | Arch: `nvidia-open`; Debian: nouveau | Arch: nouveau; Debian: NVIDIA 550 |
| Prebuilt or source | signed prebuilt | build from source (`--from-source`) |
| Network set up by the Debian installer (DHCP) | move it to NetworkManager at the next boot, so the menu bar can manage it; Wi-Fi is kept | leave `/etc/network/interfaces` |

Sound is always PipeWire: the volume and sound menus talk to it. On Debian, PulseAudio (if an earlier desktop
installed it) is masked, not removed.

What `install.sh` does:
1. Checks this is Arch or Debian 13, you're not root, and `sudo` works.
2. Installs the packages the desktop needs (the list is in `plan.md` G4), with only the GPU, CPU-microcode and
   video drivers your machine needs.
3. Arch: installs `paru` and a few AUR packages: the Helium browser and the WhiteSur icons, cursors and themes.
   Debian: adds Helium's own signed apt repository, and installs the WhiteSur themes from their git repositories
   (pinned versions). If a theme fails to build upstream, it warns and carries on; the desktop works without it.
4. Downloads the latest **signed prebuilt release** from GitHub, checks its signature and checksum against the key
   built into the script, and installs it into `/usr/lib/noobsbot-desktop/versions/<version>/`. Nothing is compiled
   on your machine. If there is no prebuilt for your CPU, or it doesn't fit your system's libraries, it builds that
   release from source instead (and installs Rust for it).
5. Installs noobsbot-terminal, the system files (session, greetd, PAM, polkit, portals), enables the services and
   writes your default settings, keeping any settings you already have. On Debian it also installs
   `xwayland-satellite` (for X11 apps; Debian has no package) to `/usr/local/bin`.

Running it again is safe: it only redoes what is missing or changed, so it also repairs an install.

Options:
| Command | What it does |
|---|---|
| `./install.sh` | install or repair, using the signed prebuilt release; asks Auto or Advanced |
| `./install.sh --auto` | install or repair with the recommended choices, no questions |
| `./install.sh --advanced` | answer the questions above first |
| `./install.sh --greeter` | also use Noobsbot Desktop's graphical login screen instead of the text one (tuigreet) |
| `./install.sh --from-source` | build Noobsbot Desktop and noobsbot-terminal from pinned git tags instead (needs Rust; slower) |
| `./install.sh --update` | only update Noobsbot Desktop, noobsbot-terminal and the system files |
| `./install.sh --dry-run` | print every action and change nothing |
| `DESTDIR=<dir> ./install.sh` | stage the files under `<dir>` only (no sudo, packages or services); for testing |
| `./install.sh --help` | show these options |

You can also check a release yourself: every release has `SHA256SUMS` and `.minisig` signatures, verifiable with
`minisign -Vm SHA256SUMS -P <the MINISIGN_PUBKEY in install.sh>`.

## First login
After the reboot, greetd shows the login screen:
- **Text login (default):** pick "Noobsbot Desktop" and log in.
- **Graphical login (`--greeter`):** looks like the lock screen. With several users, click one or press Tab; the
  session name in the bottom-left corner switches sessions; Sleep, Restart and Shut Down are at the bottom.

The desktop starts with the menu bar at the top and the dock at the bottom. Press **Super+Return** for a terminal,
**Super+Space** for Spotlight, or click the Launchpad icon in the dock to see every app.

## Using it
- **Windows:** the red, yellow and green buttons close, minimise and go full screen
  (Alt+green zooms; Super+Ctrl+F toggles full screen). Drag the title bar, or hold Super and drag
  anywhere in a window. Drag a window to a side of the screen for a half, near a corner for a quarter, or to the top to fill the screen;
  drag it away for its old size. Edges stick to other
  edges while you drag (hold Ctrl to stop that). Double-click a title bar to zoom.
- **Menu bar:** the logo menu (About, Sleep, Restart, Shut Down, Lock, Log Out, Force Quit, Software Update), the
  focused app's menu, then status icons: input source, battery, Wi-Fi, Control Center and the clock. Most Linux apps
  don't publish their menus, so the app menu has About, Hide and Quit.
- **Control Center:** click its icon in the menu bar. Wi-Fi, Bluetooth, sound output and input with levels,
  brightness, Dark Mode, Do Not Disturb, power mode.
- **Notification Center:** click the clock or press Super+N. Banners are grouped by app, plus Up Next (your calendar
  events), the month calendar, the weather (if you turn it on) and battery levels.
- **Dock:** pinned apps and running apps, with dots for open apps, badges, magnification under the pointer and
  right-click menus. By default it hides when a window of the app you're using overlaps it (Plank's "intelligent"
  mode); touch the bottom edge to bring it back, or pick another mode with `[dock] hide`. On first start your Plank
  dock is imported; `noobsbot-desktop dock --import-plank` does it again later.
- **Spotlight (Super+Space):** apps and a calculator. Arrow keys select, Return opens, Esc closes.
- **Launchpad:** every app in a grid, with search. Open it from the dock or with a four-finger pinch.
- **App drawer (Super+A, or tap Super alone):** apps by category, like a classic start menu, with search, Favorites and
  Recent; right-click an app to favorite it or add it to the dock or desktop. Lock, Log Out and Shut Down sit in its
  left strip. Turn the Super tap off with `[app_drawer] super_tap = false`; add `"app-drawer"` to `[dock] items` for a
  dock icon. Favorites and Recent live in `~/.local/state/noobsbot-desktop/favorites` and `recent-apps`, one desktop
  file id per line.
- **Spaces and Mission Control:** Super+Left/Right or a three-finger swipe switches Space. Super+Up (or swipe up)
  shows every window and the Spaces bar, where + adds a Space. Super+Down (or swipe down) shows the current app's
  windows.
- **App switcher:** hold Super and press Tab.
- **Screenshots:** Super+Shift+3 (whole screen), Super+Shift+4 (drag a region), Super+Shift+5 (toolbar with options).
  They are saved to `~/Pictures/Screenshots` and copied to the clipboard.
- **Lock screen:** Super+Ctrl+Q, or automatically when the screen turns off and always before sleep.
- **Emoji:** Super+Ctrl+E or Super+period opens the picker; the emoji is typed into the focused text field, or copied.
- **Quake terminal:** F12 drops a terminal down from the top of the screen.
- **Desktop icons:** files in `~/Desktop` appear on the wallpaper. Drag to arrange, double-click to open,
  right-click for Open, Rename, Move to Trash and New Folder.
- **Calendar events:** put `.ics` files in `~/.local/share/noobsbot-desktop/calendars/`. They show in Up Next, as dots
  in the calendar, and as reminders.
- **USB drives:** appear in a notification with Open and Eject.
- **Typing other languages:** Super+Ctrl+Space switches keyboard layout. With fcitx5 installed, its input methods
  (Nepali, Hindi, Chinese, Japanese, Korean…) appear in the input menu too.
- **Force Quit:** Super+Alt+Esc.

## Keyboard shortcuts
Super is the key with the Windows or Command logo. All shortcuts can be changed in the `[bindings]` section of the
settings file.

| Shortcut | Action |
|---|---|
| Super+Return | terminal |
| Super+Space | Spotlight |
| Super+A, or Super tapped alone | app drawer |
| Super+B / Super+E | Helium browser / Dolphin file manager |
| Super+Q / Super+W | quit app / close window |
| Super+M / Super+H / Super+Alt+H | minimise / hide app / hide others |
| Super+Tab (Shift+Tab back) | app switcher |
| Super+N | Notification Center |
| Super+Up / Super+Down | Mission Control / App Exposé |
| Super+Left / Super+Right | previous / next Space |
| Ctrl+1 … Ctrl+9 | go to Space 1–9 |
| Super+1 … Super+9 | focus, cycle or open the n-th dock app (hold Super to see the numbers) |
| Super+Shift+1 … 9 | a new window of the n-th dock app (not 3, 4, 5: screenshots) |
| Super+Shift+Left / Right | move the window to the previous / next Space |
| Super+Shift+3 / 4 / 5 | screenshot: screen / region / toolbar |
| Super+Ctrl+Q | lock screen |
| Super+Alt+Esc | Force Quit |
| Super+Ctrl+Space | next keyboard layout |
| Super+Ctrl+E, Super+period | emoji picker |
| F12 | quake terminal |
| Volume, mute, mic, brightness and media keys | as labelled, with an on-screen display |
| Power button | asks before shutting down |
| Ctrl+Alt+F1 … F12 | switch to a text console and back |

## Touchpad gestures
| Gesture | Action |
|---|---|
| tap / two-finger tap | click / right-click |
| two-finger scroll | scroll (natural direction by default) |
| two-finger pinch | zoom inside the app |
| three-finger swipe left / right | switch Space |
| three-finger swipe up / down | Mission Control / App Exposé |
| four-finger pinch in | Launchpad |
| four-finger spread | Show Desktop |

Gestures follow your fingers and finish with a spring, so a quick flick is enough and letting go halfway cancels.
Set `swipe_fingers = 4` in `[touchpad]` to use four fingers for Space and Mission Control swipes and leave
three-finger swipes to apps.

## Settings
Open **System Settings** from Launchpad or Spotlight (search "Settings"), or edit `~/.config/noobsbot-desktop/config.toml`. The file is
fully commented, every line shows the default, and saving it applies the change straight away. A line the desktop
doesn't understand is skipped and reported in the journal.

| Section | What it controls |
|---|---|
| `[appearance]` | light / dark / auto, accent colour, glass opacity, wallpapers, theming Qt apps to match, reduce motion |
| `[keyboard]` | layouts, variants, xkb options, Mac keys mode |
| `[touchpad]` | tap to click, natural scrolling, three or four-finger swipes, gesture sensitivity |
| `[hot_corners]` | an action for each screen corner (off by default) |
| `[display]` | which GPU draws the desktop, scale for every screen |
| `[[display.outputs]]` | one screen by connector name: on or off, resolution, refresh rate, scale, rotation, position, which shows the menu bar and Dock (System Settings › Displays asks to keep or revert) |
| `[windows]` | move key, magnetic edges, snapping, double-click title bar, number of Spaces |
| `[clock]` | time zone, 24-hour clock, weekday, date, seconds, first day of the week |
| `[dock]` | pinned apps, icon size, magnification, auto-hide, click, middle-click and scroll actions |
| `[idle]` | when to dim, turn the screen off and sleep (battery and AC), locking |
| `[privacy]` | which programs may record the screen |
| `[weather]` | the weather widget (off by default), city or coordinates, units |
| `[calendar]` | which `.ics` files to show |
| `[update]` | stable or beta updates |
| `[desktop]` | desktop icons on or off |
| `[bindings]` | keyboard shortcuts |
| `[[rules]]` | window rules: open on a Space, size, position, always on top, no title bar, opacity |

Example: dark mode, a purple accent, and a hot corner for Mission Control:
```toml
[appearance]
mode = "dark"
accent = "purple"

[hot_corners]
top_left = "mission-control"
```

## Updates
Once a day the desktop checks GitHub for a newer release (it sends nothing but the request itself; turn it off with
`check_updates = false`). When there is one:
1. It downloads in the background and is checked against the release signature. A download that fails the check
   is deleted and you're told.
2. A notification and the logo menu offer **Restart to Update**. It asks for your password, installs the new version
   next to the old one and switches over in one step, then asks you to log out.
3. If the new version fails to start three times in a row, the next login rolls back to the previous version
   automatically and tells you.

Set `channel = "beta"` in `[update]` to also get pre-releases. Installs made with `--from-source` or by hand only
get the notification. From a terminal in the desktop, `noobsbot-desktop --update` does the same check, download and
install; you can also always update with `./install.sh --update`.

Versions follow [semantic versioning](https://semver.org); every release's changes are in
[`CHANGELOG.md`](CHANGELOG.md).

## Privacy and security
- **No telemetry.** The only network requests are the daily update check, the weather (only if you turn it on, for
  the one place you set; your location is never looked up from your IP address), and what you do yourself.
- **Screen recording asks first:** the first time a program wants to record the screen, an alert asks you, and the
  answer is saved in `[privacy]`. Sandboxed (Flatpak) apps never get screen recording. Screen sharing goes through
  the standard portal.
- **Password prompts** (for example `pkexec`, or installing an update) use Noobsbot Desktop's own alert. The
  standard Qt agent is only a fallback.
- **Lock screen:** if the desktop ever crashes, you return to the login screen; it is never left unlocked.
- **Updates are signed:** releases are signed in CI with a key whose private half never leaves the release secret.
  The installer and the updater refuse anything that doesn't match, and refuse to downgrade.
- Privileged Wayland interfaces (screen capture, input methods, window lists) are offered only to trusted clients;
  start a tool with `noobsbot-desktop --trusted <command>` to trust it.

## Apps that come with it
| Role | App |
|---|---|
| Terminal | [noobsbot-terminal](https://github.com/noobsbot-team/noobsbot-terminal) |
| Browser | [Helium](https://helium.computer) |
| Files | Dolphin (with archive support via Ark) |
| Text editor | Kate, and nano in the terminal |
| Disks | Partition Manager and Filelight |
| Resources | System Monitor and btop, plus the desktop's Force Quit window |
| Settings | Noobsbot Desktop's own System Settings |

Qt apps such as Dolphin and Kate get the matching "Noobsbot Glass" theme (Kvantum and a Qt colour scheme), made
from the desktop's own colours by `noobsbot-desktop theme --write` (add `--gsettings` for GTK apps). Set
`theme_apps = true` in `[appearance]` to have the Dark Mode toggle rewrite it, so apps follow light and dark mode.

## Troubleshooting
- **Logs:** the current session's log is `~/.local/state/noobsbot-desktop/session.log` (the previous one is
  `session.log.old`). Also `journalctl --user -t noobsbot-desktop`.
- **Black screen or no desktop after login:** switch to a text console with Ctrl+Alt+F2, log in, and read the log
  above. Running `./install.sh` again repairs missing files.
- **A setting has no effect:** check the journal for "skipped" lines; a typo makes the desktop ignore just that line.
- **Wrong GPU on a laptop with two GPUs:** set `gpu = "/dev/dri/cardN"` in `[display]`, then log in again.
- **A bad update:** it rolls back by itself after three failed starts. You can also run `./install.sh` to put the
  latest good release back.
- **The graphical login screen won't start:** switch back to the text login by setting `command = "tuigreet …"` in
  `/etc/greetd/config.toml` (the original line is kept beside it in the file), or run `./install.sh` without
  `--greeter`.
- **Check the version:** `noobsbot-desktop --version`.

## Uninstalling
There is no uninstaller yet. To remove it by hand:
1. Point greetd at another session or desktop in `/etc/greetd/config.toml`, or disable greetd.
2. Remove `/usr/lib/noobsbot-desktop`, the `/usr/bin/noobsbot-desktop*` links and
   `/usr/share/wayland-sessions/noobsbot-desktop.desktop`.
3. Optionally remove your settings in `~/.config/noobsbot-desktop` and state in `~/.local/state/noobsbot-desktop`.

The packages it installed are ordinary Arch or Debian packages; remove the ones you don't want with pacman or apt.
On Debian, also remove `/usr/local/bin/xwayland-satellite`, `/usr/local/bin/helium-browser` and
`/usr/local/share/applications/helium.desktop`.

## Building and developing
You need Rust (edition 2024) and the system libraries listed in `.github/workflows/ci.yml`.
```sh
cargo build --release                      # the desktop and System Settings
cargo run -- --nested                      # run the desktop in a window inside your current desktop
cargo run -- --nested -- noobsbot          # …and start an app inside it
cargo run -- --nested --show spotlight     # open one popover for a screenshot or design check
cargo test --workspace                     # unit tests
cargo clippy --workspace --all-targets
cargo fmt --all --check
```
- **Nested mode** is safe: it never touches your real screen, settings or system. Inside it, Alt stands in for Super
  so your host desktop keeps its own shortcuts.
- `--show <name>` opens a single piece of the shell (for example `control`, `notification-center`, `lock`,
  `launchpad`, `mission-control`, `emoji`, `greeter`); `--help` lists them all.
- `scripts/test-install.sh` and `scripts/test-session.sh` test the installer and session wrapper in a scratch
  directory. `scripts/vm/` boots a throwaway Arch VM in QEMU (no root needed) to test the real install, real-screen
  session, login screen, password prompts and updates: `sh scripts/vm/setup.sh`, then `scripts/vm/boot.sh`.
- Commits follow [Conventional Commits](https://www.conventionalcommits.org) (`feat:`, `fix:`, …), which
  [release-plz](https://release-plz.dev) uses to pick the next version and write the changelog. Merging its release
  pull request tags the version and builds the signed release.

## Known limits
- Most Linux apps don't publish their menus, so the menu bar's app menu has only About, Hide and Quit.
- Apps that draw their own title bars (GTK4/libadwaita) don't show our traffic lights.
- No Apple fonts, icons, symbols or wallpapers (their licences forbid it); open look-alikes are used instead.
- No two-finger edge swipe for Notification Center (the touchpad doesn't report finger position); click the clock.
- Screen sharing is per screen, not per window.
- No hibernate (sleep only), no VRR, HDR or 10-bit colour yet.
- English interface only (dates and numbers follow your locale); no fast user switching.
- X11 apps look soft on HiDPI screens.

The full list, with reasons, is in Part L of [`plan.md`](plan.md).

## Project layout
| Path | What's there |
|---|---|
| `src/` | the desktop: compositor (`backend/`, `protocols.rs`, `input.rs`), shell (`shell/`), drawing (`render/`), system services (`services/`) |
| `settings/` | the System Settings app |
| `data/` | default settings (`config.toml`) and the system files the installer puts in place (`data/system/`) |
| `assets/` | the logo and the Phosphor symbols |
| `scripts/` | test scripts, the VM test kit, release-signing setup |
| `install.sh` | the installer |
| `plan.md` | the full design and the reasoning behind it |
| `PERF.md` | performance measurements |

## License
[MIT](LICENSE). The WhiteSur icons, cursors and themes the installer adds come from their own projects, under
their own licences.

# Noobsbot Desktop — Design Plan (v1 "Opal")

A complete desktop environment for Linux with a glassy theme, a global menu bar and a dock, written in Rust, for a fresh
Arch Linux system.

> Status: **planning only — no code until the owner says "start"**.
> **Name: Noobsbot Desktop** — made and maintained by the **Noobsbot Team**, the sister product of
> Noobsbot Terminal. Technical name **`noobsbot-desktop`** (program, package, config folder, session file).
> **Release codename for version 1: "Opal"** (shown as "Noobsbot Desktop 1 · Opal" in About). An opal is a gem
> that looks like glass with soft light inside but isn't see-through, like this design: glassy, yet mostly solid.
> Later major releases get other gem codenames (e.g. Pearl, Jade, Onyx).
> Code will live in `~/Projects/Linux Tools/custom-desktop-environemt/` (folder name kept as is).
> **Repository:** `github.com/noobsbot-team/noobsbot-desktop`, next to `noobsbot-team/noobsbot-terminal`. Both
> started on the owner's account (`anil0403/…`) and moved to the team's organisation; GitHub redirects the old URLs.
> **License:** the code is **MIT** (short, permissive, the same licence Smithay uses; anyone may use, change and share it). The derived
> "Noobsbot Glass" Kvantum/GTK themes live in `themes/` under **GPL-3.0** (WhiteSur's licence). Decided here so the
> repo carries `LICENSE` (MIT) from the start, and `themes/LICENSE` (GPL-3.0) once the themes exist.

---

## Contents
- **Part A** — What we are building (goal, requirements, non-goals, layers)
- **Part B** — Glossary
- **Part C** — Core technology choices (what, why, what we rejected)
- **Part D** — Design system: the glassy look shared by every element
- **Part E** — Milestones M1–M8: every element in detail (what, design, built with, why, done when)
- **Part F** — Apps that ship with Noobsbot Desktop
- **Part G** — System layer: hardware, drivers, services, package list
- **Part H** — Fonts, icons, themes
- **Part I** — Performance budget
- **Part J** — Code layout and Rust libraries
- **Part K** — Testing and design review
- **Part L** — Known limits, risks, open questions

---

# Part A — What we are building

## A1. The goal
Noobsbot Desktop is a whole desktop: everything you see after logging in. That means windows, menu bar, dock,
Spotlight, Launchpad, Notification Center, Control Center, calendar, Spaces, Mission Control, touchpad gestures,
lock screen and System Settings. It has a **global menu bar, a dock and a glassy theme**, and **behaves the same way everywhere**, is installed on a
**fresh Arch Linux** system by one script, does not rely on any other desktop,
and must be **very fast** while using **as little RAM and CPU as possible**.

## A2. Requirements (set by the owner)
| # | Requirement | Where |
|---|---|---|
| R1 | A consistent glassy look and feel, **well designed** everywhere | Part D + the "Design" block of every element in Part E |
| R2 | Glassy theme, **no blur**, low transparency (mostly solid) | D3 materials |
| R3 | Super fast, minimal system resources | Part C, Part I |
| R4 | Rust (or a better language if one exists) | C1 |
| R5 | Default terminal: **noobsbot-terminal** (owner's own) | F1 |
| R6 | Default browser: **Helium** | F2 |
| R7 | File manager: **Dolphin** | F3 |
| R8 | Text editor, icons, fonts | F4, Part H |
| R9 | Full hardware support: drivers, Wi-Fi, Bluetooth, sound, display, keyboard, USB | Part G + each milestone's "Hardware covered" |
| R10 | Calendar, date & time | M2 · E2.4, E2.5 |
| R11 | Multiple workspaces | M5 · E5.1 |
| R12 | Touchpad gestures | M1 · E1.7 (basics) + M5 · E5.3 (system gestures) |
| R13 | Notification panel | M4 · E4.1, E4.2 |
| R14 | Fresh system — nothing assumed from any existing install | G4, `install.sh` |
| R15 | Detailed reasoning for every chosen element, inside each milestone | Part E format |
| R16 | Dock identical to the current Plank dock (function, design, behaviour) | M3 · E3.1 |
| R17 | Disk management | F5 |
| R18 | Resource monitor (+ Force Quit) | F6, M4 · E4.7 |

## A3. Non-goals
- **Not a Linux distribution.** We use Arch's packages and kernel.
- **No Apple assets.** No SF fonts, no Apple icons or wallpapers (license forbids). Open look-alikes (Part H).
- **No custom drivers.** Drivers come from the Linux kernel (G1 explains why that's enough).
- **No blur** (owner's decision), which is also faster.
- **Not in v1** (listed as limits L7–L14 so nobody expects them): HDR / 10-bit colour, hibernate, desktop icons,
  fast user switching, translated UI strings, single-window screen sharing, sharp X11 apps on HiDPI.

## A4. How the desktop is put together
```
 ┌──────────────────────────────────────────────────────────────────────┐
 │  APPS: noobsbot-terminal · Helium · Dolphin · Kate · Settings · …      │  separate programs
 ├──────────────────────────────────────────────────────────────────────┤
 │  noobsbot-desktop  (ONE program — this is what we write)              │
 │   ├ compositor + window manager (draws everything, owns the screen)   │
 │   ├ shell: menu bar · dock · Spotlight · Launchpad · Notification     │
 │   │        Center · Control Center · app switcher · Mission Control · │
 │   │        Spaces · gestures · lock screen · OSD · alerts             │
 │   └ system glue: talks to the services below over D-Bus              │
 ├──────────────────────────────────────────────────────────────────────┤
 │  SERVICES: PipeWire · NetworkManager · BlueZ · UPower · udisks2 ·     │  standard Linux
 │            logind · timedated · portals                               │  background services
 ├──────────────────────────────────────────────────────────────────────┤
 │  KERNEL + DRIVERS + FIRMWARE (linux, linux-firmware, mesa)            │  from Arch
 ├──────────────────────────────────────────────────────────────────────┤
 │  HARDWARE: GPU · screen · Wi-Fi · Bluetooth · audio · USB · touchpad   │
 └──────────────────────────────────────────────────────────────────────┘
```
**We write:** Noobsbot Desktop, `install.sh`, the update helper + release pipeline (E6.13), and later the System Settings app. **We reuse:** everything else
(proven, maintained, used by every Linux desktop).

---

# Part B — Glossary
| Term | Meaning |
|---|---|
| **Desktop environment (DE)** | All the programs that make up what you see and use after login. |
| **Compositor** | The program that combines every app's window into the final screen image. On Wayland it is also the window manager and display server. |
| **Window manager** | Decides where windows go, which has focus, and how they move or resize. |
| **X11 / Wayland** | The two ways Linux apps get onto the screen. X11 is from 1987; Wayland is the modern replacement. |
| **Wayland protocol** | A standard "language" between apps and the compositor. |
| **Shell** | The desktop's own UI: menu bar, dock, panels. |
| **D-Bus** | The standard message system Linux programs use to talk to each other. |
| **Service / daemon** | A background program with no window (e.g. NetworkManager runs Wi-Fi). |
| **Kernel driver / firmware** | Code in Linux that controls a device / a small program loaded into the device itself. |
| **GPU / shader** | The graphics chip / a tiny program running on it for each pixel. |
| **SDF (signed distance field)** | Drawing shapes with math instead of images, so they stay sharp at any size. Used for corners, shadows and traffic lights. |
| **Damage tracking** | Redrawing only what changed. The key to 0% CPU when idle. |
| **Direct scanout** | Showing a fullscreen app without compositing it. Saves power. |
| **Spring animation** | Motion that behaves like a physical spring, the way Apple animates. |
| **libinput** | The standard Linux library that reads touchpads, mice and keyboards and recognises gestures. |
| **RSS** | The RAM a program really uses. |
| **Portal** | A standard service apps use for file pickers, screen sharing and screenshots. |
| **AUR / crate** | Arch community packages / Rust libraries. |
| **pt / px** | Sizes in this plan are logical pixels at 100% scale (doubled on a 200% HiDPI screen). |

---

# Part C — Core technology choices
Each choice lists **what it decides**, **what we use**, **why**, and **what we rejected**.

## C1. Language → **Rust** (edition 2024)
- **Why:** (1) C/C++-level speed, native code. (2) **No garbage collector**, so there are no stutters and RAM
  stays low. (3) **Memory safety** at compile time, which matters because a compositor crash kills the whole
  desktop and every app. (4) The best Linux-desktop ecosystem today (Smithay, cosmic-text, zbus, calloop),
  proven in production by other Rust desktops and compositors. (5) The owner already writes Rust (noobsbot).
- **Rejected:** **C** (memory bugs crash the session, more code); **C++** (complex, the same risk); **Zig**
  (immature, no Wayland ecosystem); **Go/C#/Java** (garbage-collector pauses, more RAM);
  **Python/JS** (far too slow to draw 60–120 frames per second).

## C2. Display system → **Wayland**
- **Why:** (1) One program does everything: on X11 we'd need an X server + window manager + a separate
  compositor (picom) for corners, shadows and transparency. That's more RAM, more latency, and tearing.
  (2) Tear-free by design. (3) Secure: apps can't spy on each other's windows or keystrokes. (4) The future:
  the major desktops have moved; X11 is in maintenance. (5) Proper HiDPI and mixed scaling.
- Old X11-only apps still run through XWayland (C10).
- **Rejected:** X11 (needs picom, tears, insecure, being phased out).

## C3. Compositor library → **Smithay** (pure Rust, pinned version)
- **What:** a toolkit that handles Wayland protocol plumbing, input, monitors, sessions and a GPU renderer.
- **Why:** pure Rust (fits C1); powers other Rust compositors in production; leaves every behaviour decision to us;
  has a **nested mode** (runs as a window), so development is safe.
- **Rejected:** **wlroots** (C, needs unsafe bindings); **forking an existing compositor** (huge code
  that isn't ours, built for tiling and separate-process shells); **from zero** (months of plumbing).

## C4. Drawing → **OpenGL ES 3 via Smithay `GlesRenderer` + our GLSL shaders**
- **Why:** runs on all Mesa GPUs (Intel, AMD) and NVIDIA; compositing is simple 2D work; our shaders draw
  glass, rounded corners and shadows in **one pass per surface**; Smithay's most mature renderer.
- **Rejected:** **Vulkan** (overkill for 2D, less mature in Smithay); **CPU drawing** (slow at 4K/120 Hz,
  uses more battery; kept only as an emergency fallback); **wgpu** (doesn't fit Smithay's GLES pipeline).

## C5. Shell architecture → **one process: the compositor draws the shell itself**
- **Why:** (1) **RAM:** each separate GUI program costs about 20–40 MB (its own GPU context, fonts, toolkit).
  Eight shell programs would be 160–320 MB; one process stays under 100 MB. (2) **Perfect sync:** dock
  magnification, gestures and Mission Control move in the same frame as the windows. (3) **Simpler:** the shell
  sees every window directly, with no extra protocols. (4) A single display server that owns the dock is a proven design.
- **Separate on purpose:** real apps and the future Settings app. They're big, and their bugs mustn't kill the desktop.
- **Downside & mitigation:** a shell crash ends the session. Mitigated with Rust safety, no `unwrap()` in shell
  code (enforced by Clippy lints `unwrap_used` / `expect_used` / `panic` in CI, not only by rule), and unit tests.
- **Rejected:** separate shell programs (the waybar/wofi/mako style): several times the RAM and a less coherent feel.

## C6. Shell widgets → **our own small widget set**
- **What:** about 15 building blocks (glass panel, label, icon, button, toggle, slider, list row, menu, popover,
  text field, alert, tooltip, badge, progress bar, segmented control), all drawn from Part D's design tokens.
- **Why:** a full toolkit (GTK, Qt, iced) can't draw inside the compositor's scene, would bring far more code and
  RAM, and would fight our look. Owning the widgets guarantees **every element looks identical**.
- **Rejected:** GTK/Qt (heavy, separate process); iced/slint inside the compositor (a duplicate renderer).

## C7. Event loop → **calloop**
- **Why:** Smithay's native loop; single-threaded and simple; **sleeps completely** when nothing happens (0% CPU).
- **Rejected:** tokio/async (extra threads and RAM; built for servers).

## C8. Text → **cosmic-text**
- **Why:** pure Rust; handles complex scripts (Devanagari, Arabic), font fallback and emoji; **already used in
  noobsbot**. Glyphs are cached in a GPU texture (glyph atlas), so text costs almost nothing per frame.
- **Rejected:** FreeType + HarfBuzz directly (C, more glue); fontdue (no complex-script shaping).

## C9. System services → **zbus** (D-Bus)
- **Why:** one pure-Rust library talks to every service: battery, Wi-Fi, Bluetooth, brightness, power, time zone,
  media players, tray, notifications. It's signal-based: services **tell us** about changes, so we never poll.
- **Rejected:** shelling out to `nmcli` / `bluetoothctl` (slow, fragile, needs polling). The one temporary
  exception is volume via `wpctl` in M2.

## C10. Old X11 apps → **xwayland-satellite**
- **Why:** most apps are native Wayland now. For the rest, a small separate helper is far less code than an X11
  window manager inside Noobsbot Desktop (other Rust compositors do the same).
- **Rejected:** Smithay's built-in X11 WM support (thousands of extra lines in the compositor).

## C11. Settings storage → **one TOML file, live-reloaded**
- **What:** `~/.config/noobsbot-desktop/config.toml`, read with `serde` + `toml` and watched with `notify`.
  The file starts with `version = 1`; every writer (the compositor when you toggle dark mode, the Settings app) writes
  **atomically** (temp file → rename), so two writers never corrupt it, and older versions are migrated on load.
- **Why:** readable, easy to back up; **save = applied instantly**; it doubles as the API for the Settings app
  (the app just edits the file); the same pattern as noobsbot (`custom-terminal/src/config.rs`).
- **Rejected:** a dconf-style database (hard to read); hard-coded values (recompile to change).

## C12. Logging → **tracing**
- Writes to the systemd journal (`journalctl --user -t noobsbot-desktop`). Smithay already uses it.

---

# Part D — Design system (shared by every element)

Every element in Part E is built **only** from these tokens and components, so the whole desktop looks like
one product. All values live in `src/theme.rs` and can be overridden in `config.toml`.

## D1. Design principles
1. **Clarity first.** Text is always readable. Glass is mostly solid (R2), never at the cost of contrast.
2. **Deference.** The shell is calm and neutral; apps and wallpaper are the colour.
3. **Depth through light, not blur.** Layers are told apart by tint, a highlight edge, a hairline and a shadow.
4. **Consistent geometry.** One spacing grid, one corner-radius scale, one type scale.
5. **Physical motion.** Everything moves on springs, follows your fingers, and can be interrupted.
6. **Accessible by default.** Keyboard reachable, visible focus, Reduce transparency, Reduce motion, contrast checked.

## D2. Colour
### System colours
| Token | Light | Dark | Used for |
|---|---|---|---|
| `label` | `#000000` @ 85% | `#FFFFFF` @ 85% | primary text |
| `label-secondary` | `#000000` @ 50% | `#FFFFFF` @ 55% | subtitles, times |
| `label-tertiary` | `#000000` @ 26% | `#FFFFFF` @ 25% | placeholders, disabled |
| `separator` | `#000000` @ 10% | `#FFFFFF` @ 10% | divider lines |
| `fill` | `#000000` @ 5% | `#FFFFFF` @ 8% | control backgrounds (slider track, field) |
| `window-bg` | `#ECECEC` | `#1E1E1E` | Settings app window, alerts |
| `accent` (default blue) | `#007AFF` | `#0A84FF` | selection, toggles on, focus ring, default button |

### Accent choices (eight accent colours)
| Blue | Purple | Pink | Red | Orange | Yellow | Green | Graphite |
|---|---|---|---|---|---|---|---|
| `#007AFF`/`#0A84FF` | `#AF52DE`/`#BF5AF2` | `#FF2D55`/`#FF375F` | `#FF3B30`/`#FF453A` | `#FF9500`/`#FF9F0A` | `#FFCC00`/`#FFD60A` | `#34C759`/`#30D158` | `#8E8E93`/`#98989D` |

### Traffic lights
| | Close | Minimise | Zoom/Fullscreen | Unfocused window |
|---|---|---|---|---|
| Fill | `#FF5F57` | `#FEBC2E` | `#28C840` | `#D0D0D0` (light) / `#4A4A4A` (dark) |
| Edge | 0.5 px darker 15% | same | same | none |
| Glyph on hover | × | – | ⤢ (Alt held: +) | — |

### Contrast rule
All text vs. its glass background: **≥ 4.5:1** (WCAG AA), checked at the worst case (brightest/darkest
wallpaper behind the glass). That's why glass is 85%+ opaque.

## D3. Glass materials (no blur)
Every glass surface = **tint** + **sheen gradient** + **inner highlight** + **outer hairline** + **shadow**,
drawn in one shader pass. Opacity is per material, multiplied by the global `glass_opacity` setting
(1.0 = fully solid = "Reduce transparency").

| Material | Used by | Light tint | Dark tint | Opacity L / D |
|---|---|---|---|---|
| `bar` | menu bar | `#F6F6F6` | `#242424` | 85% / 82% |
| `panel` | Control Center, Notification Center, Spotlight, Launchpad search (and the dock's optional `glass` preset) | `#F2F2F2` | `#262626` | 88% / 85% |
| `popover` | menus, calendar, tooltips, context menus | `#FAFAFA` | `#2C2C2C` | 92% / 90% |
| `card` | notification banners, Control Center modules, widgets | `#FFFFFF` | `#323232` | 90% / 88% |
| `hud` | OSD (volume/brightness), app switcher, Space indicator | `#EDEDED` | `#1C1C1C` | 88% / 85% |
| `sheet` | alerts, lock-screen password field | `#FFFFFF` | `#2A2A2A` | 96% / 95% |

| Layer | Light | Dark |
|---|---|---|
| Sheen | top 4% lighter → bottom 0% | top 3% lighter → bottom 0% |
| Inner highlight | 1 px, white 25% (top edge 40%) | 1 px, white 12% (top edge 18%) |
| Outer hairline | 0.5 px, black 10% | 0.5 px, black 50% |
| Backdrop | **no blur** (owner decision) | — |

## D4. Typography (Inter; JetBrains Mono for code)
| Style | Size / line height | Weight | Used for |
|---|---|---|---|
| Display | 96 / 100 | Light (300) | lock-screen clock |
| Large Title | 26 / 32 | Regular | Settings page titles |
| Title 1 | 22 / 26 | Regular | Spotlight input text |
| Title 2 | 17 / 22 | Semibold | Notification Center date, alert titles |
| Title 3 | 15 / 20 | Semibold | widget titles |
| Headline | 13 / 16 | Semibold | notification titles, app name in menu bar |
| Body | 13 / 16 | Regular | default text, menus, list rows |
| Callout | 12 / 15 | Regular | secondary rows, tooltips |
| Subheadline | 11 / 14 | Regular | section headers (uppercase off, `label-secondary`) |
| Caption | 10 / 13 | Medium | badges, calendar week numbers, dock labels |
- Inter features on: tabular numbers in clocks and battery % (digits don't jump), `cv11` (single-storey a) off.
- Truncation: tail ellipsis "…" everywhere; never wrap menu items.

## D5. Spacing, size & shape
- **Grid:** 4 px. Allowed gaps: 2, 4, 6, 8, 12, 16, 20, 24, 32, 48.
- **Hit targets:** at least 24 × 24 px (touchpad pointer), and 28 px for Control Center buttons.
- **Corner radii:**
  | Element | Radius |
  |---|---|
  | window | 10 |
  | menu / popover / tooltip | 8 / 8 / 6 |
  | button / text field | 6 |
  | card, Control Center module, widget | 12 |
  | notification banner | 16 |
  | Control Center panel, Notification Center column | 16 |
  | Spotlight | 14 |
  | dock | 0, square (Plank Transparent-Tight look, E3.1); 18 in the optional `glass` preset |
  | toggle / slider / password field | fully round (pill) |
  | app icon squircle | defined by the icon theme |
- **Corner shape:** continuous "squircle" corners (smoother than circular arcs, like Apple), done in the SDF shader.

## D6. Elevation (shadows)
| Level | Used by | Offset Y | Blur | Opacity L / D |
|---|---|---|---|---|
| 0 | flat items inside panels | — | — | — |
| 1 | menu bar | 1 | 4 | 10% / 30% |
| 2 | cards, banners, tooltips | 4 | 12 | 15% / 40% |
| 3 | menus, popovers, panels | 8 | 24 | 20% / 50% |
| 4 | unfocused window | 8 | 30 | 22% / 50% |
| 5 | focused window, alerts, Spotlight | 22 | 60 | 35% / 60% |

## D7. Interaction states (every control)
| State | Look |
|---|---|
| Normal | per material |
| Hover | overlay `label` @ 6% (light) / white @ 8% (dark) |
| Pressed | overlay `label` @ 12% / white @ 14%; scale 0.97 (buttons) |
| Selected | `accent` fill, white text (menus, lists); `accent` @ 20% (sidebar rows) |
| Focus (keyboard) | 3 px `accent` @ 50% ring outside the shape, following its radius |
| Disabled | 40% opacity, no hover |
| Toggle on / off | track `accent` / `fill` @ 2× ; white knob with level-2 shadow |

## D8. Standard components (built once in `src/ui/`, used everywhere)
| Component | Spec |
|---|---|
| **Menu** (dropdown / context) | `popover` material, radius 8, padding 5; rows 22 px high, 12 px side padding; hover = accent fill + white text (radius 4); shortcut glyphs right-aligned in `label-secondary`; separators 1 px `separator` with 5 px margin; submenu chevron › |
| **Tooltip** | `popover`, radius 6, Callout text, 6×8 padding, shows after 600 ms hover, fades 120 ms |
| **Button** | 22 px (regular) / 28 px (large); radius 6; default = accent fill + white text; secondary = `fill` |
| **Toggle (switch)** | 26 × 15 pill, knob 13 px |
| **Round toggle** (Control Center) | 28 px circle; on = accent fill + white glyph, off = `fill` + `label` glyph |
| **Slider** | 28 px tall pill (Control Center style), glyph inside at the left, fill = white (dark) / `label` (light) @ 90% |
| **Text field** | 22 px (regular) / 52 px (Spotlight); radius 6; `fill` background; focus ring D7 |
| **List row** | 28 px; icon 16–24 px; 8 px gap; selected = accent |
| **Alert** | `sheet`, 260 px wide, radius 12, app/system icon 64 px, Title 2 + Body centred, buttons full width stacked (or side by side when 2), default = accent, Esc = cancel |
| **Badge** | red `#FF3B30` circle, min 16 px, Caption white, top-right of an icon |
| **Progress bar** | 4 px tall pill, `fill` track, accent progress |
| **Segmented control** | 22 px, radius 6, selected segment = card material + level-2 shadow |

## D9. Iconography
| Kind | Choice | Size |
|---|---|---|
| App icons | **WhiteSur** icon theme (squircle style) | dock 84 (zoomed to 126, like the current Plank), Launchpad 80, switcher 96, Spotlight 24/32, notifications 32 |
| Symbols (menu bar, Control Center, OSD, menus) | **Phosphor Icons** (MIT, free; regular + fill weights, similar feel to Apple's SF Symbols) bundled as SVG | 16 (menu bar), 18–20 (Control Center), 24 (OSD) |
| Cursors | **WhiteSur cursors** | 24 (scales with display) |
- Menu-bar symbols are monochrome `label`; Control Center glyphs flip to white on accent when on.

## D10. Motion
| Spring preset | Response | Damping | Used for |
|---|---|---|---|
| `snappy` | 0.25 s | 0.90 | menus, popovers, toggles, hover |
| `smooth` | 0.40 s | 1.00 | windows, panels sliding, Spaces |
| `bouncy` | 0.35 s | 0.70 | small UI bounces (the dock instead uses Plank's exact timings, E3.1) |
| `gesture` | follows the finger 1:1, then a velocity-based spring | 0.85 | all touchpad gestures |
- Fades: 120 ms in / 150 ms out. Menus appear with an opacity fade + 0.98→1 scale.
- **Reduce motion** setting: every movement becomes a 150 ms crossfade.
- Animations stop completely when settled (0% CPU).

## D11. Wallpaper
- Ships with **our own** wallpapers: a soft opal-like colour-gradient set (milky white with hints of blue, pink and green; a dark variant) with light and dark variants (dark used
  automatically in dark mode). Third-party OS wallpapers are copyrighted.
- Any PNG/JPEG can be set (fill / fit / centre). Lock screen uses the same wallpaper darkened 30%.

## D12. Design workflow (how "well designed" is guaranteed)
For **every** milestone, before code:
1. **Mockup:** an HTML/visual mockup of each element in that milestone, built strictly from Part D tokens,
   light and dark, for the owner to review.
2. **Owner approval** of the mockups (changes go back into Part D or the element's spec).
3. After coding: a **screenshot review** comparing the real element with the approved mockup
   (sizes, colours, spacing, motion), plus a contrast check (D2 rule).

## D13. Responsive layout (one design, every screen size and scale)
The desktop must look right from a 1280 × 720 laptop panel at 100% to a 4K monitor at 200%, portrait monitors
included, with no element ever cut off or overflowing. Rules every element follows:

| Concern | Rule |
|---|---|
| **Units** | every size in this plan is in logical px; each output has its own scale (100–200%, fractional allowed, `fractional-scale`). Text and SVG icons are rasterised **at the output's exact scale** (never bitmap-scaled); hairlines and 1 px shadows are snapped to physical pixels so they never blur |
| **Layout engine** | `ui/layout.rs`: rows, columns and stacks with padding, gap, `grow`, `min`/`max`, like a small flexbox. Layout is computed **only when something changes** (size, content, scale) and cached; a frame never re-lays-out |
| **Minimum screen** | 1280 × 720 logical. Everything below is checked at 1280 × 720, 1366 × 768, 1920 × 1080, 2560 × 1440 @ 150%, 3840 × 2160 @ 200% and 1080 × 1920 (portrait) in the mockups and in the design review |
| **Menu bar** | fixed 24 px; right-side items keep their order and, when space runs out, drop left-to-right into a "»" overflow menu (tray icons first, clock last to go); app-menu titles truncate with "…" and never wrap |
| **Control Center** | fixed 320 px wide; if the screen is shorter than its height, the panel scrolls (never taller than screen − menu bar − 16) |
| **Notification Center** | fixed 360 px wide, full height, scrolls; on screens narrower than 1366, it overlaps the content (it never pushes windows) |
| **Spotlight** | 680 px wide, or **90% of the screen width** when narrower; results list capped at 8 rows or at the space above the dock |
| **Launchpad** | grid computed from the screen: columns = ⌊(width − 2 × 48) / 112⌋ clamped to 5…9, rows = ⌊(height − 160) / 120⌋ clamped to 3…6 (7 × 5 at 1080p, 5 × 4 at 720p, 9 × 6 at 4K); icon 80 px, or 64 px under 1366 wide |
| **Dock** | exactly Plank's rule: when the pinned + running icons don't fit the screen width, the **icon size shrinks** (84 → down to 32) until they do; zoom still applies. One dock per monitor, each fitted to its own width |
| **Windows** | a new window never opens larger than the usable area (screen − menu bar − dock), and never with its title bar under the menu bar; remembered positions are clamped back on-screen when a monitor disappears or changes scale |
| **Mission Control** | the grid algorithm takes the real aspect ratio of the screen (portrait = more rows), thumbnails keep their aspect ratio, min 120 px wide, 32 px gaps |
| **Alerts, OSD, switcher** | fixed widths (260 / 280 / content), always centred on the monitor that has the pointer or the focused window |
| **Settings app** | min 600 × 400; sidebar collapses from 220 px (icon + label) to 56 px (icon only) when the window is narrower than 700 |
| **Larger text** | Accessibility setting: the whole type scale × 1.0…1.5; controls grow on the 4 px grid; nothing is clipped at 1.5 |
| **Lists** | virtualised: only visible rows are laid out and drawn (1000 notifications or 500 apps stay instant) |
| **Portrait monitors** | supported (rotation set through `wlr-output-management`); the menu bar stays on top, the dock at the bottom, and each has its own width |
| **Touch screens** | basic touch via libinput (tap, drag, 2-finger scroll, pinch to apps); the UI isn't touch-first in v1 (hit targets are 24–28 px, not 44) |
| **Overflow** | text: tail ellipsis; icons: never scaled down below 16 px; a layout that cannot fit logs a warning in debug builds (so it's caught in review, not by users) |

---

# Part E — Milestones in detail

**Every element** is described the same way:
- **What it is** – its job.
- **Design** – what it looks like (sizes from Part D, sketch) and how it behaves.
- **Built with** – technologies and libraries.
- **Why** – the reason for each choice and what we rejected.
- **Done when** – the test that proves it's finished.

Milestone process: **mini-plan → mockups (D12) → owner approves → code → clippy + tests green → `PERF.md` numbers
within Part I → design review → commit**.
Size: S = days, M = 1–2 weeks, L = several weeks (rough estimates).

---

## M1 — Compositor, windows & glass foundation (dev mode) — size L
Goal: Noobsbot Desktop runs as a window inside another desktop (safe development). Apps open inside it in
glass windows with traffic lights.

### E1.1 Compositor core
- **What it is:** the heart of Noobsbot Desktop. It receives every app's window, draws it, and sends keyboard and mouse to the right app.
- **Design:** invisible itself; responsible for smooth 60/120 Hz and no tearing.
- **Built with:** Smithay (Wayland protocols: `wl_compositor`, `wl_subcompositor`, `wl_shm`, `linux-dmabuf`,
  `wl_seat`, `wl_output`, `xdg_shell`, `xdg-decoration`, `viewporter`, `fractional-scale`,
  `presentation-time`, `xdg-activation`, `wl_data_device` + `primary-selection` for the clipboard and drag-and-drop,
  `xdg-foreign` + `xdg-dialog` (dialogs parented to the right window, e.g. the file picker over Helium),
  `relative-pointer` + `pointer-constraints` (games, Google Maps, VMs), `keyboard-shortcuts-inhibit` (VMs, remote
  desktop), `cursor-shape`, `wlr-layer-shell` (third-party bars and launchers keep working), `tearing-control` (games),
  `kde-server-decoration` (see E1.3), `text-input-v3` (see E6.12), `security-context-v1` (Flatpak, see E6.11),
  `linux-drm-syncobj-v1` (explicit sync: **required** on NVIDIA 555+ to avoid flicker and glitches), `linux-dmabuf`
  v4 feedback (tells apps which buffer formats our GPU can import, so no black windows on mixed GPUs));
  calloop; **winit backend** (runs as a window).
  - **Clipboard persistence:** Wayland drops the clipboard when the source app quits; we keep it. The compositor
    keeps a copy of the last clipboard contents (up to 10 MB, `clipboard.rs`) so copy → quit the app → paste works.
- **Why:** Smithay + calloop (C3, C7); the winit backend lets us develop without logging out or risking the
  real session. The protocol list is what modern apps (Helium, Qt, GTK, games, VMs, noobsbot) need to run fully.
- **Done when:** noobsbot-terminal and Helium run inside it with working keyboard, mouse, clipboard and
  copy/paste between them.
- **Done:** every protocol listed is served (`src/state.rs`, `src/protocols.rs`, `src/decorations.rs`; dmabuf v4
  feedback in `src/backend/mod.rs`; explicit sync in `src/sync.rs` where the render node supports it); the clipboard
  outlives its app (`src/clipboard.rs`).

### E1.2 Windows & window management
- **What it is:** where windows go and how they move, resize, focus, minimise and zoom.
- **Design:**
  - Windows **float**. New windows open centred, slightly above the middle (golden ratio), or at
    the position remembered for that app.
  - Click = focus + raise. The focused window gets elevation 5, unfocused gets elevation 4 and its title at
    `label-secondary`.
  - **Moving a window (drag):**
    | Detail | Behaviour |
    |---|---|
    | Grab | title bar (left button), or **Super + drag anywhere** in the window (Linux habit, configurable); 1-finger tap-drag and the optional 3-finger drag on the touchpad |
    | Start | the move begins after a **4 px** drag threshold, so a plain click never nudges the window |
    | Follow | the window follows the pointer **1:1 in the same frame** (E1.10). Only the window's texture moves; the app is not asked to redraw, so even a busy app drags smoothly |
    | Limits | the title bar can never go under the menu bar; at least 40 px of the title bar must stay on screen so it can always be grabbed again; otherwise a window may hang off any edge |
    | Magnetic edges | within **8 px** of a screen edge, the menu bar, the dock, or another window's edge, the window **sticks** gently (release inside the zone aligns it exactly); holding **Ctrl** disables sticking (Alt is taken by symmetric resize) |
    | Monitors | drag across monitors with different scales seamlessly (the texture is drawn on both at each one's scale); the menu bar of the monitor containing the window's centre becomes the active one on release |
    | Snapping | hold the window at a screen edge for 300 ms → a translucent `panel`-material preview of the half/quarter zone appears; release → the window springs into it (`smooth`); dragging it away restores its previous size |
    | Spaces | drag a window to the screen edge and keep pushing for 600 ms → the Space switches (optional, off by default); dragging onto a Space thumbnail in Mission Control moves it (E5.2) |
    | Release | no inertia; the window simply stops |
    | Mission Control / drag | starting a 3-finger swipe while dragging opens Mission Control with the window still in hand |
  - **Resizing a window:**
    | Detail | Behaviour |
    |---|---|
    | Grab zones | **8 px outside** every edge (invisible) plus the outer 4 px of the window frame; corners use a 16 × 16 zone and resize diagonally. The cursor changes to ↔ ↕ ⤡ ⤢ (`cursor-shape` protocol) as soon as the pointer enters a zone |
    | Live resize | each frame we send the app one `xdg_toplevel.configure` with the new size, **never more than one outstanding** (we wait for the app's ack + new buffer before sending the next). This is what keeps resizing smooth instead of flooding slow apps |
    | While the app lags | the frame (title bar, corners, shadow) is drawn at the **new** size immediately; the app's last buffer is anchored to the edge opposite the one being dragged (drag the right edge → content pinned to the left; drag the top-left corner → content pinned to the bottom-right) and the uncovered strip is filled with the window's background colour (`window-bg`). So nothing jumps |
    | Min / max | the app's `min_size` / `max_size` hints are respected; apps that give none get a floor of 200 × 120 so a window can never vanish |
    | Modifiers | **Alt + resize** = symmetric resize from the centre; **Shift + resize** = keep the aspect ratio |
    | Double-click an edge | the window grows to the screen edge in that direction only (the default, no modifier needed) |
    | Magnetic edges | the dragged edge sticks to screen and window edges within 8 px, same as moving; Ctrl disables it |
    | Fullscreen / zoomed | resize handles are disabled; dragging a zoomed window's title bar un-zooms it first |
    | Touchpad | click-drag on a zone; pinch does **not** resize windows (it belongs to apps, E1.7) |
    | Keyboard | ~~Super+Ctrl+arrows nudge/resize by 16 px steps (configurable, off by default)~~ Done: Super+Alt+arrows move the window 16 px, Super+Alt+Shift+arrows its right / bottom edge (Super+Ctrl+arrows went to tiling, E8.5); `nudge` / `resize` actions, active only with `[windows] keyboard_nudge` (off by default); a held key repeats; the move keeps the drag limits, the resize the app's min / max |
  - **Stacking order (bottom → top), fixed so no element can ever be hidden by the wrong one:** wallpaper → desktop
    icons (E7.2) → normal windows (per Space, most recent on top) → `float_above` windows (rules) → a window's
    modal/transient dialogs always directly above their parent → fullscreen window of the current Space → the quake terminal (E2.8, `layer = "overlay"` rule) → dock
    (hidden under fullscreen) → menu bar → shell popovers (menus, Control Center, Notification Center, Spotlight) →
    app popups with a grab → banners and OSD → alerts (Force Quit, power) → Mission Control / Launchpad overlay →
    screenshot overlay → **lock screen** (above everything, always).
  - **Focus stealing:** a new window takes focus only with a valid `xdg-activation` token (it was launched by the
    user's click/keystroke, or the focused app opened it). Otherwise it opens *behind* and its dock icon bounces
    so typing in one app never lands in another.
  - **Grabs:** opening a shell popover closes any app popup grab and vice versa; a lock, a Space switch or a monitor
    hot-unplug cancels every drag, resize and grab in progress.
  - Double-click the title bar = zoom (configurable: zoom / minimise / nothing).
  - **Zoom** = fill the screen between the menu bar and dock (keeping a 0 px gap; the window still has a title bar).
  - Window snapping (on by default, can be turned off): edge → half, corner → quarter; details
    in the Snapping row above.
- **Built with:** Smithay `Space` + our `wm.rs`.
- **Why floating:** floating windows are the design (R1). Remembered positions: the user's layout survives restarts.
  Snapping: users expect it, and it's cheap to add.
- **Done when:** move, resize, focus, raise, zoom, minimise and close all work with mouse and touchpad,
  without flicker; a window being resized by its left edge keeps its content pinned to the right while a
  deliberately slow test app (`sleep 200 ms` per frame) catches up; dragging follows the pointer with no visible lag
  at 120 Hz; magnetic edges and both modifiers work (unit tests for the anchor and sticking math).
- **Built:** `src/wm.rs` (placement, magnetic edges, resize maths, double-click edge grow, tiling), `src/grabs.rs`
  (move and resize grabs, Alt symmetric and Shift aspect resize, one configure outstanding), activation-gated focus
  (`src/state.rs`), remembered frames (`src/sheets.rs`), the snapping preview (shown on arrival, E8.5); the frame drawn
  at the asked size while a slow app catches up, its last buffer pinned inside and the bare strip `window-bg` (a title
  bar's glass already spans the frame) (`grabs::asked`); a 3-finger swipe up mid-drag opens Mission Control with the
  window still in hand, a drop on a Space moving it there (`carry_held`); a display unplugged or turned off ends a drag
  or resize as a button release would, never tiling (`end_drag`).

### E1.3 Title bars & traffic lights
- **What it is:** the bar on top of every window with close / minimise / zoom buttons.
- **Design:**
  ```
  ╭──────────────────────────────────────────────────────────╮
  │ ● ● ●                Window Title                         │  28 px, popover-style glass
  ├──────────────────────────────────────────────────────────┤
  │                  (app content)                            │
  ```
  - Height **28 px**; traffic lights **12 px** circles, **8 px** apart, centre 14 px from the left edge, vertically centred.
  - Colours and hover glyphs per D2. Hovering any one shows the glyphs on all three.
  - Title: Headline (13 semibold), centred, truncated with "…", `label` when focused / `label-secondary` when not.
  - Unfocused windows: traffic lights turn grey.
  - Window corners: radius 10 (top) and 10 (bottom), continuous squircle; 0.5 px hairline around the window.
- **Built with:** server-side decorations (we draw them) negotiated through `xdg-decoration` **and** the older
  `org_kde_kwin_server_decoration` (GTK3 and Qt 5 only understand this older one; without it every GTK3 dialog, including
  the portal file picker, draws its own title bar); SDF shader for the circles and glyphs.
- **Why:** server-side is the only way **every** app gets the same title bar. SDF keeps circles crisp at
  every scale. Apps that refuse server-side decorations (GTK4/libadwaita) keep their own (see L2).
- **Done when:** noobsbot, Dolphin and Kate show our title bar (Helium keeps its own, see F2), and the
  lights work.
- **Done:** `src/render/titlebar.rs` (28 px bar, SDF lights, glyphs on hover, grey when unfocused) through both
  decoration protocols (`src/decorations.rs`); the hover glyphs are text, not SDF.

### E1.4 Glass, corners & shadow shaders
- **What it is:** the visual engine for Part D materials.
- **Design:** exactly D3 (materials), D5 (radii), D6 (shadows).
- **Built with:** three GLSL shaders: `glass.frag` (tint + sheen + highlight + hairline), `rounded.frag`
  (squircle clipping for window content), `shadow.frag` (SDF soft shadow, no images).
- **Why:** one pass per surface, no blur (R2, R3). SDF = sharp at any size, no texture memory.
- **Shadow cost:** a 60 px soft shadow computed per pixel around every window is the one expensive shader at 4K,
  so shadows are rendered **once per (radius, blur, opacity)** into a small 9-slice texture and stretched; only the
  corners cost anything, and a frame with 20 windows stays under the E1.10 budget. *Measured in M1 (PERF.md): on an
  Intel iGPU at 1080p the 9-slice was slower than the per-pixel shader (draw calls, not fill, dominate), so M1 keeps
  per-pixel shadows; re-measure at 4K in M6.*
- **Done when:** a test scene shows every material in light and dark, matching the mockup; GPU time per frame
  under 1 ms on an Intel iGPU at 1080p.
- **Built:** `src/render/glass.frag`, `rounded.frag` and `shadow.frag`; shadows stay per pixel, as measured (PERF.md).

### E1.5 Wallpaper & desktop
- **What it is:** the background picture.
- **Design:** D11. Fills the screen; changes with light/dark mode.
- **Built with:** `png` + `zune-jpeg` decoders → one GPU texture, scaled once per output size.
- **Why:** small decoders instead of the large `image` crate (less code and RAM); decoding happens once, never per frame.
- **Done when:** the wallpaper shows and switches with dark mode.
- **Done:** `src/render/wallpaper.rs` (decoded once on a worker, a light and a dark picture, crossfaded on the
  switch).

### E1.6 Keyboard & first shortcuts
- **What it is:** typing in apps, plus desktop shortcuts.
- **Modifier model (decided here; every shortcut in this plan follows it):** **Super is the desktop key; apps keep
  Ctrl and the F-keys.** Noobsbot Desktop never takes combinations apps rely on: Ctrl+←/→ (word jump in every text
  field), Ctrl+1…9 (browser tabs), F3 (find next), Ctrl+Shift+I (DevTools). Super+key is ours unless listed as
  pass-through: Super+C/V/X/Z/A/S/F/T/R/L go to the focused app untouched, which is what makes the optional mode below
  possible. **"Mac keys" mode** (optional, off by default, Open-1 at M5): Super becomes Cmd.
  Every Super+letter/digit that isn't a desktop action is translated to Ctrl+… for the focused app (not for
  terminals): C/V/X/Z/A/S/F/T/R/L **and** W (close tab), N (new window), 1…9 (tabs). Desktop actions keep their Cmd-style
  keys: Super+Q quit, Super+H hide, Super+M minimise, Super+Tab, Super+Space; and because apps now receive Ctrl through
  Super, physical **Ctrl is free for the desktop**, so Spaces and Mission Control move to Ctrl+←/→,
  Ctrl+1…9, Ctrl+↑/↓, and Notification Center to the clock / hot corner only. Switching the mode swaps the whole
  binding set at once, so no key is ever bound twice.
  **Keyboard layout / input method switch:** Super+Ctrl+Space (plain Ctrl+Space clashes with IDE autocomplete),
  plus the menu-bar keyboard item (E6.12).
- **Design:** shortcuts in M1: Super+Enter (noobsbot-terminal), Super+B (Helium), Super+E (Dolphin), Super+Q (quit
  app), Super+W (close window), Super+M (minimise), Super+H (hide), Super+Tab (basic cycling; the full switcher comes
  in M4). Added later: Super+Space Spotlight and Super+N Notification Center (M4), Super+Alt+Esc Force Quit (M4),
  Super+←/→ and Super+1…9 Spaces, Super+↑ Mission Control, Super+↓ App Exposé (M5), Super+Shift+3/4/5 screenshots
  and Super+Ctrl+Q lock (M6), Ctrl+Shift+Esc System Monitor (M6; the one Ctrl combination, because no app uses it).
  Every binding is editable in `config.toml`. **F-key exception:** only F12 (quake terminal, the owner's existing
  habit) is taken by default; F3 (Mission Control) and F4 (Launchpad) exist as bindings but are **off by default**.
- **Built with:** xkbcommon through Smithay (keymaps and layouts; the initial layout comes from `localectl` /
  `/etc/vconsole.conf` and `config.toml` overrides it); bindings table in `config.toml`.
- **No stuck modifiers:** when a shortcut is consumed by the desktop, the app never sees a half key sequence: the
  key press is withheld, and if the modifier was already delivered its release is still delivered, so Super or Ctrl
  can never stay "held" inside an app (the classic compositor bug). Key repeat for shortcuts is ours; apps do their
  own repeat from `wl_keyboard.repeat_info`.
- **Why:** xkbcommon is the Linux standard, the same layout data as every desktop. Configurable bindings let
  the owner change anything without recompiling.
- **Done when:** typing works in every layout set in config; every M1 shortcut works.
- **Done:** `src/shortcuts.rs`, `src/input.rs` (no stuck modifiers), the system keyboard layout with `[keyboard]`
  overrides, `[bindings]` in `data/config.toml`.

### E1.7 Touchpad & mouse basics
- **What it is:** pointer, clicking, scrolling and app gestures.
- **Design:**
  | Input | Action |
  |---|---|
  | 1-finger tap | click (tap-to-click, on by default) |
  | 2-finger tap / click | right-click |
  | 2-finger scroll | scroll, **natural** direction by default |
  | 2-finger pinch | **passed to the app**: zoom in Helium, Dolphin and image viewers |
  | 2-finger swipe left/right | a horizontal **scroll** passed to the app; Helium turns it into back/forward |
  | Click + drag | select / move windows |
  | Mouse back/forward buttons | passed to apps |
- **Built with:** **libinput** via Smithay (tap, scroll, pinch and swipe events; libinput reports 2-finger movement as
  scrolling and only 3+ fingers as swipe gestures); `pointer-gestures` protocol to send pinch to apps.
- **Why:** libinput is the Linux standard for touchpads, with tuned palm rejection and acceleration.
  Passing 2-finger gestures to apps keeps browser pinch-zoom working.
- **Done when:** tap, right-click, natural scroll and pinch-zoom in Helium work on a real touchpad.
- **Done:** tap, natural scroll and the click method on the real screen (`src/backend/udev.rs`, `[touchpad]`); pinches
  and swipes the desktop doesn't use go to apps (`pointer-gestures`).

### E1.8 Settings file
- **What it is:** `~/.config/noobsbot-desktop/config.toml`.
- **Design:** fully commented defaults in `data/config.toml`; saving the file applies changes instantly. `version = 1`
  at the top, atomic writes (C11), and a `[[rules]]` section for window rules (E1.9).
- **Built with:** `serde`, `toml`, `notify` (C11).
- **Never refuses to start:** a config that fails to parse → the desktop starts with defaults and shows a
  notification with the line number; a half-written file during live reload (editors write in two steps) → reload is
  debounced 200 ms and the **last good config stays** until the file parses again. Unknown keys warn, never fail.
- **Done when:** changing accent colour, glass opacity or a shortcut applies without restart; a deliberately broken
  config starts the desktop with defaults and a notification.
- **Built:** `src/config.rs` (commented defaults in `data/config.toml`, unknown keys warned, the last good config kept
  when the file breaks and a notification with its line and Open File, once per new error, atomic saves, live reload
  after a 300 ms debounce).

### E1.9 Window rules
- **What it is:** per-app rules in `config.toml`, so the desktop remembers how an app's windows behave.
- **Design:** `[[rules]]` entries matched by Wayland `app_id` and an optional title pattern: `space = "all" | n`,
  `float_above = true` (always on top, e.g. Helium's picture-in-picture window, matched by its title), `size`,
  `position`, `fullscreen`, `decorations = false` and `layer = "overlay"` (the quake terminal, E2.8: above fullscreen apps), `opacity`. Rules apply when a window
  appears and again when its title changes.
- **Built with:** `rules.rs`, evaluated in `wm.rs`; `regex-lite` for title matching.
- **Why:** several elements need it (E2.8 quake placement, E5.1 Assign To Space, picture-in-picture staying on top);
  one mechanism instead of special cases.
- **Done when:** the quake-terminal rule and a picture-in-picture-on-top rule both work from config alone.
- **Done:** `src/rules.rs` (every key above, titles by regex), with the quake and picture-in-picture rules in
  `data/config.toml`.

### E1.10 Frame scheduling & input latency (why it feels instant)
- **What it is:** the rule for *when* we read input, *when* we draw and *when* the picture reaches the screen. This is
  what "responsive" means in practice: a drag, a hover or a keystroke shows its result in the very next refresh.
- **Design (the pipeline, per monitor):**
  ```
   input events ──► state update ──► (damage?) ──► render at  vblank − t_render ──► atomic KMS commit ──► vblank
   (libinput, immediately)             │                        ▲
                                       └── nothing changed → no frame, loop sleeps (0% CPU)
  ```
  | Rule | What it means |
  |---|---|
  | **Input first** | libinput events are read the moment they arrive, coalesced (10 pointer moves in one frame = one position) and applied to the state before the frame is drawn |
  | **Hardware cursor** | the pointer is drawn on the GPU's **cursor plane**, updated on every input event independent of compositing, so the cursor never lags even when a frame is late |
  | **Render as late as possible** | we start drawing at `vblank − t_render − 1 ms`, where `t_render` is measured per output (moving average of the last 30 frames; starts at 4 ms). Drawing just before the deadline means the newest input is included: **input → photon ≤ 1 frame + ~2 ms** |
  | **Damage only** | a frame is scheduled only when something changed; the clock changes once per minute; a settled animation schedules nothing |
  | **Apps never stall the shell** | each app's latest buffer is drawn; an app that hasn't produced a new one is drawn with its last (this is what makes dragging and resizing smooth even with slow apps) |
  | **Frame callbacks for apps** | apps get `wl_surface.frame` right after each vblank and `presentation-time` feedback, so Helium, Dolphin and noobsbot can run at the monitor's rate (60 / 120 / 144 Hz, per monitor) |
  | **Direct scanout** | a fullscreen app's buffer goes straight to the display plane (E6.3); overlay planes for video where the driver has them |
  | **One thread for the frame** | the compositor loop is single-threaded (C7); anything slow (image decoding, icon rasterising, the app index, update checks) runs on worker threads and sends results back through calloop channels, so a frame is never blocked by I/O |
  | **GPU budget** | shell + windows at 1080p on an Intel iGPU: **< 2 ms** per frame; 4K @ 200%: < 6 ms (both leave room for 120 Hz) |
  | **Dev mode** | in the nested winit backend, vsync comes from the host; the same scheduler runs, so latency behaviour is tested from M1 |
- **Built with:** Smithay's output frame-scheduling hooks + `presentation-time`; DRM atomic commits with
  `drm` planes (udev backend, M6); `frame.rs` for the per-output scheduler; cursor plane via Smithay's
  `CursorImageStatus`.
- **Why:** this is the same design other compositors use for render-time budgeting and triple buffering: measure
  the render time and draw as late as safely possible. Without it a compositor adds a whole frame of latency; with it
  Noobsbot Desktop feels as direct as drawing on X11 without a compositor, but tear-free.
- **Done when:** a 240 fps phone-camera test of a drag shows the window moving within 1 frame (+ ≤ 2 ms) of the
  pointer; idle CPU 0%; frame-time log stays under budget with 10 windows; no frame is missed while an app is
  deliberately frozen.
- **Built:** `src/frame.rs` `FrameClock` (draws at vblank − estimate − 1 ms, falls back after missed frames), frame
  callbacks and presentation feedback in both backends, the hardware cursor plane and direct scanout on udev; the
  latency figures are PERF.md's to prove.

### E1.11 Robustness rules (never crash, never block, never leak)
- **What it is:** the engineering rules that apply to every line of the compositor from M1. Each one is a known way
  Wayland compositors crash, freeze or lag; each has a test (Part K).
  | Rule | Why | How |
  |---|---|---|
  | **A broken app can't crash the desktop** | one protocol violation from any client must never panic | every protocol handler returns a **protocol error to that client** (it gets disconnected) instead of unwrapping; tested with a fuzzing client that sends garbage and out-of-order requests |
  | **Nothing blocks the frame loop** | one blocking call (a stuck D-Bus service, a slow disk, a DNS lookup) freezes the whole screen | **no blocking D-Bus calls** on the main thread: zbus runs on its own thread and talks to calloop through channels; file and network I/O on workers (E1.10); every D-Bus call has a timeout (2 s) |
  | **Missing services are normal** | desktop PCs have no battery, many machines have no Bluetooth, BlueZ may be stopped | every `services/*` client treats "service not present" as a state (item hidden, module greyed), never as an error; it re-subscribes when the service appears (`NameOwnerChanged`) |
  | **Caches have limits** | a glyph atlas, icon cache or thumbnail set that only grows is a slow memory leak | glyph atlas: one 2048² GPU texture (16 MB of **video** memory) with LRU eviction; icons: GPU textures, LRU-capped at 64 MB of video memory, CPU copies dropped right after upload (so they don't count against the RSS budget, Part I), + a **disk cache** (`~/.cache/noobsbot-desktop/icons/<theme>/<scale>/`, so Launchpad is instant after the first open — rasterising 500 SVGs with `resvg` takes seconds otherwise); Mission Control thumbnails only while open; notification history 100 entries, images ≤ 256 px; clipboard 10 MB; a **heaptrack** run every milestone |
  | **Monotonic time only** | the wall clock jumps on resume, NTP sync and time-zone change; wall-clock timers then fire instantly (lock loop) or never | animations, idle timers, gesture velocity and the frame scheduler use `CLOCK_MONOTONIC`; only the clock display reads wall time |
  | **GPU can disappear** | suspend/resume on NVIDIA and a DRM device reset destroy the EGL context; a compositor that doesn't handle it crashes on wake | on `EGL_CONTEXT_LOST` / DRM reset the renderer is recreated and every texture (wallpaper, glyphs, icons) re-uploaded lazily; apps re-send buffers on the next frame |
  | **Crash → clean exit, never a dead screen** | a panic on the real backend without releasing the GPU/VT leaves a black screen and a dead keyboard | `panic = "abort"` with a panic hook that logs the backtrace to the journal and releases the DRM master and seat (libseat does this on exit); greetd then shows the login screen; `coredumpctl` keeps the dump for the bug report |
  | **Rate limits at every door** | an app sending 1000 notifications/s, a held volume key, a slider dragged for 2 s | notifications ≤ 5/s per app (extra are coalesced); volume/brightness keys repeat at most 30/s; volume, mute and output changes go to their own thread and only the latest value per target is applied, at most every 30 ms (spawning a process per key repeat was a visible lag), and the monitor reads once per burst, never while our own changes are still queued (a stale reading moved the slider back) — **built**; the `pipewire` crate only if that still shows (it needs libpipewire headers in the build images); theme generator and config writes **debounced 300 ms** |
  | **Frame deadline misses are recovered** | a wrong render-time estimate (E1.10) means a skipped frame | 3 misses in a row → fall back to rendering right after vblank for 1 s, then re-measure |
  | **Helpers are supervised** | xwayland-satellite, the polkit agent or the IME dying silently breaks a feature | child helpers are restarted with back-off (max 3 in 60 s, then a notification) |
  | **Memory and CPU are measured, not hoped for** | | `PERF.md` per milestone + a 24 h soak test in M6 (RSS must not grow) |
- **Done when:** the fuzzing client and a frozen D-Bus service (`kill -STOP` on NetworkManager) leave the desktop
  fully usable; suspend/resume 20 times in a row on Intel and NVIDIA without a restart; the 24 h soak test holds RSS flat.
- **Built:** `scripts/fuzz-client.py`, 2 s D-Bus timeouts, GPU reset recovery (`src/backend/udev.rs`), the panic hook,
  notification rate limits, a capped clipboard, LRU text and icon caches, xwayland-satellite restarted
  (`src/xwayland.rs`, with a notification when it is given up), the icon disk cache (each size's RGBA under
  `$XDG_CACHE_HOME/noobsbot-desktop/icons/`, keyed by path, mtime and size, pruned after 30 unused days).

**M1 hardware covered:** GPU (nested), keyboard, touchpad, mouse.
**M1 done when:** all elements pass; idle CPU **0%**; RAM **< 60 MB**; mockup review passed.

---

## M2 — Menu bar, Control Center, clock & calendar, system status — size L
Goal: the top of the screen has a global menu bar and status area, and Wi-Fi, Bluetooth, sound, brightness and battery are all controllable.

### E2.1 Text & widget set
- **What it is:** the foundation for all shell UI: text rendering + the D8 components.
- **Built with:** cosmic-text + glyph atlas (C8); `src/ui/` widgets (C6); Phosphor + WhiteSur icons rendered by `resvg`
  lazily on a worker thread and kept in the disk cache (E1.11), with the app index watching `/usr/share/applications`
  and `~/.local/share/applications` through `notify` so newly installed apps appear without a restart.
- **Why:** see C6, C8. Every later element is assembled from these, so the design stays identical everywhere.
- **Done when:** a "component gallery" test screen shows every D8 component in light and dark, matching the mockup.
- **Built:** `src/text.rs` (cosmic-text, tabular digits, an LRU of rendered labels instead of a glyph atlas),
  `src/render/ui.rs` and the gallery (`src/render/components.rs`, `--show gallery`), `src/symbols.rs`, the app index
  and icon threads (`src/apps.rs`) with the icon disk cache (E1.11).

### E2.2 Menu bar
- **What it is:** the bar across the top of every screen.
- **Design:**
  ```
  ┌──────────────────────────────────────────────────────────────────────────────┐
  │ ◆  Helium  File  Edit …            ⌨ EN  ▣ tray   Wi-Fi  BT  85% ▮  🔊  ⊞  Sat 17 Oct  9:41 │ 24 px
  └──────────────────────────────────────────────────────────────────────────────┘
  ```
  - Height **24 px**, `bar` material, elevation 1, full width.
  - Left: **logo** (the Noobsbot logo mark, 16 px) → logo menu: About This Computer · System Settings… · Software Update… (shows a count badge when
    one is pending, E6.13) · (separator) · Force Quit… · (separator) · Sleep · Restart… · Shut Down… · (separator) · Lock Screen · Log Out….
  - **Active app name** in Headline, then the app menu: About · Hide · Hide Others · Show All · Quit (Super+Q).
  - Right (12 px gaps, symbols 16 px): tray icons · keyboard layout (only if more than one) · Wi-Fi · Bluetooth ·
    battery (% optional) · volume · **Control Center** ⊞ · **date & time**.
  - Click an item → its menu/popover drops 4 px below the bar; the item gets a pressed pill highlight (radius 4).
    Wi-Fi, Bluetooth and volume open their Control Center module under the item (toggle and networks, toggle and
    devices, slider and outputs) with a "… Settings…" row; behind a captive portal Wi-Fi adds "Sign in to network…".
    While a screen recording runs, a stop item with its elapsed time sits left of Wi-Fi; a click stops it.
  - Restart / Shut Down / Log Out show an **alert** (D8) with a 60 s countdown.
- **Built with:** our widgets; tray through **StatusNotifierItem** + **dbusmenu** (zbus; **we host the `StatusNotifierWatcher`** service, because
  without a watcher apps never register their icons); power actions via
  **logind**.
- **Why:** this is the menu-bar layout of the design (R1). StatusNotifierItem is the tray standard modern apps use.
- **Done when:** every item opens its menu (the battery its menu; Wi-Fi, Bluetooth, volume their module); tray icons from apps appear and their menus work; Sleep/Restart/
  Shut Down/Log Out work.
- **Built:** `src/render/menubar.rs`, `src/shell/mod.rs` (logo, app and status menus), the tray host
  (`src/services/tray.rs`), power actions through logind with the 60 s alert (`src/shell/alert.rs`), the pending
  updates' count beside Software Update… (in the shortcut column; "Restart to Update…" once a download is verified).

### E2.3 Control Center
- **What it is:** quick controls in one glass panel, opened from ⊞ in the menu bar.
- **Design (320 px wide, `panel` material, radius 16, padding 10, modules in `card` material, radius 12):**
  ```
  ╭──────────────────────────────────────╮
  │ ╭──────────────────╮ ╭──────────────╮│
  │ │ (◉) Wi-Fi        │ │ ☾  Do Not    ││
  │ │     Home-5G      │ │    Disturb   ││
  │ │ (◉) Bluetooth    │ ╰──────────────╯│
  │ │     AirPods      │ ╭──────────────╮│
  │ │ (◉) AirDrop… n/a │ │ ◐  Dark Mode ││
  │ ╰──────────────────╯ ╰──────────────╯│
  │ ╭──────────────────────────────────╮ │
  │ │ Display   ☀ ━━━━━━━━━━━○──────── │ │
  │ ╰──────────────────────────────────╯ │
  │ ╭──────────────────────────────────╮ │
  │ │ Sound     🔊 ━━━━━━━━━━○───── ›  │ │  › = output picker
  │ ╰──────────────────────────────────╯ │
  │ ╭──────────────────────────────────╮ │
  │ │ ♫ Song title — Artist   ◀◀ ▶ ▶▶  │ │  Now Playing
  │ ╰──────────────────────────────────╯ │
  │ ╭──────────────────────────────────╮ │
  │ │ 🔋 85%  Power: Balanced ▾        │ │
  │ ╰──────────────────────────────────╯ │
  ╰──────────────────────────────────────╯
  ```
  (The AirDrop row isn't included; it's only drawn above to show the full layout. Our network module has
  Wi-Fi + Bluetooth only.)
  - Round toggles 28 px (D8). Clicking a module's title expands it in place (spring `smooth`): Wi-Fi shows a
    network list with signal bars and a lock icon; Bluetooth shows devices with battery %; Sound shows outputs
    (speakers, headphones, HDMI, Bluetooth). Power mode's ▾ lists power-profiles-daemon's profiles, the current one
    checked. Built: every module's list springs open and closed (`src/shell/control.rs` `expand`; reduce motion: at once).
  - Wi-Fi password → **alert** with a secure text field.
  - Opens with a `snappy` fade + scale 0.98→1 from its menu-bar icon; Esc or click-outside closes it.
- **Built with:**
  | Module | Service (via zbus unless noted) | Why this service |
  |---|---|---|
  | Wi-Fi, Ethernet | **NetworkManager**; Noobsbot Desktop registers as its **SecretAgent**, so the Wi-Fi password alert can hand the password to NM | complete, stable D-Bus API, used by every major desktop |
  | Bluetooth | **BlueZ**; Noobsbot Desktop registers an **`org.bluez.Agent1`** (PIN entry / passkey-confirm alert); without an agent, pairing keyboards and phones fails | the only Linux Bluetooth stack |
  | Display brightness | **logind** `SetBrightness` | changes brightness without root and without an extra daemon |
  | Sound: outputs **and input** (microphone device, level, mute, the mic-mute key) | **PipeWire** via `wpctl` (M2), the `pipewire` crate later | fastest path to working; native later avoids spawning a process |
  | Now Playing | **MPRIS** | every media app (Helium, Spotify, mpv) publishes it |
  | Battery | **UPower** | standard; also gives Bluetooth device battery levels |
  | Power mode | **power-profiles-daemon** | standard power saver / balanced / performance switch |
  | Do Not Disturb | our notification server (M4; toggle stored now) | built in (C5) |
  | Dark mode | our theme (D2) | built in |
- **Done when:** every control changes the real system state, and changes made elsewhere (e.g. unplugging
  headphones) update the panel live with no polling.
- **Done:** `src/shell/control.rs` and `src/render/control.rs`: the NetworkManager secret agent, the BlueZ agent,
  logind brightness, PipeWire followed with `pw-dump --monitor` and set with `wpctl` (no `pipewire` crate, E1.11),
  MPRIS, UPower, and power-profiles-daemon's profiles under Power mode's ▾ (`Module::Power`); the panel opens on a
  `snappy` fade and scale and fades out over 150 ms.

### E2.4 Clock
- **What it is:** date and time at the right end of the menu bar.
- **Design:** `Sat 17 Oct  9:41 AM` in Body with **tabular digits** (the bar doesn't jiggle as time changes).
  Options: 12/24-hour, show date, show day name, show seconds (off by default). Click → **Notification Center** (M4;
  until M4 it opens the calendar popover below).
- **Built with:** system time; **systemd-timesyncd** keeps it accurate (NTP); `chrono` formats it.
- **Why:** timesyncd is already part of systemd (nothing extra to install); chrono is the standard Rust
  date library.
- **Done when:** the time is correct after boot; it redraws exactly once per minute (once per second only when
  seconds are switched on), aligned to the minute boundary on the monotonic clock (E1.11).
- **Done:** `src/clock.rs` (formats, tests), a timer on the minute (or second) boundary (`src/state.rs`), tabular
  digits; a click opens Notification Center.

### E2.5 Calendar
- **What it is:** a month calendar (a popover in M2, then the calendar widget inside Notification Center from M4).
- **Design (280 px wide, `popover` material, radius 12):**
  ```
  ╭──────────────────────────────╮
  │  October 2026          ‹  ›  │  Title 3
  │  S   M   T   W   T   F   S   │  Caption, label-secondary
  │                  1   2   3   │
  │  4   5   6   7   8   9  10   │
  │ 11  12  13  14  15  16 (17)  │  today = accent filled circle, white text
  │ 18  19  20  21  22  23  24   │
  │ 25  26  27  28  29  30  31   │
  ╰──────────────────────────────╯
  ```
  - Days of other months at `label-tertiary`; week starts on the locale's first day (configurable); optional week numbers.
  - ‹ › change month with a horizontal slide (spring `snappy`); 2-finger horizontal swipe also changes month; clicking
    the month title returns to today. Built: the swipe follows the fingers 1:1 with the next month sliding in beside,
    then a `gesture` spring turns it past 30% of the card or on a flick, else back; ‹ ›, the title and ←/→ slide on
    `smooth`; both grids are cut at the card's edge; reduce motion turns the month at once.
  - ~~Later (optional): event dots under days, from local `.ics` files~~, then CalDAV/Google. Done: dots from
    `[calendar] files` (`src/ical.rs`, `src/events.rs`, E7.2). Not yet: CalDAV/Google.
- **Built with:** `chrono` (date math); our widgets.
- **Why:** pure date math, so no calendar library or service is needed until events are added.
- **Done when:** the correct month grid for any month and year, including leap years (unit tested).
- **Done:** `src/calendar.rs` (grid, locale week start, week numbers, leap years tested), `src/render/calendar.rs`,
  the slides and swipe in `src/shell/notify.rs`.

### E2.6 Time zone & date settings (backend)
- **What it is:** setting the time zone and automatic time.
- **Built with:** **systemd-timedated** over D-Bus (`org.freedesktop.timedate1`). **Location:** there is no reliable
  free location service any more (Mozilla Location Service shut down in 2024), so **automatic time zone, "Auto" dark
  mode and Night Shift sunset times are manual by default**: the zone is picked once; dark mode and Night Shift run on a
  schedule (default 19:00–07:00). `geoclue` (BeaconDB backend) is an optional package, off by default.
- **Why:** standard; changing the zone needs no terminal; no feature silently depends on a dead service. (UI in M7.)
- **Done when:** changing `timezone` in config updates the system zone and the clock immediately.
- **Done:** `SetTimezone` on `org.freedesktop.timedate1` when `timezone` changes (`src/services/mod.rs`), NTP and the
  time in Settings (`settings/src/system.rs`), dark mode on the 19:00–07:00 schedule; the clock shows the new zone at
  its next redraw.

### E2.7 On-screen display (OSD) for media keys
- **What it is:** the small indicator when you press volume / brightness / mute keys.
- **Design:** `hud` material pill, **280 × 48 px**, radius 24, top-right under the menu bar.
  Symbol 24 px + a progress bar (D8) + the value. Appears with `snappy`, hides after 1.5 s.
  The mute key shows a crossed-out speaker. It shares the top-right corner with notification banners (E4.1): while
  the OSD is visible, banners move down by 56 px (spring `snappy`), so the two never overlap.
- **Built with:** key events from libinput; PipeWire (`wpctl`) / logind brightness.
- **Done when:** volume, brightness, mute and play/pause keys all work and show the OSD.
- **Built:** `src/render/osd.rs`, shown 1.5 s (`OSD_TIME`), for every media key in `src/shortcuts.rs`; a held volume
  key costs one `wpctl` call per 30 ms (`src/services/sound.rs` `coalesce`, E1.11); banners spring the 56 px down
  under the OSD and back (`banners_dodge`, `smooth`).

### E2.8 Quake terminal hotkey
- **What it is:** F12 (configurable) drops noobsbot-terminal down from the top, on any Space.
- **Design:** full width, 40% height, slides down with `smooth`; same glass title-bar-less window with radius 10 at the bottom corners.
  Built: slides down on show and up on hide (`src/anim.rs`, `window_opened` / `window_restored` / `window_minimising`); reduce motion fades.
- **Built with:** noobsbot's existing `noobsbot --quake` (it toggles a running instance through its own
  socket) + a window rule in Noobsbot Desktop for its placement.
- **Why:** reuses the owner's existing feature; no change to noobsbot needed. F12 is also Helium's DevTools key: while
  the desktop owns F12, DevTools still opens with Ctrl+Shift+I; the key is configurable.
- **Done when:** F12 slides it in and out at once (a fade with reduce motion), on top of fullscreen apps too.
- **Built:** F12 shows and hides it (`toggle_quake`, starting `noobsbot --quake` when it isn't running), placed by the
  quake `[[rules]]` entry (`space = "all"`, `layer = "overlay"`); square top corners for a rule hanging it from the top
  edge (`Actions::hangs`: its rounded rect stretched a radius above it).

### E2.9 Universal glass theme for apps (Dolphin, Kate & every other app)
- **What it is:** apps that aren't drawn by Noobsbot Desktop (Dolphin, Kate, Partition Manager, System Monitor,
  Filelight, file-picker dialogs) get **the same glassy look** as the desktop, from the same Part D tokens,
  and switch light/dark together with it. **Scope:** the generator plus Dolphin and Kate are verified in M2 (nested, on
  the dev machine); Partition Manager, System Monitor and Filelight in M6, once `install.sh` installs them.
- **Design: Dolphin as the reference (Finder-like):**
  ```
  ╭──────────────────────────────────────────────────────────────────╮
  │ ● ● ●   ‹ ›   Documents                       ⊞ ☰   🔍 Search     │  our title bar (E1.3) + Dolphin toolbar
  ├───────────────┬──────────────────────────────────────────────────┤  in one glass strip
  │ Favourites    │  ▣ Report.pdf          2.4 MB   Today 9:30        │
  │  🏠 Home      │  ▣ Photos              —        Yesterday         │  file area: solid card colour
  │  🖥 Desktop    │  ▣ notes.md            3 KB     12 Oct            │  (readable, like Finder)
  │  📄 Documents ◀│                                                    │
  │  ⬇ Downloads  │                                                    │
  │ Locations     │                                                    │
  │  💾 USB Drive ⏏│                                                    │
  │ (glass panel) │                                                    │
  ╰───────────────┴──────────────────────────────────────────────────╯
  ```
  | Part of Dolphin | Look (Part D) |
  |---|---|
  | Title bar | ours: traffic lights, 28 px (E1.3); Dolphin's toolbar sits right under it in the same glass strip |
  | Sidebar (Places) | `panel` glass (88% / 85% opacity), rows 28 px, selected row = accent @ 20%, WhiteSur folder icons, Subheadline section headers |
  | File area | solid `card` colour (white / `#323232`) so file names stay perfectly readable, like Finder |
  | Selection | accent fill + white text (D7) |
  | Menus, context menus, tooltips | `popover` glass, radius 8, D8 menu spec |
  | Buttons, fields, scrollbars | D8 sizes and radii; thin overlay scrollbars |
  | Fonts | Inter (UI), JetBrains Mono (fixed width) |
  | Icons | WhiteSur (light/dark matching the mode) |
  - Kate, Partition Manager, System Monitor, Filelight: the same rules (glass sidebars/toolbars, solid content area).
- **Built with:**
  | Piece | What we ship | Why |
  |---|---|---|
  | **Kvantum theme "Noobsbot Glass"** (light + dark) | our own Kvantum theme, derived from WhiteSur's Kvantum theme, recoloured and resized to Part D. Kvantum's translucency is on (`translucent_windows`, window/menu opacity set to match D3), with **no blur** | Kvantum is the standard way to fully restyle Qt apps on any desktop, and it supports translucent windows. Starting from WhiteSur gives the right shapes; our tokens give our exact colours |
  | **Qt colour scheme "Noobsbot Glass"** (light + dark) in `~/.local/share/color-schemes/`, selected in `~/.config/kdeglobals` | Dolphin and Kate take some colours (views, selection, links) from the colour scheme, not from the style; both must match |
  | `kdeglobals` fonts + icons | `font=Inter,10`, `fixed=JetBrains Mono,10`, `[Icons] Theme=WhiteSur` / `WhiteSur-dark` | Dolphin and Kate read these on any desktop |
  | Session env | `QT_QPA_PLATFORMTHEME=qt6ct`, `QT_STYLE_OVERRIDE=kvantum` (E6.7) | forces every Qt app to use the theme |
  | **GTK** (file-picker dialogs, any GTK app) | WhiteSur-gtk recoloured to Part D; `gsettings org.gnome.desktop.interface` for theme, icons, font, `color-scheme` | GTK apps read gsettings; GTK can't do translucent windows, so it gets the same colours, solid |
  | **Helium** | **not themed: kept exactly as it is** (its own theme, its own tab-strip title bar, its own light/dark setting) | owner's choice (F2) |
  | **Theme generator** | `noobsbot-desktop theme --write` writes all of the files above from `theme.rs` tokens, and runs on every accent/mode/opacity change. It **merges**, never overwrites: in `kdeglobals` it owns only `[General] ColorScheme/font/fixed`, `[Icons]` and `[Colors:*]`, so F1's `TerminalApplication` and anything the owner set survive; likewise for the gsettings keys | **one source of truth**: change the accent once, and the desktop and every app change together |
- **Light/dark & accent switching:** toggling dark mode (Control Center) or changing the accent rewrites the Kvantum
  config, colour scheme, kdeglobals and gsettings, and sends the portal `color-scheme` change. GTK apps update
  live; Qt apps update live for colours (Dolphin and Kate watch kdeglobals); a Kvantum *style* change applies to newly opened
  windows (reopen to update), which is a Kvantum limitation.
- **Why this way instead of a "real" glass style plugin:** writing our own Qt style plugin (C++) would be a large
  extra project in another language. Kvantum + a colour scheme gets the same look with config files only.
  **Rejected:** Breeze and Adwaita (other desktops' looks), a custom Qt style plugin (too much code for the gain).
- **License note:** WhiteSur (Kvantum/GTK) is GPL-3.0, so our derived "Noobsbot Glass" themes are GPL-3.0 too.
- **Risk + spike (first week of M2):** Kvantum's translucent windows have known Qt 6 Wayland bugs (opaque or black
  windows in some apps). Spike: Dolphin with `translucent_windows=true` inside the nested compositor. If it fails, the
  **fallback is decided now**: Noobsbot Glass at 100% opacity (same colours, radii, fonts, icons); Dolphin still
  matches the desktop, only the sidebar translucency is lost (L8).
- **Done when (M2):** Dolphin and Kate match the approved mockups in light and dark; **(M6):** Partition Manager, System
  Monitor and Filelight too; the sidebar is glassy (or solid if the L8 fallback was taken), the content area readable (contrast ≥ 4.5:1); changing the accent recolours the desktop
  and these apps together.
- **Done:** `src/theme.rs` (`noobsbot-desktop theme --write`: the Kvantum theme, Qt colour schemes, kdeglobals,
  gtk-3.0 / gtk-4.0 css, gsettings), rewritten on an accent or mode change; the session wrapper sets the Qt style
  variables. The look against the mockups isn't checked here.

**M2 hardware covered:** Wi-Fi, Ethernet, Bluetooth, sound (speakers, mic, headphones, HDMI, Bluetooth audio),
brightness, battery, media/function keys.
**M2 done when:** all elements pass; idle CPU still 0%; mockup review passed.

---

## M3 — Dock — size M
Goal: the owner's current dock (Plank, Transparent-Tight theme), rebuilt natively with identical look, functions and behaviour.

### E3.1 Dock — a faithful copy of the owner's current Plank dock
- **What it is:** the app bar at the bottom of the screen. The owner's requirement: it must be **the same as the
  dock used today**, in function, design and behaviour. Today's dock is **Plank** with the **Transparent-Tight**
  theme. The values below were read from the owner's live Plank settings (`dconf /net/launchpad/plank/`), its theme
  file and `linux-setup/arch-setup.sh`. Noobsbot Desktop rebuilds that dock inside the compositor (C5), and
  every value below becomes the default in `config.toml`.
- **Settings copied from Plank:**
  | Plank setting | Current value | Noobsbot Desktop dock |
  |---|---|---|
  | position | `bottom` | bottom edge |
  | alignment | `center` | centred |
  | icon-size | `84` | **84 px** icons |
  | zoom-enabled / zoom-percent | `true` / `150` | magnification on, **150%** (icons grow to 126 px under the pointer) |
  | hide-mode | `intelligent` | **intelligent hide**: hides when the focused window overlaps the dock; shows again when nothing overlaps or when the pointer touches the bottom edge |
  | theme | `Transparent-Tight` | same look (see Design) |
  | docks per monitor | `dock1` (main screen) + `dock2` (`HDMI-1`), each with its own items | **one dock per monitor, each with its own pinned list** (done: `[dock] displays = "all"`; a display uses the shared `items` until Options ▸ Separate Items on This Display copies them into its own `[dock] outputs` list) |
  | dock-items (dock1) | Google Chrome, VS Code, Brave, Dolphin, Kate, Spectacle, kitty, noobsbot | same order (Brave's slot holds Helium, the new default browser). On a fresh system only installed apps are shown. Default fresh-install pins = Helium, Dolphin, Kate, noobsbot-terminal (Chrome, VS Code, Spectacle, kitty appear in their old places once installed) |
- **Design (Transparent-Tight theme, copied value by value):**
  ```
       ┌────┐┌────┐┌────┐┌────┐┌────┐┌────┐      84 px icons, almost no gap between them
       │icon││icon││icon││icon││icon││icon│      (ItemPadding 0.5)
   ▄▄▄▄│    ││    ││    ││    ││    ││    │▄▄▄▄  background: a short bar behind the LOWER part
   ████└────┘└────┘└────┘└────┘└────┘└────┘████  of the icons (TopPadding -12, so icons stick out above)
   ████████████▪█████▪███████████████▪█████████  ▪ = running indicator
  ```
  | Theme value | Current | Meaning in Noobsbot Desktop |
  |---|---|---|
  | TopRoundness / BottomRoundness | 0 / 0 | **square corners** |
  | LineWidth | 0 | **no border lines** |
  | FillStartColor → FillEndColor | `rgb(41,41,41)` → `rgb(80,80,80)`, alpha 255 | **dark grey vertical gradient**, opaque |
  | HorizPadding | 0 | no padding at the left/right ends |
  | TopPadding | -12 | background is shorter than the icons; icons rise above it |
  | BottomPadding | 2 | small gap under the icons |
  | ItemPadding | 0.5 | icons almost touching (tight) |
  | IndicatorSize | 5 | small running-indicator dot |
  | IconShadowSize | 1 | very small shadow behind each icon |
  | BadgeColor | transparent | badge background follows the default red (D8) only if this is changed |
  - Icons: WhiteSur icon theme (Part H), the same theme the owner's dock uses today (`WhiteSur-Dark` Plank theme
    is also installed today, kept as an alternative preset).
  - This look is the **default**. Two more presets are available in config/Settings: `whitesur-dark` (the other
    installed Plank theme) and `glass` (Part D panel material, for a fully glassy desktop).
- **Behaviour & timings (copied from the theme, in milliseconds, so it *feels* the same as Plank):**
  | Behaviour | Plank value | Noobsbot Desktop |
  |---|---|---|
  | Click animation | ClickTime 300 | icon darkens/brightens for 300 ms on click |
  | Launch bounce | LaunchBounceTime 600, height 0.625 × icon | icon bounces while the app starts |
  | Urgent bounce (app wants attention) | UrgentBounceTime 600, height 1.67 × icon | bigger bounce |
  | Urgent glow while the dock is hidden | GlowSize 30, GlowTime 10000, GlowPulseTime 2000, UrgentHueShift 150 | pulsing glow at the screen edge for 10 s |
  | Active window indicator | ActiveTime 300 | the focused app's indicator animates in 300 ms |
  | Icons sliding in/out | SlideTime 300 | new and removed items slide |
  | Moving an item | ItemMoveTime 450 | reorder animation |
  | Hide / show | HideTime 250, FadeOpacity 1 (slide, no fade), CascadeHide on | dock slides down off-screen in 250 ms; background and icons leave the edge together |
  | Zoom | 150% | neighbouring icons grow smoothly with distance from the pointer (Plank-style parabolic zoom) |
  - These timed animations are a deliberate exception to the spring rule (D10): matching Plank's feel is the requirement.
- **Functions (Plank's, all kept):**
  | Action | Result |
  |---|---|
  | Click a pinned app that isn't running | launches it (launch bounce) |
  | Click a running app | focuses its window (brings it from another Space if needed) |
  | Click the app that's already focused | minimises it (Plank behaviour) |
  | Middle-click | opens a new window/instance |
  | Scroll on an icon | cycles through that app's windows |
  | Right-click | menu: app's windows list · Keep in Dock (pin/unpin) · New Window · Close All |
  | Drag an icon | reorder; drag it off the dock to remove it (poof animation) |
  | Drag a `.desktop` file/app onto the dock | pin it |
  | Drop files onto an app icon | opens them with that app |
  | Badges and progress | count badges and progress bars from apps that publish them (LauncherEntry D-Bus API, which Plank also uses) |
  | Ctrl+right-click (Plank's preferences) | opens Dock settings (`config.toml` until the Settings app, M7) |
  Extras on top of Plank (needed by other parts of this plan, off unless used): **minimised windows** go to the
  dock's right end (E5.5 minimise animation), and **Options ▸ Assign To Space** in the right-click menu (E5.1).
- **Built with:** `freedesktop-desktop-entry` (reads `.desktop` files, which is what Plank's `.dockitem` files point to),
  `freedesktop-icons` + `resvg` (icons), Wayland **app id** to match windows to icons, zbus for the
  LauncherEntry badge API, our renderer for the theme. A one-time **importer** can read an existing Plank setup
  (`.dockitem` files + dconf values) and write them into `config.toml`, so moving from Plank is automatic.
- **Why rebuild it instead of running Plank itself:** Plank is an **X11-only** program (it can't run as a dock on
  Wayland) and is no longer actively developed. It also runs as a separate process, which costs extra RAM (C5).
  Rebuilding it inside Noobsbot Desktop keeps the same look and behaviour while being native Wayland, faster and lighter.
- **Done when:** compared side by side with Plank (same icon size, zoom, theme, hide mode and timings), the dock is
  indistinguishable in look and behaviour, and every function in the table works, on each monitor.
- **Built:** `src/shell/dock.rs`, `src/shell/dock_fx.rs` and `src/render/dock.rs`: zoom, every hide mode, launch and
  urgent bounces, the urgent glow, running dots, drag to reorder, remove and pin, file drops, badges and progress
  (`com.canonical.Unity.LauncherEntry`), the Plank import (`src/shell/plank.rs`), a dock per display (E8.3), the
  focused app's highlight fading in and out over 300 ms (ActiveTime), the `whitesur-dark` and `glass` presets
  (`[dock] style`, Settings ▸ Dock ▸ Style; `Look` in `src/render/dock.rs`: tint, opacity, radius, light edge and the
  highlight's colour; `glass` is the most see-through tint, with no blur by design, R2). Not yet: the small shadow
  behind each icon (the shadow shader draws rounded rects, not an icon's shape).

**M3 done when:** all of the above; mockup review passed.

---

## M4 — Notifications, Notification Center, Spotlight, Launchpad, app switcher, USB — size L
Goal: everything that informs you and helps you find things.

### E4.1 Notification banners
- **What it is:** pop-up messages from apps and the system.
- **Design:**
  ```
                                       ╭────────────────────────────────────╮
                                       │ ▣  Helium                      now  │  card material, radius 16
                                       │    Download complete               │  Headline
                                       │    report.pdf (2.4 MB)             │  Body, max 2 lines
                                       ╰────────────────────────────────────╯
  ```
  - **344 px** wide, min 64 px tall, radius 16, `card` material, elevation 2; 12 px from the right edge, 8 px
    under the menu bar.
  - App icon 32 px; title Headline; body Body (max 2 lines, "…"); time Caption `label-secondary` top-right.
  - Hover → a ✕ appears at the top-left corner and action buttons appear (e.g. "Open", "Reply").
  - **Banner** style: slides in from the right (`smooth`), stays 5 s (hover pauses). **Alert** style (apps can ask):
    stays until you act.
  - At most 3 stacked (newest on top); more collapse into "+N more".
  - **2-finger swipe right** on a banner dismisses it (follows the finger); click opens the app. Built: it follows 1:1
    (rubber band leftwards), then a `gesture` spring off-screen past 30% of its width or on a flick, else back;
    reduce motion decides on lift without moving.
  - Silent while Do Not Disturb is on or an app is fullscreen (configurable).
- **Built with:** Noobsbot Desktop **is** the notification server: it implements the standard
  `org.freedesktop.Notifications` D-Bus interface (zbus). Supports actions, icons, urgency, replace, progress hints.
- **Why built in:** every Linux app already sends notifications through this standard, so they all work
  unchanged. Built in (not mako/dunst) for RAM and an identical look (C5).
- **Done when:** `notify-send "Title" "Body"` shows a correct banner; actions call back the app; DND silences everything.
- **Done:** `src/services/notifications.rs` (actions, urgency, replaces, the progress hint, history) and the banners
  in `src/shell/notify.rs` (three, then "+N more"; 5 s; alert style; quiet under Do Not Disturb or a fullscreen app;
  the swipe above).

### E4.2 Notification Center (the notification panel)
- **What it is:** a panel on the right side with all notifications plus widgets.
- **How to open:** click the **date & time** in the menu bar · **Super+N** · optional hot corner. Close: Esc, click
  outside, or click the clock again.
- **Design:**
  ```
                                                   ╭──────────────────────────────╮
                                                   │ Saturday, 17 October      ⚙   │ Title 2
                                                   │                              │
                                                   │ Notifications    Clear All  │ Subheadline / button
                                                   │ ╭──────────────────────────╮ │
                                                   │ │ ▣ Helium            9:30  │ │ grouped by app
                                                   │ │   Download complete      │ │ (stacked cards,
                                                   │ ╰┬────────────────────────┬╯ │  click to expand)
                                                   │  ╰────── 2 more ──────────╯  │
                                                   │ ╭──────────────────────────╮ │
                                                   │ │ ▣ System           8:12  │ │
                                                   │ │   USB drive connected    │ │
                                                   │ ╰──────────────────────────╯ │
                                                   │                              │
                                                   │ ╭── Calendar ──────────────╮ │ widget (E2.5)
                                                   │ │  October 2026      ‹ ›   │ │
                                                   │ │  …month grid…            │ │
                                                   │ ╰──────────────────────────╯ │
                                                   │ ╭── Batteries ─────────────╮ │ widget
                                                   │ │ 💻 85%   🎧 AirPods 60%   │ │
                                                   │ ╰──────────────────────────╯ │
                                                   ╰──────────────────────────────╯
  ```
  - Column **360 px** wide, from under the menu bar to 8 px above the screen bottom, 8 px from the right edge; `panel`
    material, radius 16; slides in from the right (`smooth`), and the content scrolls.
  - **Header:** today's date (Title 2) + ⚙ (opens Notifications settings).
  - **Notifications:** grouped by app as stacked cards (`card`, radius 16). Click a stack to expand it; hover a
    group → ✕ clears the group; "Clear All" button. Each notification keeps its actions. Persists across
    restarts (history file, last 100).
  - **Do Not Disturb** banner at the top when active ("Do Not Disturb is on · Turn Off").
  - **Widgets** (in `card` material, radius 12):
    | Widget | Content | Data from |
    |---|---|---|
    | Calendar | month grid (E2.5); ~~later~~ "Up Next" events (done: the card above the month) | `chrono`; `.ics` (`src/ical.rs`, `src/events.rs`); CalDAV not yet |
    | Batteries | laptop + Bluetooth devices (headphones, mouse) | UPower |
    | World Clock (optional) | Built: after Weather, a row per city in `[widgets] clocks` (off while empty): its time in the clock's 12/24 h format, then the day and offset from here ("Tomorrow, +5:45"); redrawn on the menu-bar clock's minute tick | the system zoneinfo, read as `.ics` TZIDs are (no `chrono-tz`) |
    | System (optional) | CPU %, memory used, disk free, network ↑/↓ (refreshes every 2 s only while the panel is open). Built as a widget app: CPU, memory, disk and network ↑/↓ every 5 s; not yet refreshing only while open (the widget protocol doesn't say when a widget is shown) | `/proc`, `/sys` (`src/bin/widget-system.rs`) |
    | Weather (done, off by default) | Open-Meteo forecast for the place in `[weather]` (no key, no account) | `src/services/weather.rs` (E7.2) |
  - "Edit Widgets…" (under the widgets) opens Settings ▸ Notifications, which shows, hides and reorders other apps' widgets (`[widgets]`) for Notification Center and the desktop.
- **Built with:** our widgets; the notification store from E4.1; UPower; chrono.
- **Why:** this is the design's notification panel (R1, R13); a single place for missed notifications.
- **Done when:** notifications arrive in history, group by app, expand/collapse, clear and act; widgets show live data;
  open/close animates at 120 Hz.
- **Done:** `src/shell/notify.rs` `center` (history of 100, groups, Clear All, the Do Not Disturb row, ⚙ and Edit
  Widgets…), opened by the clock and Super+N; Up Next and Weather as in the table.

### E4.3 Spotlight (search & launcher)
- **What it is:** Super+Space opens a search box to launch apps and calculate.
- **Design:**
  ```
               ╭──────────────────────────────────────────────────────────╮
               │ 🔍  brav|                                                │ 52 px, Title 1
               ├──────────────────────────────────────────────────────────┤
               │ ▣  Helium                                Application  ⏎ │ top hit = accent row
               │ ▣  …                                                      │
               ╰──────────────────────────────────────────────────────────╯
  ```
  - **680 px** wide, top edge at 22% of screen height, `panel` material, radius 14, elevation 5.
  - Field 52 px: magnifier symbol 20 px, Title 1 text, placeholder "Spotlight Search" in `label-tertiary`.
  - Results appear under the field as you type (field grows downward, `snappy`): rows 40 px with icon 32 px,
    name in Body and kind in Callout `label-secondary` on the right. Max 8 rows, grouped: Top Hit · Applications ·
    Calculator · Files · Settings. Apps keep 3 rows free for files, so files arriving later never move a row above.
  - ↑/↓ select, Enter opens, Super+Enter = show in the file manager (files), Esc clears, then closes.
  - Settings: every page and every `SETTINGS` row, kind "System Settings", the page's symbol on its colour; Enter
    opens Settings on that page.
  - Files (Open-4): recent files, then `plocate` in the home folder (hidden paths dropped), asked 150 ms after
    typing stops on a worker thread; a stale answer is dropped. Kind = mime type by extension, or "Folder".
  - Calculator: "12*7" → a row "= 84" (Enter copies it).
- **Built with:** the shared app index (`apps.rs`); `nucleo-matcher` (the fast fuzzy matcher from the Helix
  editor); a tiny expression evaluator crate (e.g. `evalexpr`).
- **Why:** the index lives in memory, so results appear on every keystroke with zero disk access.
  Fuzzy matching finds "Helium" from "hlm". Files come from `plocate`'s existing database, not an index of our own
  (Open-4).
- **Done when:** any installed app is found within 1 frame of typing and launches on Enter.
- **Done:** `src/shell/spotlight.rs` (a top hit, apps, settings, the calculator, files from recent files and `plocate`
  off the UI thread) and `src/render/spotlight.rs`; its own matcher and evaluator rather than `nucleo-matcher` /
  `evalexpr`.

### E4.4 Launchpad (app grid)
- **What it is:** a full-screen grid of all apps: Launchpad.
- **How to open:** dock icon · 4-finger pinch in (E5.3) · F4 (off by default, E1.6). Close: Esc, click empty space, or pinch out.
- **Design:**
  - Wallpaper **dimmed and desaturated** (no blur), so icons stand out.
  - Search field at the top (centred, 240 px, `panel` material pill).
  - Grid **7 × 5** icons at **80 px** at 1080p (computed per screen, D13) with names (Callout, max 1 line) below;
    pages slide horizontally (2-finger horizontal scroll, ←/→ keys, click a page dot; while Launchpad is open it
    owns 3-finger swipes too).
  - Icons zoom in (scale 1.1→1, fade) on open with `smooth`.
  - Drag an icon onto the dock to pin it (from a folder too; the folder keeps it).
  - Folders: hold a dragged app over another icon for 400 ms (it grows on a glass well) and drop to make a folder,
    named from the apps' shared main category (e.g. "Utilities", else "Folder"); dropping on a folder adds the app.
    A folder tile is a glass rounded square with a 3 × 3 grid of its first apps. A click opens it as a `panel` glass
    over the dimmed grid, fading on the `smooth` spring; its grid pages (dots, wheel, swipe, ←/→) when it holds more
    than the panel's rows. Click the name to rename it; drag an app off the panel to take it out (a folder left with
    one app becomes that app); Esc or a click outside closes it. Search finds apps inside folders. Order and folders
    are saved in `$XDG_STATE_HOME/noobsbot-desktop/launchpad.json`; uninstalled apps drop out.
- **Built with:** the shared app index + icons (M3); our widgets.
- **Why:** the design calls for it (R1), and it's cheap because the app index and icons already exist.
- **Done when:** all apps show, paging and search work, and opening/closing is smooth.
- **Done:** `src/shell/launchpad.rs` (pages and dots, search, folders, `launchpad.json`, drag to the dock),
  `src/render/launchpad.rs`, the 4-finger pinch.

### E4.5 App switcher
- **What it is:** Super+Tab to switch apps.
- **Design:** centred `hud` panel, radius 18, icons **96 px**, 12 px gaps; the selected app has a rounded (radius 12)
  white @ 20% highlight and its name in Body under the strip. Super+Tab = next, Super+Shift+Tab = previous,
  Super+` = windows of the same app, Super+Q while open = quit the selected app.
  Appears only after a 150 ms hold (a quick tap switches instantly).
- **Built with:** our widgets + the window list the compositor already has (C5 benefit).
- **Done when:** order is most-recently-used; it switches across Spaces (jumps to the app's Space).
- **Done:** `src/shell/switcher.rs` (most recently used, Shift back, Q quits, H hides, pointer pick), across Spaces
  through `reveal`.

### E4.6 USB drives & devices
- **What it is:** plug in a USB drive or SD card and it just works.
- **Design:** notification "‹Drive name› connected · Open" → opens Dolphin; the drive appears in Dolphin's
  sidebar with an Eject button; a notification "Safe to remove" after ejecting.
- **Built with:** **udisks2** (mounting without root through polkit, D-Bus signals). udisks2 **does not automount by
  itself**; desktops do it. `services/disks.rs` watches `InterfacesAdded` for removable filesystems, mounts them with
  `Filesystem.Mount` (allowed for the active session by polkit's default rules) and posts the notification; Eject =
  `Unmount` + `Drive.PowerOff`, then "Safe to remove". Dolphin shows the mounted drive natively. Encrypted (LUKS) USB
  drives: Dolphin asks for the passphrase.
- **Why:** udisks2 is the standard, and Dolphin already integrates with it.
- **Done when:** plug → notification → open in Dolphin → eject → "safe to remove".
- **Done:** `src/services/disks.rs` (UDisks2: auto-mount; Eject unmounts and powers off) and `src/shell/disks.rs` (the
  "connected" and "can be removed" notifications).

### E4.7 Force Quit window & "not responding" detection
- **What it is:** a Force Quit Applications window, plus a warning when an app freezes.
- **Design:**
  ```
  ╭─────────────────────────────────────────╮
  │ ● ● ●     Force Quit Applications        │
  │  If an app doesn't respond for a while,  │  Callout, label-secondary
  │  select its name and click Force Quit.   │
  │ ╭─────────────────────────────────────╮ │
  │ │ ▣ Helium                              │ │  list rows 28 px, icon 20 px
  │ │ ▣ Kate               (not responding)│ │  red label for frozen apps
  │ │ ▣ noobsbot-terminal                  │ │
  │ ╰─────────────────────────────────────╯ │
  │                          [ Force Quit ]  │  default button (accent)
  ╰─────────────────────────────────────────╯
  ```
  - 360 × 320 px, `sheet` material, radius 12, elevation 5, centred. Opens with **Super+Alt+Esc** or logo menu ▸ Force Quit….
  - Force Quit asks the app to close (SIGTERM), then kills it after 3 s (SIGKILL) if it's still there.
  - **Not responding:** Noobsbot Desktop pings each app (`xdg_wm_base.ping`). With no answer for 5 s, the window's title bar
    dims and shows "(Not Responding)", and clicking the close button opens an alert: "‹App› is not responding.
    **Wait** / **Force Quit**".
- **Built with:** `xdg_shell` ping/pong (standard, Smithay supports it); the process id from the Wayland client
  connection; `libc::kill`.
- **Why built in:** the compositor is the only part that knows reliably which window belongs to which process,
  and it keeps working even when the system is busy.
- **Done when:** a deliberately frozen test app is flagged within 5 s and can be force-quit from both the window and the alert.
- **Done:** `src/shell/force_quit.rs` (`xdg_wm_base` pings, 5 s, SIGTERM then SIGKILL), "(Not Responding)" in the
  title, the Wait / Force Quit alert, Super+Alt+Esc and the logo menu. Pings go out with input to the app, not on a
  timer.

**M4 hardware covered:** USB storage, SD cards.
**M4 done when:** all elements pass; mockup review passed.

---

## M5 — Spaces, Mission Control, system gestures, animation polish — size L
Goal: multiple workspaces and the full multi-finger gesture experience.

### E5.1 Multiple workspaces ("Spaces")
- **What it is:** several virtual desktops, each with its own windows.
- **Design / behaviour:**
  - **Dynamic**: start with 1 Space; **+** in Mission Control adds one; ✕ on a Space thumbnail removes it
    (its windows move to the neighbouring Space). Option: fixed count (`spaces = 4`).
  - **Switch:** Super+← / →, Super+Ctrl+1…9 (Super+1…9 went to the dock, E8.3; Ctrl+arrows and Ctrl+digits stay with the apps, E1.6), **3-finger swipe left/right** (E5.3), or click a Space in Mission Control.
    The whole screen slides (spring `smooth`; follows the fingers with gestures).
  - **Move a window:** Super+Shift+← / → (the window comes with you), drag in Mission Control, or right-click title
    bar → Move to ▸ Space N.
  - **Fullscreen = its own Space** (green traffic light); exiting removes that Space. Alt+green = zoom instead.
  - **Each monitor has its own Spaces** (default).
  - Clicking a dock icon or picking an app in the switcher **jumps to the Space** where its window is.
  - **Assign app:** dock → Options ▸ This Space / All Spaces / None (the quake terminal = All Spaces).
  - **Indicator:** on switch, a small `hud` pill at the top centre with dots (current dot = `label`, others @ 30%), fades after 0.8 s.
  - Optional Space number/dots in the menu bar (off by default). Built: `[menu_bar] space_indicator = "number" | "dots"`, left of the status items; the number counts desktops as Mission Control names them (a fullscreen Space shows its app's name); dots are one per Space on that display (others @ 30%), each display its own with `[spaces] per_display`; a click opens Mission Control.
- **Built with:** `spaces.rs`: per monitor a list of Spaces, each with its own window stack and focus. Hidden Spaces
  are **not drawn** (zero GPU) but their apps keep running. `ext-workspace-v1` (`protocols/workspaces.rs`) so other tools can list Spaces and switch to one.
- **Why:** dynamic Spaces are the design (R1, R11); not drawing hidden Spaces keeps it free (R3).
- **Done when:** all behaviours above work by keyboard, gesture and mouse; switching is interruptible midway.
- **Built:** `src/spaces.rs` (dynamic or fixed Spaces, each display its own, slides that follow the fingers,
  fullscreen Spaces, jumps through `reveal`), `src/protocols/workspaces.rs`, the menu-bar indicator above; the switch
  pill fades out after 0.8 s.

### E5.2 Mission Control & App Exposé
- **What it is:** an overview of all windows and Spaces.
- **How to open:** Super+↑ · 3-finger swipe up · optional hot corner (F3 can be bound on keyboards with a dedicated
  key; it is "find next" in browsers, so it's off by default). **App Exposé** (only the current app's windows):
  Super+↓ · 3-finger swipe down.
- **Design:**
  ```
   ┌──────────────────────────────────────────────────────────────────────┐
   │   [ Desktop 1 ]  [ Desktop 2 ]  [ Helium (fullscreen) ]   [ + ]        │  Spaces bar, 96 px thumbnails
   │                                                                       │
   │        ╭─────────╮     ╭──────────────╮                               │
   │        │ window  │     │   window     │      windows spread out,      │
   │        ╰─────────╯     ╰──────────────╯      no overlap, 32 px gaps   │
   │              ╭──────────────╮                                         │
   │              │   window     │                                         │
   │              ╰──────────────╯                                         │
   └──────────────────────────────────────────────────────────────────────┘
  ```
  - Wallpaper dimmed 30%. Windows animate from their real positions into a non-overlapping grid (spring `smooth`;
    follows the fingers during the gesture).
  - Hover a window → 3 px accent outline + its title in a glass pill below it.
  - Click = focus that window and exit. Drag a window onto a Space thumbnail = move it there.
  - The Spaces bar collapses to names (Callout) and expands into thumbnails on hover.
- **Built with:** layout algorithm in `mission.rs` (pure function, unit-tested); live window textures (only while open).
- **Why:** thumbnails reuse the textures the apps already gave us, so no extra rendering happens while closed.
- **Done when:** with 10+ windows, opening and closing stays at full frame rate; layout never overlaps.
- **Done:** `src/mission.rs` (a grid that never overlaps, unit tested; the Spaces bar) and `src/render/mission.rs`
  (live thumbnails, dim, accent ring and title); Super+↑ / ↓, 3-finger swipes and hot corners open it.

### E5.3 Touchpad gestures (full system set)
- **What it is:** the multi-finger gesture set.
- **Design (all gestures follow the fingers 1:1, then finish with a velocity-based spring; a quick flick is
  enough, and a slow drag that's released halfway cancels):**
  | Gesture | Action | Milestone |
  |---|---|---|
  | 1-finger tap | click | M1 |
  | 2-finger tap / click | right-click | M1 |
  | 2-finger scroll | scroll (natural direction, configurable) | M1 |
  | 2-finger pinch | zoom inside the app (Helium, Dolphin, images) | M1 |
  | 2-finger swipe left/right | horizontal scroll; back/forward in apps that map it (Helium) | M1 |
  | 2-finger swipe right on a banner | dismiss notification (horizontal scroll over our banner) | M4 |
  | **3-finger swipe left / right** | **switch Space** | M5 |
  | **3-finger swipe up** | **Mission Control** | M5 |
  | **3-finger swipe down** | **App Exposé** (current app's windows) | M5 |
  | **4-finger pinch in** (thumb + 3 fingers) | **Launchpad** | M5 |
  | **4-finger spread out** | **Show Desktop** (windows slide to the screen edges) | M5 |
  | 3-finger drag (optional, off by default) | drag windows/text without clicking (libinput feature) | M5 |
  | 3-finger hold (optional) | reserved for later (e.g. look up) | later |
  - Option: use **4 fingers** instead of 3 for Space/Mission Control swipes (a setting), which leaves
    3-finger swipes to apps.
  - Gesture settings: sensitivity, natural scrolling, tap-to-click, finger count; in config now, Trackpad page in M7.
- **Built with:** **libinput** gesture events (swipe / pinch / hold with begin–update–end and finger count) via
  Smithay; our recogniser in `gestures.rs` (gesture → action, with velocity tracking); pinch goes to apps through the
  `pointer-gestures` protocol, 2-finger movement as normal scroll events.
- **Why:** libinput already recognises multi-finger gestures reliably on every Linux touchpad (with palm rejection).
  We only map them to actions and animate them. Following the fingers 1:1 is what makes gestures feel physical.
- **Limit:** A "swipe from the right edge with 2 fingers for Notification Center" isn't possible, because
  libinput doesn't report where fingers are on the pad. Notification Center opens by clicking the clock, Super+N or a hot corner.
- **Done when:** every gesture in the table works on a real touchpad, is interruptible, and has no false triggers
  while typing (palm rejection) or scrolling.
- **Built:** `src/gestures.rs` (the recogniser, with velocity and flicks, fed by `src/input.rs`): every row but
  the reserved hold; Space, Mission Control and App Exposé swipes, the Launchpad pinch and the Show Desktop spread
  follow the fingers (a flick or past the commit point goes on, else it springs back). 3-finger drag is libinput's own
  (`touchpad.three_finger_drag`, set in `src/backend/udev.rs`); while it's on the system swipes take 4 fingers
  (`Touchpad::effective_swipe_fingers`), untested on a real pad.

### E5.4 Hot corners (optional, off by default)
- **Design:** each screen corner can trigger: Mission Control · App Exposé · Desktop · Launchpad · Notification Center ·
  Lock Screen · nothing. 250 ms delay to avoid accidents; a small glass ripple shows the trigger.
- **Built with:** pointer position in `gestures.rs`.
- **Done when:** each action triggers reliably and never during a drag.
- **Done:** `src/gestures.rs` `Corners` (250 ms, once per visit, never while a grab runs), `[hot_corners]`, the
  ripple.

### E5.5 Window animation polish
- **Design:**
  | Event | Animation (D10) |
  |---|---|
  | Open | scale 0.92→1 + fade in, `smooth` |
  | Close | scale 1→0.95 + fade out, 150 ms |
  | Minimise | window shrinks and slides into its dock spot ("scale" effect; the genie is `[dock] minimise_effect = "genie"`, E8.4) |
  | Restore | the reverse |
  | Zoom / un-zoom | frame springs to its new size |
  | Space switch, Mission Control, Show Desktop | follow the gesture, spring to the end |
  | Reduce motion on | 150 ms crossfades only |
- **Built with:** `anim.rs` springs; the renderer draws window textures with transforms (no app redraw needed).
- **Done when:** every animation can be interrupted (e.g. grab a window mid-minimise) without glitches.
- **Built:** `src/anim.rs` (open, close, minimise into its dock spot or tile, the genie as `[dock] minimise_effect`,
  restore picking up a minimise midway, zoom springs, Show Desktop following the spread) and the Space slide
  (`src/spaces.rs`); with reduce motion, zoom and Show Desktop crossfade.

**M5 hardware covered:** multi-finger touchpad gestures.
**M5 done when:** all elements pass at full frame rate; mockup review passed.

---

## M6 — Real session: daily-drivable on a fresh Arch install — size L
Goal: install on a fresh Arch system, log in, and use it all day.

### E6.1 Real hardware backend
- **What it is:** running directly on the screen instead of inside another desktop.
- **Built with:** Smithay **udev** backend: **DRM/KMS** (talks to the GPU driver), **libinput** (all input
  devices), **libseat / logind** (safe device access without root), **GBM/EGL** (GPU buffers). VT switching
  (Ctrl+Alt+F1…F6).
- **Why:** the standard way every Wayland compositor runs; logind gives device access securely.
- **Hybrid graphics (Intel/AMD iGPU + NVIDIA dGPU laptops, the common case):** rendering happens on the iGPU; a
  monitor wired to the dGPU (many laptops' HDMI port) is driven through Smithay's **`MultiRenderer`** (buffers copied
  across GPUs). Without this the external monitor stays black or the compositor crashes on plug-in. The primary GPU is
  chosen from the boot VGA device and can be overridden in config. **Software fallback:** if EGL fails (QEMU without
  virgl, a broken driver), the pixman CPU renderer starts instead, so the system is never unusable.
- **Done when:** boots into Noobsbot Desktop on Intel, AMD and (with `nvidia-open`) NVIDIA; VT switch and back works; an
  Intel + NVIDIA laptop drives its dGPU HDMI port; QEMU without 3D acceleration still boots to a usable desktop.
- **Built:** `src/backend/udev.rs` (libseat, libinput, GBM, `DrmCompositor`, a second GPU's monitor through
  `MultiRenderer`, the GPU from `[display] gpu` else the boot VGA device, VT switching). Software fallback: with no
  driver for the GPU (simpledrm, QEMU without virgl) Mesa picks kms_swrast/llvmpipe by itself; a primary GPU whose
  GBM, EGL display, context or shell shaders fail is started once more on it (`GBM_BACKEND=dri`,
  `GBM_ALWAYS_SOFTWARE`, `LIBGL_ALWAYS_SOFTWARE`, inherited by apps). llvmpipe runs every shader, glass included;
  either way the log says "using software rendering" and a notification says graphics acceleration isn't available.
  Not yet: the pixman renderer, only for a system without Mesa (the shell's elements are all GLES: `SceneElement`
  over `GlesRenderer` with its own shader elements, ~30 files under `src/render`); fbdev-only machines (efifb, no
  DRM device); a GPU reset that won't come back still exits rather than going software. Untested on hardware.

### E6.2 Displays & multiple monitors
- **What it is:** monitors, resolution, scaling, refresh rate, Night Shift.
- **Design:** plug in a monitor → it lights up within 1 s, placed to the right (or as last arranged; layouts are
  remembered per monitor set). Each display has its own menu bar and Spaces. Per-display scale 100/125/150/175/200%.
  The highest refresh rate is chosen by default. **Night Shift** warms colours on a schedule (default 19:00–07:00; sunset/sunrise only if geoclue is enabled, E2.6).
  **Not in v1:** HDR and 10-bit colour (L11). **VRR** (adaptive sync) is a per-display toggle (`vrr`, off by default).
- **Built with:** DRM/KMS hotplug via udev; `fractional-scale` protocol; Night Shift via the GPU **gamma table**
  (zero rendering cost); `wlr-output-management` (`protocols/output_management.rs`) so external tools can read and change layouts;
  their changes are saved to `[[display.outputs]]` and applied like the Settings app's own.
- **Edge cases handled:** all monitors unplugged (zero outputs: windows and state are kept, drawn again on the next
  output; nothing is destroyed); lid closed with an external monitor = **clamshell mode** (laptop panel off, windows
  move to the external one, no sleep); a dock bringing up two monitors one after the other (layout applied once both
  are known, 500 ms settle); a monitor coming back with a different scale (remembered positions clamped, D13).
  Built: the settle (`src/backend/udev.rs` `Settle`): a DRM connector change waits until none has come for 500 ms,
  then the changed cards are scanned and the displays planned once; an unplugged display stays lit meanwhile (its
  frames go nowhere or fail and are skipped). Startup, VT return, GPU add/remove, Settings, `wlr-output-management`
  and config reload apply at once.
- **Done when:** laptop + HDMI monitor at different scales works, survives unplug/replug and sleep/wake; unplugging
  every monitor and plugging one back loses no window; clamshell mode works.
- **Done:** `src/displays.rs` (plan, clamshell, layouts per set of displays), the settle above,
  `src/protocols/output_management.rs`, fractional scale; Night Light is a final shader pass (`src/render/night.rs`),
  not the gamma table.

### E6.3 Direct scanout & idle inhibit
- **What it is:** fullscreen video and games bypass compositing; videos keep the screen awake.
- **Built with:** Smithay direct scanout; `idle-inhibit` protocol (Helium uses it during video).
- **Why:** lower power and latency (R3).
- **Done when:** fullscreen YouTube in Helium shows lower power use (`powertop`) than windowed, and the screen doesn't dim.
- **Done:** direct scanout through `DrmCompositor` planes (client buffers passed through the `MultiRenderer` wrapper)
  and idle inhibit (`src/idle.rs`). While Night Light is on, its pass keeps fullscreen apps off scanout.

### E6.4 Lock screen
- **What it is:** protects the session when you step away.
- **Design:**
  ```
  ┌─────────────────────────────────────────────────────────────┐
  │                     Saturday, 17 October                       │  Title 2, white 85%
  │                           9:41                               │  Display (96 Light), white
  │                                                               │
  │                                                               │
  │                           ( ◯ )                               │  user picture 64 px circle
  │                           Your Name                           │  Headline, white
  │                  ╭───────────────────── ➜ ╮                   │  password pill, sheet material
  │                  ╰────────────────────────╯                   │  200 × 30, radius 15
  │                                                               │
  │        Sleep   Restart   Shut Down             ⌨ EN   🔋 85%  │  bottom: Callout buttons + status
  └─────────────────────────────────────────────────────────────┘
  ```
  - Wallpaper darkened 30%. A wrong password makes the field **shake** and shows "Incorrect password".
  - Notifications on the lock screen: count only ("3 notifications"), with no content by default (privacy).
  - Locks on: Super+Ctrl+Q, the logo menu, lid close, wake from sleep, and after idle time.
- **Built with:** drawn **by the compositor itself**; while locked no app is drawn and no input reaches apps.
  Password checked with **PAM** (`pam-client` crate) through our own PAM service file `/etc/pam.d/noobsbot-desktop`
  (installed by `install.sh`; includes `pam_faillock`, so guessing is throttled like at login). `ext-session-lock` is
  also supported so external lockers work. A **watchdog** in the session wrapper (`noobsbot-desktop-session` runs the compositor under `noobsbot-desktop-watchdog`, E6.13): the compositor
  writes a heartbeat byte over a pipe once a second from an event-loop timer; once it has beaten and a 30 s startup grace is over, with no heartbeat for 10 s
  (monotonic clock, so suspend doesn't count) the watchdog kills it (SIGTERM, SIGKILL after 2 s), which
  ends the session and returns to greetd, so a hang while locked can never expose the desktop. The PAM check runs on a worker thread
  (PAM can take a second with faillock or fingerprint), so the lock screen keeps animating and typing stays responsive.
- **Why built in:** the compositor owns the screen, so it's the safest place. If Noobsbot Desktop crashes while locked, the
  session ends and you return to the login screen; it never "fails open".
- **Done when:** lock/unlock works from every trigger; no app content is visible or reachable while locked; a
  wrong password shakes the field.
- **Done:** `src/shell/lock.rs` and `src/render/lock.rs`, PAM on a worker thread (`src/pam.rs`, its own FFI rather
  than `pam-client`), ext-session-lock (`src/idle.rs`), the watchdog (`src/bin/watchdog.rs`), a logind delay inhibitor
  so it locks before sleep.

### E6.5 Idle, sleep & power
- **Design:** after idle time, the screen dims (2 min) → turns off (5 min) → sleeps (15 min on battery, never on
  AC); all configurable. Lid close = sleep. The power button shows a glass "Sleep · Restart · Shut Down" alert.
  Low battery: notifications at 20% and 10%, then sleep at 5%.
- **Built with:** idle timing in the compositor; `ext-idle-notify` for other tools; **logind** for sleep/shutdown
  and lid/power-button events (we hold a logind **`delay` inhibitor** on `PrepareForSleep`: lock the screen first, then
  let the machine sleep, so waking never shows the unlocked desktop for a frame); **UPower** for battery levels. The 5%
  action has **one owner**: `install.sh` sets `CriticalPowerAction=Suspend` (plus `AllowRiskyCriticalPowerAction=true`, without which UPower ≥ 1.90
  hibernates or powers off instead, and `PercentageCritical=10` / `PercentageAction=5`) in the drop-in
  `/etc/UPower/UPower.conf.d/50-noobsbot-desktop.conf` and we only show
  the 20% / 10% notifications (no double action). Hibernate: not in v1 (needs swap sizing at install time; L10).
- **Done when:** each step happens on time; it locks on wake; videos (idle-inhibit) keep it awake.
- **Built:** `src/idle.rs` (dim, off, lock and sleep from `[idle]`, low-battery warnings, ext-idle-notify), logind
  inhibitors and `PrepareForSleep` (`src/services/login.rs`), UPower, the power button's Sleep · Restart · Shut Down
  alert (`power-button` in `[bindings]`; Shut Down is the default and the countdown's).

### E6.6 Login screen
- **What it is:** where you log in after boot.
- **Design:** greetd + tuigreet (text, centred, clean) for M6. A **graphical glass login** that matches the
  lock screen ~~comes later (optional, M7+)~~ is built, opt-in (E7.2): greetd runs Noobsbot Desktop in its "greeter mode".
- **Built with:** **greetd** (a tiny login daemon, written in Rust) + **tuigreet**.
- **How it works:** greetd draws nothing itself. At boot systemd starts greetd → greetd starts the *greeter*
  (the login screen program) → the greeter sends the username and password over greetd's IPC socket → greetd checks
  them with **PAM** → greetd starts `noobsbot-desktop-session` (the wrapper, E6.13) as that user → on logout the greeter comes back.
- **Config** (`/etc/greetd/config.toml`, written by `install.sh`; then `systemctl enable greetd`):
  ```toml
  [terminal]
  vt = 1

  [default_session]
  command = "tuigreet --time --remember --cmd noobsbot-desktop-session"
  user = "greeter"
  ```
  Optional auto-login: an `[initial_session]` block with `user = "<owner>"` and `command = "noobsbot-desktop-session"`.
  (`noobsbot-desktop-session` is the wrapper from E6.13 that starts the current version and rolls back a broken
  update; the session `.desktop` file runs the same wrapper.)
  `install.sh` also adds `pam_gnome_keyring.so` (auth + session) to `/etc/pam.d/greetd`, so the keyring unlocks with
  the login password; otherwise Helium and Wi-Fi ask for a keyring password after every boot.
- **Security (what greetd protects and what it can't):**
  | Threat | Protection |
  |---|---|
  | Wrong / skipped password at login | PAM check; no session without the correct password |
  | Password guessing | Arch's `pam_faillock` (default: 3 failures → locked 10 min) |
  | Crashing the login screen | greeter runs as the low-privilege `greeter` user; greetd just restarts it, so no session leaks |
  | Other consoles (Ctrl+Alt+F2…) | each needs the password too |
  | **Physical access** (boot from USB, edit kernel options, remove the disk) | **no login manager can stop this** → **full-disk encryption (LUKS)**, chosen at Arch install time; **BIOS/UEFI password** + boot order locked; bootloader editing disabled (`editor no` in systemd-boot / GRUB password); optional **Secure Boot** (`sbctl`) |
  | Auto-login | **off by default**; it skips the password by design (only for desktops nobody else touches) |
  `install.sh` runs a **security check** and warns (without changing anything) if: the root disk isn't encrypted,
  bootloader editing is enabled, auto-login is on, or Secure Boot is off (informational).
- **Own greeter (E7.2, opt-in):** `noobsbot-desktop --greeter` draws a glass login (the lock screen,
  E6.4, plus a user row and session choice) and speaks greetd's small JSON IPC itself (`shell/greeter.rs`, no crate).
  `install.sh --greeter` writes `data/system/etc/greetd/noobsbot-greeter.toml` as the config; tuigreet stays the default and fallback.
- **Why:** minimal, doesn't depend on any desktop, the modern standard for lightweight setups.
  **Rejected:** SDDM (heavier), GDM (pulls in another desktop).
- **Done when:** boot → tuigreet → log in → Noobsbot Desktop starts; logging out returns to tuigreet.
- **Done:** greetd + tuigreet (`data/system/etc/greetd/config.toml`, enabled by `install.sh` with its security check);
  the graphical greeter too (`--greeter`, `src/shell/greeter.rs`, opt-in with `install.sh --greeter`).

### E6.7 Session startup & system glue
- **What happens at login:**
  1. Set the environment: `XDG_CURRENT_DESKTOP=noobsbot-desktop`, `XDG_SESSION_TYPE=wayland`, `XDG_MENU_PREFIX=arch-` (`noobsbot-` on Debian),
     `QT_QPA_PLATFORMTHEME=qt6ct`, `QT_STYLE_OVERRIDE=kvantum`, `TERMINAL=noobsbot`, `XCURSOR_THEME=WhiteSur-cursors`,
     `XCURSOR_SIZE=24`, `MOZ_ENABLE_WAYLAND=1`, `_JAVA_AWT_WM_NONREPARENTING=1`, `XMODIFIERS=@im=fcitx` (only when
     fcitx5 is installed, E6.12).
  2. Hand those to the systemd user session + D-Bus (`dbus-update-activation-environment`). **Without this,
     screen sharing and file pickers break**, a classic mistake.
  3. Start helpers: xwayland-satellite, polkit agent, gnome-keyring, portals. **Portal backend file** (installed by
     `install.sh`; required since xdg-desktop-portal 1.17 for any desktop it doesn't know, otherwise there is **no**
     file picker and no screen share at all): `/usr/share/xdg-desktop-portal/noobsbot-desktop-portals.conf`
     ```ini
     [preferred]
     default=gtk
     org.freedesktop.impl.portal.ScreenCast=wlr
     org.freedesktop.impl.portal.Screenshot=wlr
     ```
     **One password store:** gnome-keyring (Secret Service) for Helium, Wi-Fi and everything else; KWallet is not
     installed and Dolphin and Kate are told so (`~/.config/kwalletrc`: `[Wallet] Enabled=false`), so no second wallet ever
     pops up (KIO network-share passwords are then asked once per session, L9).
  4. Run **XDG autostart** entries (`~/.config/autostart/*.desktop`), so "Open at Login" apps start.
- **Session file:** `/usr/share/wayland-sessions/noobsbot-desktop.desktop` with `Exec=noobsbot-desktop-session` (the
  wrapper with watchdog and rollback, E6.13), so every login path starts the same way.
- **Done when:** after login, screen sharing, file pickers, saved passwords and autostart apps all work.
- **Done:** the session wrapper (`data/system/usr/bin/noobsbot-desktop-session`: the environment,
  `dbus-update-activation-environment`), `noobsbot-desktop-session.target`, the portals config, the session file,
  xwayland-satellite (`src/xwayland.rs`) and our polkit agent (`src/services/polkit.rs`).

### E6.8 Screenshots & screen recording/sharing
- **Design:** Super+Shift+3 = whole screen, Super+Shift+4 = drag a region (crosshair, dimmed outside, size label),
  Super+Shift+5 = toolbar (glass `hud`: Screen · Window · Region · Record · Options). A thumbnail floats at the
  bottom-right for 5 s (click = open, drag = drop into apps). Saved to `~/Pictures/Screenshots/Screenshot <date>.png`
  and copied to the clipboard. Shutter sound is optional.
- **Built with:** the compositor already has the pixels → saved with `png`; screen sharing for Helium
  (Meet, Discord) through **xdg-desktop-portal-wlr** + PipeWire, using the `wlr-screencopy` /
  `ext-image-copy-capture` protocols we implement; portal-wlr's screen chooser is `chooser_type = simple` with `slurp`
  (package `slurp`), otherwise it cannot pick a screen. **Limit:** portal-wlr shares whole screens, not single windows
  (L7). **Recording** (Super+Shift+5 ▸ Record): our own recorder pipes the compositor's frames through `ffmpeg`
  (optional package) into an mp4.
- **Why:** browsers only share screens through the portal standard.
- **Done when:** all three screenshot modes work; screen sharing works in Google Meet in Helium.
- **Done:** `src/shell/screenshot.rs` and `src/render/screenshot.rs` (screen, window and region; the toolbar; file,
  clipboard and thumbnail; recording through `ffmpeg`), wlr-screencopy for the portal (`src/capture.rs`).
  `ext-image-copy-capture` isn't served (E6.11).
- **Status:** dragging the thumbnail starts a compositor-owned Wayland drag (`text/uri-list` + `image/png`, written
  on a worker thread); it follows the pointer at half size and 60% opacity, and its 5 s timer waits. A drop an app
  takes dismisses it; a cancelled drop brings it back for 5 s; a drop on the dock goes to the dock (Trash trashes the
  file, an app icon opens it).

### E6.9 X11 app support
- **Built with:** **xwayland-satellite** (C10), started automatically.
- **Limit:** XWayland apps are scaled as bitmaps on HiDPI screens (slightly soft text); native Wayland apps are sharp (L14).
- **Done when:** an X11-only app (e.g. `xeyes`) opens with our title bar; xwayland-satellite is restarted automatically if it dies (E1.11).
- **Done:** `src/xwayland.rs` (xwayland-satellite on a free display, restarted when it dies, given up after 5 quick
  deaths with a notification).

### E6.10 Installer (`install.sh`)
- **What it is:** one script that turns a fresh Arch install into Noobsbot Desktop.
- **Does:** runs **as the normal user** (AUR packages cannot be built as root) and uses `sudo` only for `pacman` and
  system files → detects hardware (`/proc/cpuinfo`, `lspci`, `/sys/bus/thunderbolt`) → installs the package list (G4) including only the needed
  drivers → bootstraps an **AUR helper** (`paru`, built with `makepkg`) → installs `helium-browser-bin` and the WhiteSur themes
  from AUR → installs Noobsbot Desktop and noobsbot-terminal from their latest **signed GitHub Release** tarballs (minisign
  verified, E6.13), or builds from **pinned git tags** with `--from-source` → writes system files from the repo (greetd config, `/etc/pam.d/noobsbot-desktop`,
  `pam_gnome_keyring` in `/etc/pam.d/greetd`, the portals.conf, `UPower.conf`, the session `.desktop`) → enables
  services (NetworkManager, bluetooth, greetd, power-profiles-daemon, timesyncd) → sets default apps (Part F) →
  first run: `xdg-user-dirs-update`, default config, default wallpaper → runs the security check (E6.6).
  **Updating:** normally automatic (Software Update, E6.13); `install.sh --update` does the same from a terminal.
- **Why a script:** simple, readable, repeatable. An AUR `PKGBUILD` may come later (Open-3).
- **Done when:** fresh Arch in a **QEMU VM** → `install.sh` → reboot → tuigreet → Noobsbot Desktop with every feature
  working; then the same on real hardware.
- **Done:** `install.sh` (hardware detection, the AUR helper, a minisign-verified release or `--from-source`,
  `--update`, `--advanced`, `--greeter`, system files, services, defaults, the security check, the Debian path) and
  `scripts/test-install.sh`.
- **Debian 13 (trixie):** the same script, same steps, in Debian's names (G4b). Differences: `apt-get
  --no-install-recommends` (the needed Recommends are listed); Helium from its own signed apt repository; WhiteSur
  from its git install scripts (pinned commits); `xwayland-satellite` from the release tarball into `/usr/local/bin`
  (built from a pinned commit when the tarball has none); Rust from `rustup` (Debian's is too old); the files in
  `data/debian/system` replace or add to `data/system` (greetd user `_greetd` on VT 7, PAM `common-auth`, an app menu
  for Dolphin and Kate instead of `archlinux-xdg-menu`); a `helium-browser` wrapper so the dock and Super+B work as on Arch.
- **Modes:** **Auto** (recommended answers, no questions; also without a terminal and with `--update`) or
  **Advanced** (every question first, then a summary to confirm, then unattended): non-free sources (Debian), keep or
  replace another login manager, text or graphical login screen, browser, NVIDIA driver, prebuilt or source, and the
  Debian installer's network moved to NetworkManager. Answers come from `/dev/tty` and are kept in
  `~/.config/noobsbot-desktop/installer.conf` for later repairs.
- **One prebuilt per CPU, for both distros:** built on the oldest supported system (Debian 13, glibc 2.41), and
  load-tested in `archlinux:latest` and `debian:13` before it is signed (release.yml). Hardware differences are
  handled at run time (Mesa, DRM, libinput, D-Bus services). If the prebuilt doesn't fit (another CPU, a missing
  library), the installer builds that release from source. Watch Arch soname bumps of the linked libraries.

### E6.11 Privileged protocols & permissions
- **What it is:** some Wayland protocols let a client **record the screen, read every window title, inject input or
  re-arrange monitors** (`wlr-screencopy`, `ext-image-copy-capture`, `wlr-output-management`, `ext-workspace`,
  `ext-foreign-toplevel-list`, `virtual-keyboard`, `virtual-pointer`, `wlr-layer-shell`, `keyboard-shortcuts-inhibit`).
  Exposed to everyone, any app or script becomes a screen recorder or keylogger with no prompt.
- **Design:** We ask "‹App› would like to record this computer's screen". These globals are only
  advertised to **trusted clients** (the portals, xwayland-satellite, our Settings app, anything started with
  `noobsbot-desktop --trusted <cmd>`). Any other client that binds one gets a one-time **alert** (D8): "Allow ‹app› to
  record the screen / control input?" with Allow / Deny, remembered per app in config (Settings ▸ Privacy & Security, E8.2). Screen
  sharing from Helium goes through the portal's own picker, as on every desktop.
- **Built with:** Smithay's per-client global filter + the client's pid/exe from the socket credentials (`perms.rs`,
  whose table says who sees each global; all of the above but `ext-image-copy-capture` are served);
  `security-context-v1` so Flatpak apps never see the privileged set at all.
- **Why:** a desktop that owns the whole screen must not hand that control to every program; this is where Wayland is
  stricter than X11, and we keep that advantage.
- **Done when:** `wf-recorder` started from a terminal triggers the alert; a portal screen share does not; a denied
  client receives no frames.
- **Built:** `src/perms.rs` (who sees each global, the trusted socket, `--trusted`), per-client screencopy answers
  with the screen-recording alert (`src/capture.rs`, `src/shell/alert.rs`), security-context. The "control input"
  alert: virtual keyboard and pointer are offered to unsandboxed clients; an untrusted one's first virtual keyboard or
  pointer asks "Allow ‹app› to control this computer's keyboard and mouse?", its events go nowhere until allowed, the
  answer is kept per app in `[privacy] input_control` / `input_control_denied` (Settings ▸ Privacy & Security), and
  no virtual input reaches the lock screen or the login screen, even from trusted clients. Decided, not gated:
  layer-shell, output management, ext-workspace, foreign-toplevel-list and shortcuts-inhibit stay open to every
  unsandboxed client. Gating them would break ordinary bars, launchers and `wlr-randr`, unsandboxed apps already run
  as the user (with its files and its programs), and Flatpak apps never see them
  (security-context).

### E6.12 Input methods (typing Nepali, Hindi, CJK)
- **What it is:** the Noto fonts (Part H) let the desktop *show* Devanagari and CJK; this lets the owner *type* them.
- **Design:** the menu-bar keyboard item lists layouts **and** input methods; Super+Ctrl+Space cycles them (E1.6).
  The candidate popup is drawn by the input method and placed by us next to the text cursor. Emoji picker: E7.2.
- **Built with:** `text-input-v3` (app side) + `input-method-v2` (IME side) in the compositor (`ime.rs`); **fcitx5**
  (`fcitx5`, `fcitx5-m17n` for Nepali/Hindi, `fcitx5-configtool`; CJK engines on request) as an **optional** package
  set, started by autostart when installed; `XMODIFIERS=@im=fcitx` for XWayland apps only (`GTK_IM_MODULE` /
  `QT_IM_MODULE` are **not** set, so native Wayland apps use text-input-v3).
- **Why:** fcitx5 is the best-maintained Wayland IME; these two protocols are what Qt 6, GTK 4 and Chromium speak.
- **Done when:** Nepali can be typed in Kate, Helium and noobsbot-terminal with the candidate popup next to the cursor.
- **Built:** `src/ime.rs` (text-input-v3 and input-method-v2, the popup next to the cursor, layouts and fcitx5 methods
  in the keyboard menu), `src/services/fcitx.rs`, `src/ibus.rs`, `XMODIFIERS` in the session wrapper,
  Super+Ctrl+Space cycling the engine's methods while it lists several (else the layouts), `install.sh --advanced`
  offering the fcitx5 set.

### E6.13 Versioning, releases & Software Update (auto-updating)
- **What it is:** how a version number is born, how a release is built, and how every installed desktop learns about
  it and installs it, like a built-in **Software Update**. Nobody edits a version number by hand, and the owner
  never has to tell users "pull and rebuild".
- **Version scheme (SemVer, one source of truth):**
  | Part | Rule | Example |
  |---|---|---|
  | `MAJOR` | a new gem codename (1 = Opal, 2 = Pearl, …); breaking config changes | `2.0.0` |
  | `MINOR` | new features (`feat:` commits) | `1.3.0` |
  | `PATCH` | fixes only (`fix:` commits) | `1.3.1` |
  | pre-release | beta builds, `-beta.N`, visible only on the Beta channel | `1.4.0-beta.2` |
  The version lives **only** in `Cargo.toml`; `noobsbot-desktop --version` prints `1.3.0 (Opal) · 2026-10-17 · a1b2c3d`
  and About shows "Noobsbot Desktop 1.3 · Opal". The same scheme is adopted by `noobsbot-terminal`.
- **Git workflow (version control):** `main` is protected (CI must pass, no force-push); work happens on short
  branches merged by pull request; **Conventional Commits** (`feat:`, `fix:`, `perf:`, `docs:`, `feat!:` for breaking)
  are the input to the automatic version bump. Tags `v1.3.0` are created by the bot, never by hand.
- **Automatic version bump + changelog:** **release-plz** (a Rust tool) runs in CI on every push to `main`. It reads
  the commits since the last tag, computes the next version (feat → minor, fix → patch, `!` → major), bumps
  `Cargo.toml`, writes `CHANGELOG.md` and opens a **"release v1.3.0" pull request**. Merging that PR is the only human
  step: release-plz then tags `v1.3.0` and creates the GitHub Release with the changelog as release notes.
- **Release build (CI, `.github/workflows/release.yml`, on tag `v*`):** builds `noobsbot-desktop` for `x86_64`
  (`aarch64` later) with `lto = "thin"`, `strip = true`; packages
  `noobsbot-desktop-1.3.0-x86_64.tar.zst` (binary, `themes/`, `data/`, session file, PAM/portal/greetd templates) +
  `SHA256SUMS` + a **minisign** signature of it (`SHA256SUMS.minisig`; private key only in a CI secret, the public key is compiled
  into the updater). Attached to the GitHub Release. `install.sh` installs **this tarball** by default (fast, no Rust
  toolchain needed on the user's machine) and only builds from source with `--from-source`.
- **Software Update on the installed desktop (`services/update.rs`):**
  ```
  ╭──────────────────────────────────────────────────────╮
  │ ⟳  Software Update                                   │  Settings ▸ General ▸ Software Update (M7)
  │                                                       │
  │   Noobsbot Desktop 1.3 · Opal        [ Update Now ]  │  Title 3 + default button
  │   1.3.0 — 42 MB · Released 2 days ago                 │  Callout, label-secondary
  │   ▸ More Info…  (release notes, from CHANGELOG)       │
  │                                                       │
  │   (◉) Automatically keep this computer up to date     │  toggle
  │       Download in background · Install at next login  │
  │   Channel  [ Stable | Beta ]                           │  segmented control
  │   Last checked: Today, 9:41          Check Now        │
  ╰──────────────────────────────────────────────────────╯
  ```
  | Step | How |
  |---|---|
  | **Check** | once every 24 h (calloop timer; also "Check Now" and the first launch after boot) `GET https://api.github.com/repos/noobsbot-team/noobsbot-desktop/releases/latest` (Beta channel: `/releases`, pre-releases allowed). Compares SemVer with the running version. No account, no identifier, no telemetry: the request carries nothing but the version header. Off with one toggle. Fails silently when offline |
  | **Notify** | notification (E4.1): "Noobsbot Desktop 1.3 is available · *Update Tonight* / *Details*"; a red badge "1" on System Settings in the dock |
  | **Download** | in the background to `~/.cache/noobsbot-desktop/updates/`; `SHA256SUMS` + **minisign signature verified** before anything is touched; a bad signature deletes the file and shows an alert |
  | **Install** | a compositor can't replace itself while running, so the verified tarball is staged, and installed by the tiny privileged helper `noobsbot-desktop-update` through **polkit** (admin password in our alert; one polkit action `org.noobsbot.desktop.update`). It unpacks to `/usr/lib/noobsbot-desktop/versions/1.3.0/`, switches the `/usr/bin/noobsbot-desktop` symlink, keeps the previous version. Applied at the **next login** ("Install at next login", default) or now via "Log Out & Update" |
  | **Rollback** | the session wrapper `noobsbot-desktop-session` starts the current version and writes a `boot-ok` marker after 60 s of running; if a new version dies 3 times within 2 min before the marker, the wrapper starts the **previous version** and shows "Update 1.3.0 was rolled back" with a Report button (opens the GitHub issue page). Settings keeps a "Revert to 1.2.4" button for 30 days |
  | **Themes and config** | each version ships its `themes/`; the generator (E2.9) regenerates on first start of a new version; `config.toml` is migrated by `version` (C11), never rewritten destructively |
  | **noobsbot-terminal** | same updater, second repo (`noobsbot-team/noobsbot-terminal` releases), shown as a second row in Software Update |
  | **Installed from AUR instead** (Open-3) | the updater detects it (`pacman -Qo /usr/bin/noobsbot-desktop`) and only *notifies*, pointing to the package manager, so it never fights pacman |

  Done (Settings ▸ General ▸ Software Update): the version row with its status ("1.3.0 is available" / "Up to date"),
  **More Info…** (opens the release page; the notes aren't parsed), **Update Now** (the notice's "Restart to Update"
  path), **Last checked** + **Check Now** (`org.noobsbot.Desktop.Update.Check`; the shell writes
  `$XDG_STATE_HOME/noobsbot-desktop/update.json`), **Revert to x.y.z** for 30 days after an update while the older
  `versions/<v>` exists (`noobsbot-desktop-update --revert <v>`, then the log-out alert), and the red "1" on System
  Settings in the dock while an update is pending (the count of pending updates), the noobsbot-terminal row (its
  version from `noobsbot --version`; only shown with More Info…, since its own installer or package updates it), and
  the download size and release date under each version row ("12.3 MB · 27 Oct 2026").
- **Built with:** `release-plz` + GitHub Actions (CI); `ureq` (small blocking HTTP client, run on a worker thread so the
  compositor loop never blocks), `semver`, `minisign-verify`, `zstd`/`tar` crates; `polkit` action + a 100-line
  privileged helper (`src/bin/update-helper.rs` → binary `noobsbot-desktop-update`); the session wrapper is a shell
  script (`noobsbot-desktop-session`) that runs the compositor under a tiny Rust binary (`src/bin/watchdog.rs` → `noobsbot-desktop-watchdog`, shipped
  in each version dir) for the heartbeat watchdog (E6.4).
- **Why this way:** GitHub Releases is free, already where the code lives, and needs no server of ours; release-plz
  removes human version bumps and keeps the changelog honest; minisign is the smallest proven signing scheme (used for
  Zig and libsodium releases, among others), and it is what stops a hijacked download from becoming a root
  install; staged install + rollback is how mature systems avoid bricking the desktop with an update.
  **Rejected:** `git pull && cargo build` on the user's machine (needs the Rust toolchain, 5-minute builds, no
  signatures); replacing the binary live (the compositor would die); Flatpak (compositors can't be Flatpaks); only
  pacman/AUR (no repo until Open-3, and AUR builds from source anyway); our own update server (cost and upkeep);
  unsigned downloads (a MITM would get root).
- **Done when:** merging a `feat:` commit produces a release PR with the right version and changelog; merging it
  produces a tagged, signed GitHub Release; a VM running 1.2.4 shows the notification, installs 1.3.0 at next login,
  and a deliberately broken 1.3.1 is rolled back automatically.
- **Built:** release-plz and the release workflows (`release-plz.toml`, `.github/workflows/`), signed tarballs,
  `src/services/update.rs` (with `curl`, not `ureq`), `src/bin/update-helper.rs`, the watchdog and rollback in the
  session wrapper (its "was rolled back" notification has Report, which opens a new issue), the Settings page above,
  and installing at the next login without a click (opt-in, `[update] install_at_login`, Settings ▸ General ▸ Software
  Update ▸ "Install updates automatically at login"): while a verified download waits, the compositor leaves
  `install-at-login` in its state dir; the session wrapper then runs `noobsbot-desktop-update --verified` before the
  compositor starts, through a second polkit action (`org.noobsbot.desktop.update.verified`, `allow_active` = `yes`,
  picked by that first argument). That mode takes no other argument: it finds the caller's own download, checks its
  signature and refuses anything not newer, as the password path does; the new version then starts with "Updated to
  x.y.z", under the same rollback. Existing installs need `install.sh --update` once for the new action and wrapper.

**M6 hardware covered:** GPU (real), monitors, HDMI/USB-C displays, sleep/lid/power button, webcam (via portal),
phones (MTP), Thunderbolt docks (bolt), CPU microcode, video decoding.
**M6 done when:** the owner uses Noobsbot Desktop as the daily desktop for a full day with no blocker.

---

## M7 — System Settings app (+ optional extras) — size L

### E7.1 System Settings app
- **What it is:** a glass System Settings window with a sidebar. It's the only Noobsbot Desktop part that is a separate program.
- **Design:**
  ```
  ╭─────────────────────────────────────────────────────────────────────╮
  │ ● ● ●                                                                │
  │ ╭───────────────────╮                                                │
  │ │ 🔍 Search          │   Appearance                     Large Title   │
  │ │ ( ◯ ) Your Name    │                                                │
  │ │───────────────────│   ╭───────────────────────────────────────╮    │
  │ │ 📶 Wi-Fi           │   │ Appearance   [ Light | Dark | Auto ]  │    │  grouped card rows
  │ │ ᛒ  Bluetooth       │   │ Accent       ● ● ● ● ● ● ● ●          │    │
  │ │ 🔊 Sound           │   │ Glass opacity  ━━━━━━━○──  85%        │    │
  │ │ 🖥  Displays        │   ╰───────────────────────────────────────╯    │
  │ │ 🎨 Appearance  ◀   │                                                │
  │ │ ▭  Dock            │                                                │
  │ │ ⌨  Keyboard        │                                                │
  │ │ ▢  Trackpad        │                                                │
  │ │ 🕑 Date & Time     │                                                │
  │ │ ▦  Spaces          │                                                │
  │ │ 🔔 Notifications   │                                                │
  │ │ 🔒 Lock Screen     │                                                │
  │ │ ✋ Privacy         │                                                │
  │ │ ⚙  General         │                                                │
  │ │ 👤 Users           │                                                │
  │ │ ⓘ  About           │                                                │
  │ ╰───────────────────╯                                                │
  ╰─────────────────────────────────────────────────────────────────────╯
  ```
  - 720 × 560 default; sidebar 220 px in `panel` material (rows 28 px, colourful 20 px icon squircles, selected row =
    accent @ 20%); content uses grouped card rows (radius 10), with label left and control right.
  - Pages: Wi-Fi · Bluetooth · Sound (output; **input device, level, mute**; alert volume) · Displays (drag-to-arrange
    monitors, a Variable refresh rate switch on capable displays) · Appearance (mode Light / Dark / **Auto = schedule**, accent, glass opacity,
    wallpaper picker, Reduce transparency/motion) · Dock · Keyboard (layouts, repeat, shortcuts editor) ·
    **Trackpad** (each gesture with an animated preview (done: a small pad at the row's left, playing while hovered, still trails under Reduce motion), finger count, tap-to-click, natural scroll, speed) ·
    Date & Time (zone search, 12/24 h, automatic) · Spaces · Notifications (per-app allow, banner/alert, DND schedule) ·
    Lock Screen (idle times, password delay) · **Privacy** (screen-recording / input permissions, E6.11) · **General ▸ Software Update** (E6.13) · Users
    (picture, name, password) · About (computer, CPU, RAM, GPU, OS, disk, version + "Check for updates").
  - Search finds any setting.
- **Built with:** a separate Rust app. Toolkit decided at M7 (Open-2): **iced** (pure Rust, used in production apps, wgpu
  like noobsbot) is the front-runner; **slint** (lighter, built for low memory) is the alternative. Both get styled
  with the Part D tokens. It changes settings by **editing `config.toml`** (Noobsbot Desktop reloads instantly) and calls
  the system services (NetworkManager, BlueZ, timedated, …) for system-level settings.
- **Why separate:** it's big and rarely open, so it shouldn't cost RAM all day, and its bugs can't crash the desktop.
- **Done when:** every setting in Noobsbot Desktop can be changed here and applies live; looks match the mockup.
  Done: every `config.toml` key has a row (Search finds it) or its own editor (Keyboard Shortcuts for `[bindings]`), and
  applies live except `display.gpu` (next login, by nature). Left to the desktop, which edits them itself: `widgets.*`
  (drag in Notification Center), `dock.items` and `dock.assign` (the dock and its menu), and `[[rules]]` (by hand).
  Looks are not checked against the mockup.

### E7.2 Optional extras (each its own mini-plan when chosen)
| Extra | What | Built with |
|---|---|---|
| Graphical login screen (done, opt-in) | lock-screen-style login | greetd greeter mode of Noobsbot Desktop (`--greeter`, see E6.6) |
| Calendar events (done) | "Up Next" widget, event dots, reminders | own `.ics` parser (`ical.rs`, `events.rs`), `[calendar] files` (a folder not there yet is watched through its nearest parent until it appears); CalDAV later |
| Own polkit agent (done) | password prompts in our alert design instead of the fallback agent's | polkit agent D-Bus API + `polkit-agent-helper-1` (`services/polkit.rs`); the standard Qt agent is the fallback |
| Weather widget (done, off by default) | forecast in Notification Center | Open-Meteo (no key, no account; city or lat/long, no IP lookup), `services/weather.rs` |
| Fingerprint unlock (done) | lock screen: fprintd's verify beside the password field ("Touch the fingerprint sensor", red "Not recognised, try again", three tries as pam_fprintd); Settings ▸ Users ▸ Fingerprints counts, enrolls (a sheet counting touches) and deletes them; sudo is the system's PAM config (`pam_fprintd`), untouched | `fprintd` over D-Bus (`services/fingerprint.rs`, `settings/src/users.rs`); optional, not installed by `install.sh` |
| Printers (done) | printing from apps | CUPS, through Settings ▸ Printers (`settings/src/printers.rs`, E8.2; untested with a print job) |
| Desktop icons (done) | files in `~/Desktop` shown on the wallpaper (L12) | icon grid in `shell/desktop.rs`; open/rename/trash through Dolphin's KIO |
| Emoji picker (done) | Super+Ctrl+E / Super+. popover, search by name, recent row | Noto Color Emoji + our widgets; inserts via `text-input-v3` |
| Own screen recorder (done) | Super+Shift+5 ▸ Record without `wf-recorder` | compositor frames (read back only when they change, at most 30 a second) → raw RGBA on `ffmpeg`'s stdin from a thread → H.264 mp4 in `~/Videos`; the toolbar's region is cropped to an even size; no audio, no cursor (`shell/screenshot.rs`) |
| Own Finder-style file manager | replace Dolphin | big project; only if wanted |

---

## M8 — Feature parity with a full traditional desktop — size L

An audit on 2026-10-26 compared Noobsbot Desktop with a mature traditional Linux desktop, one setting, applet and key
at a time (about 30 settings modules, 36 panel applets, 880 settings keys). This milestone lists every gap that has a
sensible form in our design, as Noobsbot Desktop would draw it: same glass, same Part D tokens, same behaviour rules. A feature
with no such form is either given one or listed under "Not cloned" with the reason.

Order of work: **E8.1 first** (things that look done but aren't), then **Settings pages, the dock's per-window
features and appearance** (E8.2–E8.4), then the rest. Each E8.x gets its own mini-plan as usual.

### E8.1 Settings that do nothing today (fix first, S each)
| Gap | Where | Fix |
|---|---|---|
| ~~`[windows] double_click_title` is shown in Settings but ignored~~ | `src/decorations.rs` | Done: Zoom / Minimise / Nothing honoured |
| ~~`[windows] snapping` is on by default, has a toggle and a README line, but nothing reads it~~ | `src/grabs.rs` | Done: gates edge tiling while dragging (E8.5) |
| ~~`[keyboard] mac_keys` toggle is parsed but never applied~~ Done: the key is gone | `src/config.rs` only | implement the E1.6 swap or hide the toggle. |
| ~~Window rule `opacity` is documented but not drawn~~ | `src/render/mod.rs` | Done: fades the app's content; title bar and shadow stay solid |
| ~~Minimise from apps that draw their own title bar (GTK4, libadwaita) does nothing~~ | `src/state.rs` | Done: `minimize_request` calls `minimise` |
| ~~Trackpad tap-to-click and natural scroll apply only when the device is re-plugged~~ | `src/backend/udev.rs` | Done: `configure_inputs` re-applies trackpad and mouse settings on reload |
| ~~Green traffic light zooms; E5.1 says full screen (Alt = zoom)~~ | `src/decorations.rs` | Done: green is full screen, Alt+green zooms |
| ~~Space and Mission Control gestures fire on release instead of following the fingers (D10, E5.3); the README says they follow~~ | `src/gestures.rs` | Done: the slide and Mission Control follow the swipe, then a `gesture` spring from the fingers' speed; reduce motion acts on release |
| ~~Logo menu "System Settings…" and "About This Computer" rows are disabled~~ | `src/shell/mod.rs` | Done: open Settings at Appearance / About |
| ~~Do Not Disturb is lost on restart~~ | `src/shell/control.rs` | Done: saved as `[notifications] dnd` |
| ~~Dock, hot corners, notifications and the switcher appear on the first display only (D13 wants every display or the main one)~~ | `src/backend/winit.rs`, `src/shell/notify.rs` | Done: a dock on every display (`[dock] displays = "all" \| "main" \| "pointer"`), hot corners on every display (closed where displays meet), banners, the OSD, Force Quit, the emoji picker and the desktop icons and widgets on the main display (one output's caches and hit tests), Notification Center on the clicked bar's display, alerts on the focused window's display (else the pointer's), the on-screen keyboard on the focused text field's display (else the pointer's), the switcher on the one it opened on; a lock client's surfaces each sized to their own display, ours over every display with the password on the main one; Launchpad on the pointer's display (or its dock's), Spotlight on the focused window's (else the pointer's), the app drawer over its dock icon on that dock's display, the screenshot toolbar and region on the pointer's display (a window shot on the window's), its thumbnail on the shot's |
| ~~`show-desktop` has no default key~~ | `data/config.toml` | Done: Super+F11 |

### E8.2 System Settings pages (priority 1)
Extends E7.1. New pages: **Network, Battery, Mouse, Accessibility, Printers**. Every row writes `config.toml` or
calls the system service, and applies live.
| Page | Add |
|---|---|
| Appearance | wallpaper gallery (thumbnails of `/usr/share/backgrounds`, `~/Pictures`, "+ Add Folder") instead of the path field; Fill / Fit / Centre / Stretch / Tile; slideshow with interval and crossfade; Auto-dark hours or sunset (today fixed 19–07 in `src/render/mod.rs`); Reduce motion and Desktop icons rows; text size; pointer size; minimise effect Scale / Genie; also write the app font, monospace font and scrollbar keys for GTK and Qt apps. Built: gallery with "+ Add Folder…" (`gallery_folders`), Fill/Fit/Centre/Stretch/Tile, slideshow + crossfade, auto-dark hours or sunset, reduce motion, desktop icons, text and pointer size (Accessibility page), Scale/Genie (Dock page), Qt theming, monospace font and overlay scrollbars for GTK (gsettings) and Qt (kdeglobals, Kvantum). |
| Displays | mirroring ("Use as: Mirror for …"); identify (name label on each screen while the page is open); "Reset"; brightness slider; Night Light (designed in E6.2, not built); layouts remembered per set of monitors; clamshell (lid closed + external monitor keeps running); underscan for TVs. Built: arrangement, resolution, refresh, scale, rotation, primary, Keep/Revert countdown, Night Light; brightness slider (built-in panel, through the desktop's `org.noobsbot.Desktop.Displays.SetBrightness`); Identify (each display's name on a glass plate for 2 s, `…Displays.Identify`); Reset Arrangement (positions cleared, behind Keep/Revert); layouts remembered per set of displays (make/model/serial key, `display-layouts.json` in the state dir, restored into `[[display.outputs]]`); clamshell (libinput lid switch: panel off while another display is on, back on when the lid opens). Mirroring: `mirror = "<connector>"` on `[[display.outputs]]` and Displays › Use as (behind Keep/Revert); `plan` leaves a mirror out of the arrangement and `space` (a gone, off, self or chained source falls back to extended); on the real screen its frame is the source's `frame_elements` drawn offscreen at the source's size and shown letterboxed in its own mode on its own frame clock, redrawn when the source is damaged (nested: one window, nothing to mirror). DDC/CI brightness for monitors: `ddcutil` (installed by install.sh, with `i2c-dev`; its udev rule gives the seat user the buses) finds them at login, and the one brightness (Control Center, keys, Settings) sets every monitor too, on its own thread with a drag coalesced to its latest value; without a backlight the first monitor's reading is the slider's; monitors plugged in later are found again 2 s after the displays settle (DRM hotplug, debounced for docks) and set to the current brightness. Underscan: `underscan = 0…10` (percent) on `[[display.outputs]]` and a Displays › Underscan slider on HDMI displays; the compositor draws the frame that much smaller, centred on black (no DRM connector props, which only some drivers have), and the output scale grows by 1 / (1 − underscan) so the logical size, and the pointer, match the picture (apps draw a touch soft; untested on a TV). Not yet: a slider per monitor in Settings (its slider still shows only with a backlight). |
| Sound | output and input device pickers; live input level meter; balance; alert sound and alert volume; "Play user interface sound effects", startup sound, volume-change feedback; per-app volume in the Control Center Sound expansion. Done: output and input pickers, volume and mute; live input level meter; balance; alert sound picker with Play and `[sound] alert_volume` (the bell only); `ui_sounds` (screenshot, Trash, volume-key feedback); `startup_sound` (`desktop-login`, else `service-login`, once per login session); per-app volume rows in the Control Center; the page follows volume, mute and device changes made elsewhere (keys, Control Center, other apps) through `pactl subscribe` while it shows, 100 ms after a burst. |
| Network (new; Wi-Fi page grows into it) | network list, Known Networks with Forget and Auto-Join; Details sheet (IP, DNS, router, MAC); "Other…" for hidden and WPA-Enterprise networks; Ethernet details and static IP; VPN and WireGuard connect; proxy; captive-portal "!" icon that opens the login page; "Advanced…" opens `nm-connection-editor` until the native sheets exist. VPN stays out of the Control Center card (E2.3). Built: network list with join, Known Networks and wired connections with a Details… sheet (Auto-Join, DHCP or manual IPv4, DNS, Forget), Wi-Fi/Ethernet details, "Advanced…"; "Other…" (hidden: None, WPA2/WPA3 Personal, WPA2 Enterprise PEAP/TTLS with MSCHAPv2/PAP and an optional CA file); VPN and WireGuard connect in Settings and in the Control Center's Wi-Fi list (the task asked for it there, against E2.3), WireGuard `.conf` import; `[network] proxy` (manual env for apps we start, manual or PAC in GTK's proxy settings); captive portal (NetworkManager connectivity PORTAL; nothing while its check is off): a notification once per portal and a "!" on the menu-bar network item, whose menu has "Sign in to network…", both opening the check URL (or http://nmcheck.gnome.org); "Other…" also offers WPA3 Personal (`sae`); WireGuard import keeps IPv6 addresses and DNS (`ipv6.address-data`, `ipv6.dns`); the Details sheet has Configure IPv6 (automatic or manual address, prefix, router, DNS); WireGuard import keeps `DNS =` names that aren't addresses as search domains (`ipv4.dns-search`, or `ipv6.dns-search` when only IPv6 has addresses); the page is called Network in the sidebar (`--page network`, the menu bar's "Network Settings…"; `wi-fi` and `wifi` still open it); while it shows, Wi-Fi rescans on open and every 10 s and the list is re-read 4 s after each. |
| Bluetooth | Nearby Devices (discovery) with Connect; per-device ⓘ ▸ Forget, Rename (the pairing agent and alerts already exist). Done: Nearby Devices with Connect, Connect/Disconnect, Forget, Rename (BlueZ `Alias`). |
| Battery (new) | show percentage; time remaining and power source in the battery menu; Low Power Mode; dim on battery; display off and sleep times for battery and for power adapter (today `[idle]` in the config file only); keyboard backlight; low-battery warnings for wireless mouse and keyboard. Built: page with level, power source, time remaining, power mode (Low Power), sleep and display off on battery / adapter; low-battery notification at 20% and 10%; keyboard backlight keys, brightness slider and off after inactivity; show percentage; battery menu with time left, power source and peripherals' batteries (also in the Batteries widget); dim on battery; low-battery notifications at 20% and 10% for wireless mice, keyboards and headsets, one per device. Done. |
| Keyboard | key repeat delay and rate sliders with an Off stop (now constants in `src/input.rs`, sent live with `set_repeat_info`); Input Sources list with a searchable picker, reorder, IMEs; "previous" and "n-th source" actions; remember the source per window; Modifier Keys dropdowns (Caps Lock, Compose, Ctrl/Alt swap) plus a raw options field; Keyboard Viewer; Keyboard Shortcuts dialog (E8.7). Done: repeat sliders (rate 0 = off), Input Sources with searchable picker and ↑/↓ reorder, fcitx5 input methods listed and added/removed through its controller (`SetInputMethodGroupInfo`; a note while fcitx5 isn't running), ibus as the other engine (`[keyboard] input_method = "fcitx5" | "ibus" | "auto" | "none"`, a dropdown in Input Sources: the compositor starts `ibus start --type wayland` and gives apps `XMODIFIERS=@im=ibus`; its engines listed with `ibus list-engine`, added through `preload-engines`, switched with `ibus engine`; untested live), Caps Lock / Compose / Control and Alt (`ctrl:swap_lalt_lctl`) / raw options, Keyboard Shortcuts dialog, next / previous (Super+Ctrl+Shift+Space) / `input-source 1…9` (unbound) actions, `[keyboard] per_window_source`, Keyboard Viewer (input menu; the on-screen keyboard read-only, held keys lit). |
| Mouse (new, `[mouse]`) | tracking speed; acceleration on/off; primary button left/right; natural scroll (separate from the trackpad); double-click speed (also used by our own title-bar double-click); middle-click emulation; middle-click paste on/off. Done: speed, acceleration, left-handed, natural scroll, middle-click emulation, double-click speed (title bars, edges, desktop icons, GTK), middle-click paste (primary selection withheld, GTK follows). |
| Trackpad | tracking speed; secondary click (two fingers / bottom corners / off); tap to drag with drag lock; ignore trackpad while a mouse is plugged in; disable while typing; scroll method; each gesture with on/off; hot corners. Done: speed, secondary click (incl. Off), tap to drag, drag lock, ignore with mouse, disable while typing, scroll method, a toggle per gesture; hot corners rows on Desktop & Dock (with a new `display-off` action). |
| Desktop & Dock | dock click, middle-click and scroll actions (E8.3); double-click title bar action; minimise effect; **default apps** (browser, mail, music, video, photos, text, PDF, file manager) written to `mimeapps.list`, and Super+B / Super+E follow them. Done: click / middle / scroll actions, double-click title, minimise effect; `default-app` keys open the MIME default; Default Apps rows (and Terminal) writing `mimeapps.list`. |
| Date & Time | "Set time and date automatically" (timedated `SetNTP`); manual date and time; searchable time-zone list instead of the text field; week numbers; first weekday from the locale. Done: NTP toggle (timedated), time-zone search that also sets the system's, manual date and time (`SetTime`), `[clock] week_numbers`, `week_start = "auto"` (LC_TIME). |
| Notifications | per-app allow, banner or alert, sound, show on lock screen; previews Always / When Unlocked / Never; DND schedule; show while full screen. Built: DND row; per-app rows (Allow, Banners / Alerts / None, sound, badge, show in Notification Center) writing `[notification_apps]`; previews; DND schedule; full-screen option; per-app "Show on lock screen" (`lock`, off by default): allowed apps' latest notifications listed under the clock (app and title; body only with previews Always), the rest counted. Done. |
| Lock Screen | the `[idle]` timers and lock toggle; password delay (immediately … 1 h); message on the lock screen. Done: dim, display off, lock toggle, password delay (immediately … 1 h), lock message. |
| Privacy & Security | recent files: remember, how long, Clear; connectivity check; camera and microphone per app; empty Trash after 30 days. Built: recent files Clear and age (General page); Privacy & Security page with NetworkManager's connectivity check (off also stops sign-in page detection) and the apps in the portal permission store's `devices` table (camera, microphone, speakers), each with Allow; Screen Recording lists `[privacy] screen_recording` and `screen_recording_denied`, each program with Remove (it asks again next time). Done. |
| General | **Login Items** (`~/.config/autostart` and system entries, toggle with `Hidden=true`, + app or command, −); Storage (low-disk warning, Trash and thumbnail cleanup); About ▸ copy System Report. Done: Login Items, Storage (Trash and recent-files ages; low-disk warning in the desktop), Software Update, reopen at login, About ▸ Copy System Report. |
| Users | name, picture (stock, file, camera) and password through AccountsService and our polkit agent. Done: Users page (`settings/src/users.rs`) with the account's picture, full name and login; full name (`SetRealName`); picture from the wallpaper gallery or a file from the XDG portal's file chooser (`FileChooser.OpenFile`, `settings/src/chooser.rs`; the same chooser serves "+ Add Folder…" and VPN ▸ Import WireGuard; a path field only when there is no portal), cut to a 256 px `~/.face` (`SetIconFile`); Change Password… through `passwd` on a pipe (not `SetPassword`); other users listed (`ListCachedUsers`); Add User… sheet (full name, account name suggested from it and checked against `/etc/passwd`, password, Standard or Administrator) through `CreateUser`, then `SetPassword` with a SHA-512 hash from `openssl passwd -6` (`passwd` only changes one's own); Delete User… on other users, an alert with Keep or Delete Home Folder (`DeleteUser`); polkit denials and D-Bus errors show inline. Automatic login left out: the login screen is greetd, whose `initial_session` ignores AccountsService. Take Photo… (shown with `ffmpeg` and a camera: the first `/dev/videoN` whose sysfs `index` is 0 and whose name has no "IR"): a 3-2-1 countdown, one still from `ffmpeg -f v4l2` a second in, then Retake or Use Photo, which goes through the same `~/.face` cut and `SetIconFile`; no live preview. |
| Accessibility (new) | everything in E8.6 |
| Printers | CUPS, printer list, queue, Add Printer; print-queue icon in the dock while printing. Built through CUPS's own tools (`settings/src/printers.rs`): queues with Idle / Printing / Stopped and the default (`lpstat -p -d`); Options… with Set as Default (`lpoptions -d`), Print Test Page, the jobs with Cancel (`lpstat -o`, `cancel`) and Remove Printer (`lpadmin -x`, again through pkexec when cupsd refuses); Add Printer… lists `lpinfo -v`'s devices off the UI thread, or takes an address, and adds them driverless (`-m everywhere`; PPD drivers left to the CUPS web page); a note without CUPS. The dock shows a `printer` icon among the running apps while `lpstat -o` has jobs (every 5 s, only while cupsd runs); a click opens Settings ▸ Printers. install.sh adds `cups` and enables `cups.socket`. Untested with a print job. |

### E8.3 Dock: the per-window jobs of a window list (priority 1)
The dock stays per app, by design. Every per-window job a taskbar does gets a surface of our own instead:
- ~~**Window previews:** hover (or long-press) an icon with several windows → a glass strip of live thumbnails above it,
  click to focus, × or middle-click to close, hovering a thumbnail fades the other windows. Reuses Mission Control's
  thumbnails and `close_button`.~~ Done: hover an icon with several windows; ✕ or middle-click closes, the hovered
  window's surroundings dim (`src/shell/dock.rs` `Peek`, `src/render/dock.rs`); a still press held 0.5 s shows them too.
- **Minimised windows** as thumbnail tiles after a separator at the dock's right end (E3); the minimise animation
  lands on the tile. Built; `[dock] minimise_into_icon` turns them off.
- ~~**One dock per display** with its own filtering (D13, E3).~~ Done: `[dock] displays` (all / main / pointer); each dock
  has its own hide state and zoom, shares pins, badges, dots and the Trash, and with `current_workspace_only` shows its
  own display's current Space. Pins stay one list for every dock.
- ~~**Super+1…9** focuses, cycles or opens the n-th dock app, Super+Shift+n opens a new window, and holding Super shows
  number pills over the icons. Space keys move to Super+Ctrl+1…9, so Ctrl+1…9 stays with apps (browser tabs).~~ Done:
  `dock-app` / `dock-app-new` actions, number pills, Spaces on Super+Ctrl+1…9.
- **Right-click menu:** Options ▸ Assign To (All Spaces / This Space / None), Move to Display, Open at Login; Close
  Others; recent files of the app (`recently-used.xbel`); ◆ next to minimised windows. Built: Open at Login, Keep in
  Dock, Show in Files, recent files, ◆, Close / Close All, and an Options submenu whose Assign To (`[dock] assign`)
  and Move to Display open their own submenus; Close Others.
- ~~**Spring-loading:** a file dragged over an icon brings that app's window up (App Exposé when it has several).~~
  Done: held 0.6 s (`src/shell/dock.rs` `Spring`).
- ~~**Config:** `[dock] click = "toggle" | "cycle"`, `middle = "new" | "close"`, scroll on / off / reverse, Shift-click
  new window, cycle across apps.~~ Done: `click`, `middle`, `scroll`, Shift/Ctrl-click new window,
  `cycle_across_apps` (click / scroll cycling walks on into the next dock app), with Settings rows.
- **Later, if asked:** `[dock] mode = "windows"`, one icon per window with the title as tooltip.

### E8.4 Appearance and themes (priority 1)
Rows live on the Appearance page (E8.2). Behind them:
- Wallpaper: gallery, fit modes, slideshow and crossfade in the wallpaper renderer (built: `[appearance] wallpaper_folder`, `wallpaper_interval`, `wallpaper_shuffle`; 1 s crossfade on every change).
- ~~Auto-dark from configurable hours or sunset (needs the location the user typed for weather; no geoclue, E2.6).~~
  Done: `[appearance] dark_from`, `dark_until`, `auto_sunset` (`src/sun.rs`, from the weather place).
- ~~Night Light as an output colour-temperature shader with schedule, as designed in E6.2.~~ Done: `[night_light]`
  (custom hours or sunset, temperature), a final multiply pass fading over a minute (`src/render/night.rs`); Settings
  rows on Displays.
- Text size and pointer size applied to the shell and exported to apps and XWayland (`XCURSOR_SIZE` today). Done: the whole shell layout grows with `text_scale` (`text::ts`), apps get GTK `text-scaling-factor`, the Qt font size and `XCURSOR_SIZE`.
- ~~Genie minimise as the alternative to Scale (E5.5).~~ Done: `[dock] minimise_effect = "genie"` (`src/render/genie.frag`).
- Our fonts, colour scheme and accent written to GTK and Qt settings so apps follow. Built: `noobsbot-desktop theme
  --write` (Kvantum, Qt colour scheme with accent, kdeglobals font; gsettings colour scheme, theme, Inter, text and
  cursor size, monospace font, overlay scrolling; the accent as `@define-color` in gtk-3.0 / gtk-4.0 `gtk.css`).

### E8.5 Window management
- ~~**Tiling:** drag to an edge for a half, a corner for a quarter, the top edge to fill; preview after a
  300 ms hold; dragging out restores the old size (E1.2). Keyboard: Super+Ctrl+arrows tile (replaces the unbuilt
  nudge keys in E1.2).~~ Done: `wm::tile_at` in the move grab behind `[windows] snapping` (Ctrl skips it); the preview
  shows on arrival, and a side edge held 700 ms switches Space instead; `untile` restores; Super+Ctrl+arrows.
- ~~**Window menu:** right-click the title bar, or the app asks (`show_window_menu`): Minimize, Zoom, Fill, Center,
  Move & Resize ▸ halves and quarters, Full Screen, Keep on Top, Move to ▸ Space n / Display, Close.~~ Done:
  `src/decorations.rs` window menu, also from `show_window_menu`.
- ~~**Sheets:** modal dialogs attach under the parent's title bar and move with it (E1.2 stacking).~~ Done: `src/sheets.rs`.
- ~~**Actions** for bindings: `zoom`, `fullscreen` (Super+Ctrl+F), `keep-on-top`, `center`, `move-to-display`,
  `move-to-space <n>`, `move-to-new-space`, `cycle-app-windows` (Super+`, E4.5).~~ Done: all in `src/shortcuts.rs`.
- ~~Each app's window frame remembered (E1.2).~~ Done: `window-frames` in the state dir (`src/sheets.rs`).
- ~~Switcher: pick with hover and click; H hides the selected app; centred on the active display.~~ Done: `src/shell/switcher.rs`.
- ~~Optional: Super + right-drag resizes; focus follows mouse as a hidden `[windows] focus = "hover"`.~~ Done: both
  (`src/input.rs`).

### E8.6 Accessibility
- **Zoom** (full screen and picture-in-picture), with invert and greyscale filters; keys Super+Alt+8 / = / −. Built
  (`[accessibility] zoom_style`, `colour_filter`), following the text caret and keyboard focus (`zoom_follows_focus`).
- **Keyboard:** sticky keys, slow keys, bounce keys, mouse keys (in `src/input.rs`). Built: `[accessibility] sticky_keys`,
  `sticky_keys_shortcut` (Shift ×5), `slow_keys_ms`, `bounce_keys_ms`, `mouse_keys`.
- ~~**Pointer:** dwell click, hold for secondary click, pointer size, shake to find the pointer.~~ Done:
  `[accessibility] dwell_click_ms`, `hold_secondary_ms`, `shake_to_find` (`src/pointer_assist.rs`), `[appearance] pointer_size`.
- **Display:** increase contrast, larger text, reduce transparency (exists), reduce motion (exists). Built:
  `[accessibility] increase_contrast`, glass opacity, reduce motion, colour filters; larger text is the text-size work.
- **Alerts:** visual and audible bell (`xdg_system_bell`), Caps Lock indicator. Built: `[accessibility] bell`, a Caps Lock
  OSD and ⇪ in the lock screen's field.
- ~~**On-screen keyboard** (`virtual_keyboard` protocol), also on the lock screen.~~ Done: our own glass panel
  (`src/shell/osk.rs`, `on-screen-keyboard` action, `[accessibility] on_screen_keyboard`) typing through the seat, so the
  lock screen gets it too; `zwp_virtual_keyboard_v1` is served for other keyboards.
- ~~**Screen reader** path: AT-SPI for our shell widgets and an Orca key monitor. Size L.~~ Done: menu bar, dock and
  Control Center over AT-SPI (`src/access.rs`), the `KeyboardMonitor` key monitor (`src/services/a11y.rs`),
  `[accessibility] screen_reader`; also Spotlight, Launchpad, banners (critical ones assertive) and Notification
  Center, alerts, polkit and Force Quit, the app switcher and the lock screen.

### E8.7 Spaces, gestures, keys and shortcuts
- Spaces and Mission Control on every display; move a window to the next Space by dragging it against the edge;
  reorder Spaces; Mission Control keyboard navigation; Mission Control view of every Space. Done: each display has its own Spaces (`[spaces] per_display`), keys and swipes act on the display under the pointer, Mission Control on every display.
- ~~Keyboard Shortcuts dialog in our form: category sidebar, record keys by pressing them, + / −, Restore Defaults,
  ⚠ on conflicts (today the first match wins silently).~~ Done: Settings ▸ Keyboard ▸ Keyboard Shortcuts…
  (`settings/src/shortcuts.rs`); recording asks the desktop to pass every key through; conflicts are also logged.
- Default keys: log out (Super+Shift+Q), sleep key, keyboard backlight (UPower), touchpad toggle, display key
  (Settings › Displays, where mirroring lives), calculator / mail / web / explorer / search keys (default-app,
  Spotlight), media stop / seek / eject, Shift+Alt+volume quarter steps, Alt+Print window screenshot, Super+Alt+5
  screen recording. Done.
- Lock screen lets volume, brightness and media keys through; shows Now Playing; switches keyboard layout. Done.

### E8.8 Menu bar, tray, widgets and session
- ~~Tray: real submenus and an overflow "»" menu.~~ Done: submenus open beside their row at any depth (after 200 ms of rest or at once on a click; each goes the way its parent went, flipped at the screen edge and kept on it, `shell::submenu_x`), each tray level asked for with AboutToShow. Every shell menu takes the keyboard (arrows skip dead rows, Return/Right run or open, Left/Esc back out one level, letters jump, Left/Right walk the bar menus); a diagonal move toward an open submenu keeps it (aim triangle, 300 ms); the window menu's Move to lists the other displays by name.
- ~~Menu bar on every display; auto-hide option; a Recent Items menu with Clear.~~ Done: `src/backend/winit.rs`, `src/state.rs`, `src/shell/mod.rs`.
- ~~Trash at the dock's right end; Launchpad folders and drag to rearrange.~~ Done: Trash after the separator (Open, Empty Trash,
  drop to trash, `[dock] show_trash`); Launchpad icons drag to reorder and onto each other to make folders (saved in
  `launchpad.json`).
- ~~Widgets for Notification Center and the desktop as separate processes over D-Bus (no plugins in the compositor).~~
  Done: `org.noobsbot.Desktop.Widgets` (`src/services/widgets.rs`), placed by `[widgets] center` / `desktop`; one
  widget app ships: `system` (CPU, memory, disk; `src/bin/widget-system.rs`), started by the shell at login.
- ~~Keep-awake Control Center tile with the list of apps blocking sleep~~ (done: idle-inhibit windows and logind `block`
  inhibitors; on until logout).
- ~~Reopen windows at login (checkbox in the Log Out alert).~~ Done: `session.toml` in the state dir; each app starts once 3 s after login and its windows go back to their Space, display and frame as they map (20 s wait).
- ~~Thunderbolt "Allow accessory to connect?" alert through `boltd` (G2).~~ Done: a device boltd lists as "connected" asks Don't Allow / Allow (queued while locked); Allow enrolls it with policy "auto". Real sessions only.
- ~~Desktop icons for mounted drives with Eject.~~ Done: drives sit first at the top right (not draggable); double-click opens the file manager, right-click has Open and Eject; `[desktop] show_drives`.
- ~~Housekeeping: low-disk notification, Trash and thumbnail cleanup, recent-files age.~~ Done: `/` and `$HOME` checked
  hourly (under 5% or 2 GB: one notification until it recovers); once a day on a thread: `[housekeeping]` Trash and
  recent-files ages, thumbnails unused for 90 days.

### Not cloned (no fitting form, or not worth it)
Window shading; title-bar scroll opacity; button-layout and title-bar font choices (fixed by D2, D4); notifications at
the bottom; free-form clock and lock-screen formats; a sound file per event; several switcher styles; pointer warping;
volume above 100%; peek at the desktop with blur (D3); lockdown policies; an add-on store for applets and desklets
(widgets over D-Bus instead, E8.8); X11-only options; "turn all networking off".

- **Done when:** every row above is built, or moved to "Not cloned" with its reason, and each new page matches its
  mockup (D12).

---

# Part F — Apps that ship with Noobsbot Desktop

## F1. Terminal → **noobsbot-terminal** (default) — owner's own
- **What:** the owner's GPU-rendered terminal (Rust, winit + wgpu), repo `noobsbot-team/noobsbot-terminal`, command `noobsbot`;
  tabs, splits, suggestions, quake mode.
- **Why:** owner requirement (R5). It's also native Wayland, fast (GPU) and written in the same language.
- **Made the default everywhere** (`install.sh` sets every one of these, because different apps look in different places):
  | Who opens a terminal | Where it looks | Setting written |
  |---|---|---|
  | Noobsbot Desktop (Super+Enter, dock, Spotlight, quake F12) | our config | `terminal = "noobsbot"` in `config.toml` |
  | Apps with `Terminal=true` in their `.desktop` file (e.g. btop, nano from Spotlight/Launchpad) | our launcher | started inside `noobsbot -e <command>` |
  | GTK/GLib apps and the freedesktop standard | `xdg-terminal-exec` | `~/.config/xdg-terminals.list` → `noobsbot.desktop`; package `xdg-terminal-exec` |
  | **Dolphin "Open Terminal Here" (Shift+F4), Kate, other Qt apps** | their own setting, *not* the standard | `~/.config/kdeglobals`: `[General] TerminalApplication=noobsbot` and `TerminalService=noobsbot.desktop` |
  | Scripts and CLI tools | `$TERMINAL` environment variable | `TERMINAL=noobsbot` in the session environment (E6.7) |
  | Right-click → Open With for shell scripts | MIME default | `xdg-mime default noobsbot.desktop application/x-shellscript` |
  - **Limit:** Dolphin's and Kate's *built-in* terminal panels (F4) embed Konsole's component, which noobsbot can't
    replace. We don't install Konsole, so those panels are hidden; "Open Terminal Here" opens noobsbot instead.
- **Quake mode:** F12 (E2.8).
- **Installed by:** `install.sh`, from its signed GitHub Release (or from source with `--from-source`); updated by
  Software Update (E6.13). This needs the noobsbot-terminal repo to adopt the same release pipeline (release-plz +
  signed tarballs) before M6.

## F2. Browser → **Helium** (default) — https://helium.computer
- **Why:** owner requirement (R6, changed from Brave on 2026-10-18). Helium (by imput, GPL-3.0) is Chromium-based
  (ungoogled-chromium, no Google services, uBlock Origin built in): native Wayland, hardware video decoding, screen
  sharing via portals, MPRIS, idle-inhibit — everything the plan relied on Helium for.
- **Integration:** flags `--ozone-platform=wayland --password-store=gnome-libsecret` (Chromium only auto-detects
  the keyring on a few known desktops; on an unknown desktop it would store passwords **unencrypted**), written to
  `~/.config/helium-browser-flags.conf`, which the `helium-browser-bin` launcher (`helium-browser`) reads, so **every**
  launch path (dock, Spotlight, `xdg-open` links from other apps) gets them; set as the system default
  (`xdg-mime default helium.desktop` for http/https/html/xhtml, which is what `xdg-settings` does on a desktop it doesn't know); passwords in gnome-keyring.
  Its Wayland app_id is checked in M6 (expected `helium`); the picture-in-picture rule matches the title only, so it
  works whatever the id is.
- **Look: kept as it is.** Noobsbot Desktop does **not** theme Helium: it keeps its own theme, its own tab-strip
  title bar (with its own window buttons) and its own light/dark setting. The window still gets our rounded corners,
  shadow, dock icon, Mission Control and gestures, like every other window.
- **Installed by:** `helium-browser-bin` (AUR; official signed release tarballs from github.com/imputnet/helium-linux).
  Updates come with the AUR package (Helium's own auto-update doesn't cover Linux).

## F3. File manager → **Dolphin**
- **Why:** owner choice (R7). Fast; tabs, split view, previews; network shares, phones (MTP) and archives via KIO.
- **Extras:** `kio-extras` (thumbnails, MTP, network), `kdegraphics-thumbnailers` + `ffmpegthumbs` (PDF and video
  previews), `kimageformats` + `qt6-imageformats` (WebP, HEIC, AVIF), `kio-admin` (edit system files behind a polkit
  prompt), `ark` (zip/tar/7z), `archlinux-xdg-menu` (without it, "Open With" is **empty** here).
- **Passwords:** KWallet is **not** installed; gnome-keyring is the one store (E6.7). Network-share passwords are asked
  once per session in Dolphin (L9).
- **Look:** Noobsbot Glass (E2.9).
- **Cost:** Qt framework libraries (~300 MB disk), with **no RAM use until Dolphin opens**.

## F4. Text editor → **Kate** (GUI) + **nano** (terminal)
- **Why Kate:** the same Qt family as Dolphin (shared libraries, consistent look), with syntax highlighting,
  tabs and a file tree. **Why nano:** the simple terminal editor anyone can use.
- **Rejected:** gedit and other GTK editors (pull GTK libraries alongside Qt, mixed look); VS Code (heavy).

## F5. Disk management → **Partition Manager** + **Filelight**, with disk info in Dolphin and Settings
- **What it is:** everything about drives: see disks and partitions, format a USB stick, create, resize or delete
  partitions, check drive health, and see what's using space.
- **Design / where you find it:**
  | Task | Where | Tool |
  |---|---|---|
  | Free space at a glance | Dolphin status bar + sidebar; Settings ▸ About ▸ **Storage** bar (M7, coloured usage bar) | udisks2 |
  | Format a USB stick / SD card (exFAT, FAT32, NTFS, ext4) | right-click the drive in Dolphin → Format, or Partition Manager | Partition Manager |
  | Create / resize / move / delete partitions, change labels, mount points | **Partition Manager** (Spotlight: "Disk") | Partition Manager |
  | Drive health (SMART: temperature, bad sectors, hours used) | Partition Manager ▸ SMART Status; low-health notification (E4.1) | smartmontools |
  | What's filling the disk | **Filelight** (sunburst chart of folders); "Open in Filelight" in Dolphin | Filelight |
  | Mount / eject USB drives | automatic (E4.6) | udisks2 |
  | "Disk almost full" warning | notification at 90% and 95% used on `/` and `/home` | our code (udisks2 / `statvfs`) |
- **Built with:** `partitionmanager` (Qt, uses kpmcore; asks for the admin password through polkit only when changing
  disks), `filelight`, `smartmontools`, filesystem tools `exfatprogs`, `dosfstools` (FAT32), `ntfs-3g` (NTFS),
  `btrfs-progs`, `e2fsprogs` (ext4). The disk-full warning lives in Noobsbot Desktop's `services/disks.rs`.
- **Why these:** Partition Manager and Filelight are **Qt apps like Dolphin and Kate**, so they share the libraries
  already installed (only ~8 MB extra) and get the same Noobsbot Glass theme (E2.9). Partition Manager does everything
  a disk utility should, plus resizing. Filelight is the clearest "what uses my space" view.
  **Rejected:** GTK disk tools (GTK4/libadwaita: extra GTK libraries, its own title bar, see L2); GParted (GTK3, runs
  as root with X11, poor fit on Wayland).
- **Milestone:** packages installed in M6 (`install.sh`); disk-full warning M4 (with notifications); Storage bar M7.
- **Done when:** a USB stick can be formatted from Dolphin, partitions can be resized, SMART health shows, and
  filling a test disk past 90% raises the warning.

## F6. Resource monitor → **System Monitor** (`plasma-systemmonitor`) + **btop** + Noobsbot Desktop's Force Quit window
- **What it is:** see what's using CPU, memory, GPU, disk and network, and stop apps that hang (an Activity Monitor
  + Force Quit).
- **Design / where you find it:**
  | Need | Where | Tool |
  |---|---|---|
  | Full monitor: processes, CPU/RAM/GPU/disk/network graphs, end a process | **System Monitor** app (Spotlight: "Activity"), shortcut **Ctrl+Shift+Esc** | plasma-systemmonitor |
  | Quick monitor in the terminal | `btop` in noobsbot-terminal | btop |
  | Stop a frozen app | logo menu ▸ **Force Quit…** or **Super+Alt+Esc** → Noobsbot Desktop's own Force Quit window | Noobsbot Desktop (E4.7) |
  | "App not responding" | when a window stops answering for 5 s: its title dims and an alert offers **Wait / Force Quit** | Noobsbot Desktop (E4.7) |
  | Glance at load (optional) | Notification Center **System** widget: CPU %, memory, disk free, network up/down | Noobsbot Desktop (E4.2) |
- **Built with:** `plasma-systemmonitor` (the Qt System Monitor; brings `ksystemstats` for the data); `btop`; Noobsbot Desktop
  reads `/proc` and `/sys` itself for the widget and Force Quit (no extra service).
- **Why these:** System Monitor is the one the owner already keeps in the dock today (dock2), it's Qt like the
  other apps (same look, shared libraries), and it covers everything Activity Monitor does, including GPU.
  btop is tiny and great in the terminal. Force Quit is built into Noobsbot Desktop because the compositor already knows
  every window and its process. It works even when everything else is busy, and looks like the rest of the desktop.
  **Rejected:** Mission Center (good, but GTK4/libadwaita: extra libraries and its own title bar); GTK system
  monitors (another desktop's look); htop (btop is nicer and still small).
- **Milestone:** apps installed in M6; Force Quit + "not responding" in M4; System widget M4 (optional).

## F7. Optional (owner can add)
Image viewer (gwenview), video player (mpv), PDF viewer (okular), office, email. The desktop works without them.

---

# Part G — System layer: hardware, drivers & services

## G1. Why we write no drivers
Drivers live **inside the Linux kernel** and device **firmware** comes from `linux-firmware`. If a device works on
Linux, it works on Noobsbot Desktop. Our job is the standard **services** above the drivers plus a glass UI for each.
`install.sh` detects hardware and installs only what that machine needs.

## G2. Hardware table
| Hardware | Kernel driver + firmware | Service | Noobsbot Desktop UI | Milestone |
|---|---|---|---|---|
| CPU | built in; `intel-ucode` / `amd-ucode` | — | About | M6 |
| Display / GPU | Intel `i915`/`xe`, AMD `amdgpu`, NVIDIA `nvidia-open` + `nvidia-utils`; `mesa`, `vulkan-intel`/`vulkan-radeon` | Noobsbot Desktop (DRM/KMS) | Displays settings, Night Shift | M6 (UI M7) |
| External monitors | same GPU drivers | Noobsbot Desktop hotplug | auto-arrange, remembered layouts | M6 |
| Brightness | `backlight` | logind | Control Center slider, keys, OSD | M2 |
| Video decoding | `intel-media-driver` / `mesa` (AMD VA-API is part of mesa) / NVIDIA VA-API | — | none (Helium uses less CPU) | M6 |
| Wi-Fi | `iwlwifi` (Intel), Realtek/MediaTek/Atheros in kernel; Broadcom → `broadcom-wl-dkms` (only the chips the kernel can't run) | NetworkManager | menu bar + Control Center | M2 |
| Ethernet | in kernel | NetworkManager | network menu | M2 |
| Bluetooth | `btusb` + firmware | BlueZ | Control Center, pairing alert | M2 |
| Sound | `snd_hda_intel`, Intel SOF → `sof-firmware`, `alsa-ucm-conf` | PipeWire + WirePlumber | volume, output picker, OSD | M2 |
| Bluetooth audio / AirPods | BlueZ | PipeWire (built in) | output picker, battery widget | M2 / M4 |
| Keyboard | `usbhid` / `i2c-hid` | libinput + xkbcommon | layouts, layout switcher | M1 (UI M7) |
| Media / function keys | kernel input | Noobsbot Desktop | OSD | M2 |
| Touchpad / mouse | `i2c-hid`, `hid-multitouch` | libinput | gestures (E1.7, E5.3) | M1 / M5 |
| USB drives, SD cards | `usb-storage`, `uas` | udisks2 | notification + Dolphin + Eject | M4 |
| Phones | kernel | `libmtp` (kio-extras), `libimobiledevice` | Dolphin | M6 |
| USB-C / Thunderbolt docks | `thunderbolt` | `bolt` | "Authorize dock?" alert | M6 |
| Webcam | `uvcvideo` | PipeWire camera portal | none (works in Helium) | M6 |
| Battery / power | ACPI | UPower, power-profiles-daemon | menu bar, Control Center, widget | M2 / M4 |
| Sleep / lid / power button | ACPI | logind | E6.5 | M6 |
| Printers (optional) | — | CUPS | app print dialogs, Settings ▸ Printers | M8 |
| Fingerprint (optional) | kernel | fprintd | lock screen, Settings ▸ Users | done (E7.2) |
| Firmware updates (optional) | — | fwupd | notification | later |

## G3. Services: what and why
| Service | Does | Why this one | Rejected |
|---|---|---|---|
| systemd + logind | boot, sessions, sleep, device access, brightness | Arch standard; safe device access without root | — |
| PipeWire + WirePlumber (+ pulse, alsa) | all audio + screen-share video | modern standard, low latency, Bluetooth audio built in, runs old apps | PulseAudio (legacy), JACK (pro-only) |
| NetworkManager | Wi-Fi, Ethernet, VPN, hotspot | complete D-Bus API, used everywhere | iwd alone (Wi-Fi only), connman |
| BlueZ | Bluetooth | the only stack | — |
| UPower | battery info | standard, tiny | reading `/sys` ourselves |
| power-profiles-daemon | power modes | standard, one D-Bus call | TLP (no D-Bus toggle) |
| udisks2 | USB mounting without root | standard, Dolphin uses it | scripts |
| polkit + polkit-kde-agent | admin password prompts | standard; Qt agent matches Dolphin/Kate | own agent now (later, E7.2) |
| gnome-keyring | stores passwords (Helium, Wi-Fi) | Secret Service standard, light | KWallet (heavier) |
| xdg-desktop-portal + -gtk + -wlr | file pickers, screen sharing | required by browsers; `-wlr` matches our protocols | other portal backends (pull in another desktop) |
| systemd-timesyncd / timedated | time sync, time zone | already in systemd | chrony |
| greetd + tuigreet | login | minimal, independent | SDDM, GDM |
| xwayland-satellite | X11 apps | minimal, separate | built-in X11 WM |

## G4. Complete package list (fresh Arch, installed by `install.sh`)
| Layer | Packages |
|---|---|
| Base | `base` `linux` `linux-firmware` `sudo` `systemd` |
| Build tools | `base-devel` `git` (needed for `paru` and AUR builds) · `rust` only with `--from-source` |
| CPU | `intel-ucode` / `amd-ucode` (detected) |
| GPU | `mesa` + `vulkan-intel` / `vulkan-radeon` / `nvidia-open` + `nvidia-utils` (detected) |
| Video decode | `intel-media-driver` (detected; AMD's VA-API comes with `mesa`, `libva-mesa-driver` is no separate package any more) |
| Audio | `sof-firmware` `alsa-ucm-conf` `pipewire` `wireplumber` `pipewire-pulse` `pipewire-alsa` |
| Network / BT | `networkmanager` `bluez` `bluez-utils` (`broadcom-wl-dkms` + `linux-headers` only if needed) |
| Input / seat | `libinput` `libxkbcommon` `seatd` (provides libseat; the logind backend is used) |
| Graphics | `wayland` `xorg-xwayland` `xwayland-satellite` |
| Login | `greetd` `greetd-tuigreet` |
| Services | `upower` `power-profiles-daemon` `udisks2` `polkit` `polkit-kde-agent` `gnome-keyring` `xdg-desktop-portal` `xdg-desktop-portal-gtk` `xdg-desktop-portal-wlr` `xdg-utils` `xdg-user-dirs` `xdg-terminal-exec` `slurp` |
| Apps | noobsbot-terminal (repo) · `helium-browser-bin` (AUR) · `dolphin` `kio-extras` `kdegraphics-thumbnailers` `ffmpegthumbs` `kimageformats` `qt6-imageformats` `kio-admin` `ark` `archlinux-xdg-menu` · `kate` `nano` · `partitionmanager` `filelight` · `plasma-systemmonitor` `btop` |
| Disk tools | `smartmontools` `exfatprogs` `dosfstools` `ntfs-3g` `btrfs-progs` `e2fsprogs` |
| Look | `inter-font` `ttf-jetbrains-mono` `noto-fonts` `noto-fonts-cjk` `noto-fonts-emoji` · `kvantum` `qt6ct` · WhiteSur themes (AUR: `whitesur-icon-theme` `whitesur-cursor-theme-git` `whitesur-gtk-theme-git` `kvantum-theme-whitesur-git`) · Phosphor icons (bundled with Noobsbot Desktop) |
| Optional | `cups` `fprintd` `fwupd` `bolt` `libimobiledevice` `geoclue` `ffmpeg` · input methods: `fcitx5` `fcitx5-m17n` `fcitx5-configtool` (E6.12) |
| Dev / test only | `qemu-desktop` `virt-manager` (fresh-system VM), `powertop`, `heaptrack` (Part K gates); `paru` is bootstrapped by `install.sh` |
| Security (optional) | `sbctl` (Secure Boot, E6.6) |

AUR names verified 2026-10-18. Nothing from any other desktop is installed.

## G4b. The same list on Debian 13 (installed with `--no-install-recommends`)
| Arch | Debian 13 |
|---|---|
| `base-devel` `rust` | only for a build from source: `build-essential` `pkgconf` `clang` `rustup` + the `-dev` libraries (see ci.yml) |
| `linux-firmware` `sof-firmware` | with the non-free sources: `firmware-linux-free` `firmware-linux-nonfree` `firmware-misc-nonfree` `firmware-sof-signed` `firmware-iwlwifi` `firmware-realtek` `firmware-atheros` `firmware-brcm80211` `firmware-intel-graphics`; without: `firmware-linux-free` |
| `intel-ucode` / `amd-ucode` | `intel-microcode` / `amd64-microcode` (non-free) |
| `mesa` `vulkan-intel` `vulkan-radeon` | `libegl1` `libgles2` `libgbm1` `libgl1-mesa-dri` · `mesa-vulkan-drivers` |
| `intel-media-driver` | `intel-media-va-driver` |
| `nvidia-open` `nvidia-utils` | nouveau by default; Advanced: `nvidia-open-kernel-dkms` `nvidia-driver-libs` `firmware-nvidia-gsp` `libnvidia-egl-wayland1` `libnvidia-egl-gbm1` + headers (driver 550) |
| `networkmanager` `bluez-utils` | `network-manager` `wpasupplicant` · `bluez` (has the tools) |
| `broadcom-wl-dkms` `linux-headers` | `broadcom-sta-dkms` + `linux-headers-<flavour>` |
| `libinput` `libxkbcommon` `seatd` | `libinput10` `libxkbcommon0` `seatd` `libseat1` `libpam0g` `libudev1` |
| `xorg-xwayland` `xwayland-satellite` | `xwayland` `libxcb1` `libxcb-cursor0` · xwayland-satellite from the release |
| `greetd-tuigreet` | `tuigreet` · `dbus-user-session` `systemd-timesyncd` |
| `polkit` `polkit-kde-agent` | `polkitd` `pkexec` `polkit-kde-agent-1` · `libpam-gnome-keyring` |
| `pipewire` … | the same names + `pipewire-bin` (`pw-dump`) · `libglib2.0-bin` (`gdbus`) |
| `kimageformats` `qt6-imageformats` `kconfig` | `kimageformat6-plugins` `qt6-image-formats-plugins` `libkf6config-bin` · `qt6-wayland` |
| `archlinux-xdg-menu` | none: `data/debian/system/etc/xdg/menus/noobsbot-applications.menu` |
| `inter-font` `ttf-jetbrains-mono` `noto-fonts` `noto-fonts-cjk` `noto-fonts-emoji` | `fonts-inter` `fonts-jetbrains-mono` `fonts-noto-core` `fonts-noto-cjk` `fonts-noto-color-emoji` |
| `kvantum` | `qt-style-kvantum` · `breeze-icon-theme` |
| `helium-browser-bin` (AUR) | `helium-bin` from `https://pkg.helium.computer/deb` (key in `data/debian/helium`) |
| WhiteSur (AUR) | git: icons, cursors, gtk (needs `sassc` `libglib2.0-dev-bin` `libxml2-utils`), kvantum (pinned in install.sh) |

Debian names verified 2026-10-26 (on arm64; the x86-only ones by name).

---

# Part H — Fonts, icons & themes
| Use | Choice | Why | Rejected |
|---|---|---|---|
| UI text | **Inter** | closest free match to Apple's SF Pro; built for screens | SF Pro (license), Cantarell (another desktop's look) |
| Monospace | **JetBrains Mono** | very readable; good for noobsbot | — |
| Other languages | **Noto Sans / Serif** | covers almost every script | — |
| CJK | **Noto CJK** | complete coverage | — |
| Emoji | **Noto Color Emoji** | full colour, free | Apple Color Emoji (license) |
| App icons | **WhiteSur icon theme** | high-quality squircle style, large coverage | Apple icons (license), Papirus (doesn't match the design) |
| Symbols | **Phosphor Icons** (MIT) | consistent line icons with regular + fill weights, close to SF Symbols in feel | SF Symbols (license), mixed theme symbolics (inconsistent) |
| Cursors | **WhiteSur cursors** | cursors matching the icons | — |
| Qt apps (Dolphin, Kate, …) | **our "Noobsbot Glass" Kvantum theme + colour scheme** (derived from WhiteSur Kvantum) + qt6ct, generated from Part D tokens (E2.9) | the same glassy look as the desktop, light/dark + accent in sync | Breeze (another desktop's look), plain WhiteSur (not our exact tokens) |
| GTK dialogs | **WhiteSur-gtk recoloured to Part D** (E2.9) | same colours for file pickers (solid; GTK can't do translucency) | Adwaita (another desktop's look) |
| Icon fallback | `hicolor` + `breeze-icons` (comes with Dolphin) | never a missing icon | — |
| Wallpapers | **our own** set (D11) | Apple wallpapers are copyrighted | — |

---

# Part I — Performance budget (checked at every milestone)
| Target | Goal | How |
|---|---|---|
| CPU idle | **0%** | damage-only redraw; clock once per minute; services push changes (no polling); calloop sleeps |
| RAM (whole desktop) | **< 100 MB** USS (M1 < 60 MB) | one process (C5); glyph and icon caches live in GPU memory, CPU copies dropped (E1.11); hidden Spaces not drawn |
| GPU memory (shell only) | **< 128 MB** | glyph atlas 16 MB + icon textures ≤ 64 MB + wallpapers + shadow slices; app buffers not counted |
| Startup | **< 500 ms** to usable | no toolkit; app/icon index built in the background |
| Input latency | **≤ 1 frame + ~2 ms** (input → photon) | render as late as possible before vblank, hardware cursor plane (E1.10) |
| Layout | never per frame | layout cached, recomputed only on change (D13) |
| Gestures & animations | full frame rate (60/120 Hz) | transforms on existing textures; no app redraws |
| Fullscreen video/games | close to bare-metal | direct scanout; idle-inhibit |
| Glass cost | **one pass** per surface | no blur (R2) |
| Battery | better than traditional desktops | all of the above + power-profiles-daemon |
| Binary | small | `lto = "thin"`, `strip = true` |
Measured with USS (`Private_Clean + Private_Dirty` in `/proc/<pid>/smaps_rollup`; plain RSS also counts Mesa's
shared libLLVM/libgallium pages, ~130 MB that every GL process maps), per-thread CPU ticks, frame-time logs and
`powertop`; results per milestone in `PERF.md`.

---

# Part J — Code layout & Rust libraries

## J1. Folder layout
One crate; becomes a cargo workspace when the Settings app is added (M7).
```
custom-desktop-environemt/
  plan.md  Cargo.toml  rustfmt.toml (from custom-terminal)  README.md
  src/
    main.rs        start-up: --nested (dev) / --session (real), logging, backend
    state.rs       central state + Wayland protocol handlers
    backend/       winit.rs (dev window) · udev.rs (real hardware, M6)
    wm.rs          placement, focus, stacking, move/resize, minimise, zoom, fullscreen, snapping
    rules.rs       window rules (E1.9)
    clipboard.rs   clipboard persistence (E1.1)
    perms.rs       privileged-protocol permissions (E6.11)
    ime.rs         text-input-v3 / input-method-v2 (E6.12)
    spaces.rs      Spaces (M5)
    input.rs       shortcuts, gesture recogniser, hot corners, routing
    render.rs      frame assembly + damage tracking
    frame.rs       per-output frame scheduler, render-time estimate, cursor plane (E1.10)
    shaders/       glass.frag · rounded.frag · shadow.frag
    text.rs        cosmic-text → glyph atlas
    icons.rs       icon lookup + SVG/PNG → textures
    theme.rs       Part D tokens (light + dark)
    anim.rs        springs (D10)
    config.rs      config.toml + live reload
    apps.rs        .desktop index (dock, Spotlight, Launchpad) + launching
    ui/layout.rs   flex-like layout engine, cached (D13)
    ui/            D8 components: panel, label, icon, button, toggle, slider, list, menu, popover, field, alert, tooltip, badge, progress, segmented
    shell/         menubar · control · calendar · notifications (banners + center) · dock · spotlight · launchpad · switcher · mission · lock · osd · screenshot · forcequit · alerts
    services/      zbus: power · network (+ NM SecretAgent) · bluetooth (+ BlueZ Agent1) · audio · brightness · media (MPRIS) · tray (+ SNI watcher) · notifications (server) · launcher-entry (badges) · time · session · disks · update (E6.13)
    bin/           update-helper.rs → noobsbot-desktop-update (privileged installer, polkit) · watchdog.rs → noobsbot-desktop-watchdog (heartbeat watchdog)
  assets/          Phosphor symbols (SVG), noobsbot-desktop logo
  data/            config.toml (commented defaults) · helium-browser-flags.conf · wallpapers/ · system/ (mirrors `/`: greetd config, pam.d/noobsbot-desktop, portals.conf, UPower.conf.d drop-in, wayland-sessions .desktop, session target, polkit action)
  themes/          Noobsbot Glass Kvantum + colour scheme + GTK (GPL-3.0, generated from theme.rs)
  install.sh  LICENSE (MIT)  CHANGELOG.md (generated)  release-plz.toml
  .github/workflows/  ci.yml (fmt, clippy, tests) · release-plz.yml (version PR + tag) · release.yml (build, sign, publish)
```

## J2. Rust crates and why
| Crate | For | Why | Milestone |
|---|---|---|---|
| `smithay` (+ `calloop`) | compositor + event loop | C3, C7 | M1 |
| `tracing`, `tracing-subscriber` | logs | C12 | M1 |
| `serde`, `toml`, `notify` | settings + live reload | C11 (same as noobsbot) | M1 |
| `regex-lite` | window-rule title matching (E1.9) | tiny, no backtracking blow-ups | M1 |
| `png`, `zune-jpeg` | wallpapers, screenshots | small, fast; avoids the big `image` crate | M1 |
| `cosmic-text` | text | C8 (same as noobsbot) | M2 |
| `resvg` | SVG icons and symbols | best pure-Rust SVG renderer | M2 |
| `zbus` | all system services | C9 | M2 |
| `chrono` | clock, calendar | standard | M2 |
| `freedesktop-desktop-entry` | `.desktop` files | System76-maintained | M3 |
| `freedesktop-icons` | icon theme lookup | standard lookup rules | M3 |
| `nucleo-matcher` | fuzzy search | very fast (Helix) | M4 |
| `evalexpr` (or similar) | Spotlight calculator | tiny | M4 |
| `pipewire` | volume, outputs, mic (replaces `wpctl`, E1.11) | official Rust bindings; no process per change | only if coalesced `wpctl` (E1.11) still lags |
| `libc` | `kill` for Force Quit, socket credentials | standard | M4 |
| `pam-client` | lock screen password | standard Linux auth | M6 |
| ~~`greetd_ipc`~~ | own graphical greeter (E6.6) | not needed: the protocol is spoken directly in `shell/greeter.rs` | — |
| `ureq`, `semver`, `minisign-verify`, `zstd`, `tar` | Software Update (E6.13) | small blocking HTTP + proven signature check; no async runtime | M6 |
| `chrono-tz` (optional) | world clocks | time-zone data | later |
Rule: **a crate is added only when its feature is built.**

---

# Part K — Testing & design review
| Level | What | When |
|---|---|---|
| Design mockups | HTML mockups of each milestone's elements from Part D tokens, light + dark; owner approves | before coding each milestone |
| Unit tests | pure logic: hit-testing, springs, calendar grid, fuzzy ranking, Mission Control layout, gesture recogniser thresholds, config parsing | every milestone |
| Nested run | `cargo run -- --nested`; walk every element's "done when" | M1–M5 |
| Design review | screenshots vs. approved mockups (sizes, colours, spacing, motion); contrast ≥ 4.5:1 | end of each milestone |
| Protocol debugging | `WAYLAND_DEBUG=1 <app>` | as needed |
| Performance | `ps -o rss,%cpu`, frame-time logs, `powertop` | every milestone |
| Fresh-system test | Arch in a QEMU VM → `install.sh` → login | M6+ |
| Real hardware | Wi-Fi, Bluetooth, sound, sleep/wake, external monitor, all gestures | M6+ |
| Safety net | Ctrl+Alt+F3 → text console | M6+ |
| **Spikes** | three pass/fail experiments in the first coding week: (a) the nested compositor runs Helium + Dolphin + the portal file picker with the portals.conf; (b) Dolphin translucent through Kvantum on Wayland (E2.9); (c) a GTK3 dialog takes our title bar through `kde-server-decoration` (E1.3). A failed spike updates the plan before M1 continues | first week |
| Lints + CI | `cargo clippy -D warnings`, with `unwrap_used` / `expect_used` / `panic` denied in `src/shell` and `src/ui`; GitHub Actions runs fmt, clippy and tests on every push | every commit |
| Release pipeline | a `feat:` commit on a test branch → release PR → tag → signed tarball; a VM on the previous version updates and a broken build rolls back (E6.13) | M6 |
| Latency + resize | 240 fps camera drag test; slow-app resize anchoring; screen-size matrix from D13 (720p … 4K @ 200%, portrait) checked in mockups and real runs | M1, every milestone |
| Robustness | fuzzing client (garbage / out-of-order requests); `kill -STOP` on NetworkManager and BlueZ while using the desktop; 20 × suspend/resume; unplug all monitors; broken `config.toml`; `heaptrack` + 24 h soak (RSS flat) | M1 (fuzz, config), M6 (rest) |
| VM | QEMU with `virtio-gpu` + virgl for the GPU path **and** without it for the pixman fallback | M6 |
| Perf log | each milestone's RSS, idle CPU, startup time and frame time recorded in `PERF.md`; the Part I budget is a gate, not a wish | end of each milestone |

---

# Part L — Known limits, risks & open questions

## Known limits (accepted)
- **L1 — Global app menus:** most Linux apps (incl. Helium) don't publish File/Edit/View menus, so the menu bar shows
  the app name + About/Hide/Quit.
- **L2 — Apps with their own title bars** (GTK4/libadwaita) won't show our traffic lights; themes make them look close.
- **L3 — No Apple fonts, icons, symbols or wallpapers** (license). Open look-alikes (Part H).
- **L4 — Dolphin/Kate bring Qt framework libraries** (~300 MB disk); no RAM cost until opened.
- **L5 — No 2-finger edge swipe for Notification Center** (libinput doesn't report finger position); click the clock / Super+N.
- **L6 — Polkit password prompts:** our own agent (E7.2) shows them in our alert; the fallback agent's dialog appears only if ours
  can't register.
- **L7 — Screen sharing is per screen, not per window** (xdg-desktop-portal-wlr limit); window capture comes with our own
  `ext-image-copy-capture` toplevel support later.
- **L8 — Dolphin's sidebar glass depends on Kvantum translucency on Wayland**; if the M2 spike fails, Qt apps get the
  solid Noobsbot Glass (same colours) instead.
- **L9 — KIO network-share passwords** are asked once per session (no KWallet; one keyring, E6.7).
- **L10 — No hibernate** in v1 (sleep only).
- **L11 — No HDR or 10-bit** in v1. VRR is a per-display toggle (`[[display.outputs]] vrr`), off by default.
- **L12 — Desktop icons** are a simple grid (E7.2): no multi-select, no thumbnails.
- **L13 — English UI only** in v1 (dates and numbers follow the locale); no fast user switching.
- **L14 — X11 apps are soft on HiDPI** (bitmap-scaled through XWayland); native apps are sharp.

## Risks
| Risk | Mitigation |
|---|---|
| Smithay API changes | pin version; upgrade between milestones |
| Shell bug crashes the desktop (C5) | Rust safety; no `unwrap()` in shell code; tests; returns to login, never unlocked |
| NVIDIA quirks | `nvidia-open` 555+, explicit sync (`linux-drm-syncobj-v1`), GPU-reset recovery (E1.11); test in M6; pixman fallback |
| Hybrid-GPU laptops | `MultiRenderer` from day one of M6 (E6.1); tested on an Intel + NVIDIA machine before M6 closes |
| Super key taken by the host desktop during nested development | dev config can use Alt |
| Glass readability on busy wallpapers | ≥ 85% opacity + contrast checks (D2); "Reduce transparency" |
| Large scope | strict milestones; each ends usable; mockups approved before code |
| Kvantum translucency broken on Qt 6 Wayland | spike in M2; solid fallback decided in advance (E2.9, L8) |
| Desktop shortcuts fight the apps | modifier model fixed in E1.6: Super is ours, Ctrl and F-keys belong to apps |
| `install.sh` fetches code from the internet | runs as the user, `sudo` only for pacman; pinned tags + commit hashes; system files come from the repo |
| GitHub unreachable / rate-limited | update check fails silently and retries next day; nothing else depends on it |
| Update bricks the session | staged install, `boot-ok` marker, automatic rollback to the previous version (E6.13) |
| Privileged protocols abused by a rogue app | E6.11 permission alerts; privileged globals only for trusted clients |

## Open questions (decide at the named milestone)
- **Open-1 (M5):** ship the optional "Mac keys" mode (Super = Cmd, full binding swap, E1.6) in v1 or later? The
  default modifier model is already decided in E1.6; this only concerns the optional mode.
- **Open-2 (M7):** Settings app toolkit: iced (front-runner) vs slint.
- **Open-3 (after M6):** AUR `PKGBUILD` as a second install path (Software Update, E6.13, covers updates already; the updater only notifies on AUR installs).
- **Open-4 (decided):** file search in Spotlight uses no index or daemon of our own: recent files
  (`recently-used.xbel`) plus `plocate` when installed (home folder only, hidden paths dropped). Without it, only
  recent files show.
- **Open-5 (decided):** weather uses Open-Meteo, off by default; it sends only the city or coordinates you set.

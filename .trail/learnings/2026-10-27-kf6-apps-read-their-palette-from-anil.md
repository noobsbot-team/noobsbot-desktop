---
title: "KF6 apps read their palette from kdeglobals [UiSettings] ColorScheme, else Breeze Light"
author: Anil
date: 2026-10-27
branch: feat/parity
cost: { minutes: 1, tokens: 4538 }
id: l-1c6fa144
scope: repo
status: active
replaces: l-7fcf643b
src: agent
touches:
  - src/theme.rs
---

## Decided
src/theme.rs `write` puts `[UiSettings] ColorScheme=<NoobsbotGlass|NoobsbotGlassDark>` in kdeglobals. KF6 KColorSchemeManager (which Dolphin links) reads that key from the app rc cascaded with kdeglobals. When the key is empty and the platform theme isn't the KF6 one (we run qt6ct), it falls back to the bundled BreezeLight.colors whenever Qt's colour-scheme hint isn't Dark. That gave Dolphin #232629 text on our dark Kvantum frames, while views kept our #323232 background from [Colors:View]. The ID is the .colors file name without the extension, not the [General] Name.

## Tried and ruled out
- Unsetting QT_QPA_PLATFORMTHEME: no change.
- A first test of the kdeglobals key looked like a failure. Four stale test Dolphins were still running in the nested desktop, so kill them by PID before each measurement.

## Watch out
- A `[UiSettings] ColorScheme` that the user picks in an app's own colour menu goes to that app's rc (e.g. dolphinrc) and overrides ours.
- Running apps only pick the change up on restart.
- `cargo test` doesn't rebuild target/debug/noobsbot-desktop, so run `cargo build` before using `theme --write` by hand.
- The nested desktop's idle dim halves every pixel in screenshots.

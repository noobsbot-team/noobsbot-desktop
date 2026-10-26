---
title: "noobsbot-desktop update reuses the in-desktop updater, not install.sh"
author: Anil
date: 2026-10-26
branch: feat/update-command
cost: { minutes: 17, tokens: 55073 }
id: l-1a816613
scope: repo
status: active
src: agent
touches:
  - README.md
  - src/main.rs
  - src/services/update.rs
  - install.sh
---

## Decided
- `noobsbot-desktop update` (src/services/update.rs `cli`, dispatched in src/main.rs) shares `latest()`/`download()` and the pkexec helper (`HELPER`) with "Restart to Update", so the check, signature verification and no-downgrade rules live in one place. It is a subcommand handled before the compositor starts, like `theme` and `dock`.
- It does not log out; it prints "log out and back in", since the running session isn't the CLI's to end.

## Tried and ruled out
- A shell wrapper around `./install.sh --update`: needs a git checkout on the user's machine and duplicates what the signed-release updater already does.

## Watch out
- Installs older than the release that ships this command (e.g. 0.2.1) don't have it; they update once via Restart to Update or `./install.sh --update`.
- Only the "up to date" path was run by hand; the download + pkexec path is the same code as Restart to Update but wasn't exercised end to end from the CLI.
- Non-install.sh installs (AUR, hand-built) get `Download::None` and exit 1 with a pointer to their package manager.

---
title: release-plz needs the org and repo to allow Actions to create PRs
author: Anil
date: 2026-10-26
branch: main
cost: { minutes: 14, tokens: 11513 }
id: l-271d2405
scope: repo
status: active
src: agent
touches:
  - CHANGELOG.md
  - Cargo.toml
  - README.md
  - data/system/usr/share/polkit-1/actions/org.noobsbot.desktop.update.policy
  - install.sh
  - plan.md
  - settings/Cargo.toml
  - src/main.rs
  - src/services/update.rs
  - ".github/workflows/release-plz.yml"
---

## Watch out
- `.github/workflows/release-plz.yml` opens the release PR with `GITHUB_TOKEN`. That only works when "Allow GitHub Actions to create and approve pull requests" is on in BOTH the noobsbot-team org (Settings > Actions > General) and the repo. Otherwise release-pr fails with 403 "GitHub Actions is not permitted to create or approve pull requests."
- It reset to off when the repo moved from anil0403 to the noobsbot-team org (Oct 2026). The repo setting can't be turned on (409) until the org allows it.
- Fix: enable it at the org, then `gh api -X PUT repos/noobsbot-team/noobsbot-desktop/actions/permissions/workflow -f default_workflow_permissions=read -F can_approve_pull_request_reviews=true`, then `gh run rerun <id> --failed`.

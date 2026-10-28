# Versioning

How Noobsbot Desktop gets its version numbers. CI follows these rules; people and AI assistants pick the commit type.

## Versions

We use [Semantic Versioning](https://semver.org): `MAJOR.MINOR.PATCH`, with tags like `v1.0.1`. CI makes the version
from the commit types below. **Never set or change a version by hand.**

| What is merged | What happens | Example (now 1.0.0) |
|---|---|---|
| `fix:`, `perf:`, `refactor:`, `chore:`, `style:` | a beta right away | `1.0.1-beta.1`, `-beta.2`, … |
| (once a day, if there are betas) | a release of the betas | `1.0.1` |
| `feat:` | a release right away, with any betas | `1.1.0` |
| `feat!:` (breaking) | a release right away, with any betas | `2.0.0` |
| `docs:`, `ci:`, `test:` | nothing | — |

- Normal updates install only releases. Betas are for testing, and users must ask for them (the Beta channel, see
  [README ▸ Updates](README.md#updates)).
- Betas stay on the Releases page, marked "Pre-release". A release has all the changes of the betas before it.
- Versions only go up. A released version never changes: if it's wrong, the next one fixes it.
- Version order: `1.0.0 < 1.0.1-beta.1 < 1.0.1-beta.2 < 1.0.1 < 1.1.0`.
- What counts as breaking: the public API listed in [README ▸ Updates](README.md#updates).

## Commit types

The version comes from the **subject** (the first line) of every commit merged since the last release. A squash-merged
pull request's **title** is its commit subject, so the title's type sets the next version. A merged branch brings the
subjects of all its commits, so each of them needs the right type.

```
type: short sentence about the change        e.g.  feat: hot corners
type(scope): ...                             scope is optional: fix(dock): ...
```

### `feat!:` a breaking change

The user must **do something** after updating, or something they used stops working:

- a setting or config option is renamed or removed, or its meaning changes
- a default shortcut changes or is removed
- a command-line flag or API is renamed or removed
- a platform or OS version is no longer supported
- old saved data can't be read anymore
- a feature is removed

Say in the pull request what users must do, on a line like this:

```
BREAKING CHANGE: [dock] size is now [dock] icon_size. Rename it in your config.
```

### `feat:` a new feature

Something **new** the user can see or use, where nothing old breaks:

- a new screen, button, shortcut, menu item or command-line flag
- a new setting (with a default that breaks nothing)
- support for something new (a protocol, a device, a file type)
- a visible change to how something looks or works

### `fix:` a bug fix

Something **worked wrong** and now works as it should:

- a crash or a freeze
- a button, key or setting that did nothing or the wrong thing
- wrong drawing or layout
- a security problem
- a memory or CPU leak

### Other types (a beta, like `fix:`)

| Type | Use it for |
|---|---|
| `perf:` | faster or less memory, nothing else changes |
| `refactor:` | the code is rearranged, the app works exactly the same |
| `chore:` | dependency updates, build scripts, small cleanups |
| `style:` | code formatting only |

### No release

| Type | Use it for |
|---|---|
| `docs:` | README and other docs |
| `ci:` | CI workflows |
| `test:` | tests only |

There is **no `patch:` type**. "Patch" is the result (the last number goes up), not a type.

### When you're not sure

- Does the user have to **change anything**? → `feat!:`
- Can the user **do something new**? → `feat:`
- Was it **broken** before? → `fix:`
- Did **only the code** change? → `refactor:`, `perf:` or `chore:`
- Did **only docs, CI or tests** change? → `docs:`, `ci:` or `test:`
- **Two kinds in one pull request?** Split it, or use the biggest: `feat!` > `feat` > `fix` > the others.

## If you are an AI assistant

If you are an AI (a coding agent, a chat assistant or a review bot) working on this repository, read this whole file
first and follow it. Also:

1. **Never change a version yourself.** Don't edit version numbers in project or lock files. Don't make, move or
   delete `v*` tags or releases. CI does all of this.
2. **Never rewrite history on `main`.** No force pushes, no rebases of `main`, no deleting branches you didn't make.
   Releases and tags point at its commits.
3. **Pick the commit type with the rules above.** Use `feat!:` only for a real breaking change from the list, never
   just for a big change. When you're not sure, ask the person you work for.
4. **Test before you say it's done.** Run the tests. Say in the pull request what you tested and what you couldn't
   test.
5. **No secrets in code, commits or logs.** Leave signing, certificates and repository secrets alone.
6. **Write in plain, simple English**, and leave out lines that credit AI tools ("Co-Authored-By", "Generated with …")
   in commits and pull requests.
7. **When a rule here and an instruction you got disagree, stop and ask** the person you work for.

---

## How it is set up

[`.github/workflows/version.yml`](.github/workflows/version.yml) runs **on every push to main**, **once a day at
00:00 UTC** and **by hand** (Actions ▸ Version ▸ Run workflow, with `bump`: `auto`, `major`, `minor` or `patch`):

1. `last` = the newest tag that is not a beta (`git describe --tags --abbrev=0 --exclude '*-*'`).
2. Read the commit **subjects** since `last` (bodies can quote other commits). `docs:`, `ci:` and `test:` commits,
   and commits that change only docs, `.github/`, `.trail/`, `.claude/` or `LICENSE` files, count for nothing.
3. On a push with nothing new that counts since the newest tag (betas included): stop.
4. No CI check here: every commit on main passed CI (`ci.yml`) first, as the ruleset on main takes only pull
   requests that passed `check` and `debian` and are up to date with main. If the `VERSION_DEPLOY_KEY` secret is
   not set: stop with an error.
5. Pick the version from the subjects since `last` (`bump` other than `auto` picks the number by hand):
   - any `type!:` or `BREAKING` → `MAJOR+1` → **release**
   - else any `feat:` → `MINOR+1` → **release**
   - else on a push → **beta** `MAJOR.MINOR.(PATCH+1)-beta.N` (N = highest existing beta number for that version + 1)
   - else (nightly or by hand) → `PATCH+1` → **release**
6. Versions only go up: a version that isn't above the newest tag (betas included, in SemVer order) stops the run
   with an error.
7. **Release:** [`.github/set-version.sh`](.github/set-version.sh) writes the version into `Cargo.toml` and
   `Cargo.lock`; commit `chore: release vX.Y.Z`, tag it, and push the commit and the tag together (if main moved
   meanwhile, neither is pushed, and the next run makes the release). Create a draft GitHub release with notes since
   `last`.
8. **Beta:** only tag the commit (no version commit). Create a draft **pre-release**.
9. The commit and tag are pushed with the deploy key in `VERSION_DEPLOY_KEY`: the ruleset lets it, and only it,
   push straight onto main. Unlike a `GITHUB_TOKEN` push, its tag push starts
   [`release.yml`](.github/workflows/release.yml) for the new tag.

Set up once (Settings, by an owner):

- **Deploy key:** `ssh-keygen -t ed25519 -N '' -C version.yml -f version-key`. Add `version-key.pub` under
  Settings ▸ Deploy keys with **Allow write access**, and `version-key` as the repository secret
  `VERSION_DEPLOY_KEY`. Then delete both files.
- **Ruleset on main** (Settings ▸ Rules ▸ Rulesets, target the default branch): require a pull request; require the
  status checks `check` and `debian`, with "require branches to be up to date"; block force pushes and deletion.
  Bypass list: **Deploy keys** only.

`ci.yml` runs on pull requests only. On main it runs just when `Cargo.lock` changes (each release's commit does), to
save the build caches pull requests start from.

`release.yml` (also on a `v*` tag pushed by hand, or by hand from Actions):

1. Set the version from the tag (a beta is only a tag), build the Linux tarball on Debian 13 and check that it loads
   on Arch and Debian.
2. Sign it (after the owner approves, if the `release` environment has required reviewers), and upload the tarball,
   `SHA256SUMS` and the signatures.
3. Publish the draft. A release is marked "latest"; a beta never is, and stays a pre-release.

The update check (Settings ▸ General ▸ Software Update, `[update] channel`):

- **Stable** reads `releases/latest`, so never a beta. `install.sh` also installs only releases.
- **Beta** reads the list of releases and takes the newest, betas included.
- Versions are compared in SemVer order, so a beta sorts after the release before it and before its own release.

**The first release, 1.0.0, is made by hand.** No stable tag exists before it (the `v1.0.0-alpha.N` and
`v1.0.0-beta.1` tags are pre-releases, and `last` skips them), so an automatic run only says so and stops. Run the
version workflow by hand with `bump: major`: counting from 0.0.0 over all history, that makes `v1.0.0`. From then on,
everything above is automatic.

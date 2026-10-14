# Linux_Util Desktop: plan

Built in stages, each ending in something that works and can be committed, so
it is safe to stop between any two. Tick the boxes as work completes.

**Current stage: 7 (version and updates) complete. All planned stages are done.**

## Decisions made

| Topic | Decision |
|---|---|
| Name | Repo `linux_util_desktop`; display name **Linux_Util Desktop** |
| Scope | GUI only. No install logic is rewritten; it wraps `linux_util.sh` |
| Toolkit | Tauri (small Rust shell + TypeScript UI) for an identical look on every distro |
| Packaging | AppImage, `.deb`, `.rpm`. No Flatpak (sandbox blocks admin rights) |
| Bundling | Vendored, pinned copy of `linux_util.sh` + `lib/`, recorded in `vendor/linux_util.VERSION` |
| Versioning | Own tags, independent of linux_util |
| Privileges | Graphical password prompt, never handled in-app. Mechanism in "Privilege handling" below |

## Architecture

```
┌─ Linux_Util Desktop window (Tauri) ───────────────┐
│  UI (TypeScript/HTML/CSS)                         │
│    category list · search · checkboxes · log pane │
│        │ invoke             ▲ events (output)     │
│  Rust shell                                       │
│    run_list()  run_action()  askpass helper       │
└────────┬──────────────────────────────────────────┘
         │ spawns as the normal user
         ▼
   vendor/linux_util.sh  --list --json | --install | --uninstall | --update
         │ calls sudo when needed
         ▼
   graphical password prompt
```

## Privilege handling (needs a spike in Stage 3)

`linux_util.sh` **exits immediately if run as root** and calls `sudo` itself.
So wrapping the whole script in `pkexec` will not work. The planned approach:

1. Run the script as the normal user.
2. Put a tiny `sudo` shim first on `PATH` that runs `/usr/bin/sudo -A "$@"`.
3. Point `SUDO_ASKPASS` at a helper that shows the app's password dialog.

This needs no change to linux_util. Without a terminal the script also skips
its `sudo -v` credential cache, so the helper may prompt more than once per
action. The helper can hold the password in memory for the length of one run.
Stage 3 starts by proving this works on Fedora before building on it.
Fallback if it does not: run the script inside a PTY and answer the prompt.

## Stages

### Stage 0: Groundwork (done)
- [x] Repo created at `~/Projects/github/linux_util_desktop`
- [x] README, PLAN, CLI contract, LICENSE, .gitignore
- [x] First commit and GitHub repo (`acebmxer/linux_util_desktop`; v0.1.0 released 2026-10-14)

### Stage 1: Skeleton
Goal: an empty Linux_Util Desktop window that builds and launches.
- [x] Scaffold Tauri app (TypeScript, no heavy UI framework unless needed)
- [x] Window title, app ID, icon placeholder, desktop entry
- [x] `npm run tauri dev` works; document prerequisites for Fedora, Debian, Arch
- [x] CI: build on push (Ubuntu runner)

### Stage 2: Read-only list
Goal: see every utility and its install status. Nothing can change the system.
- [x] Vendor `linux_util.sh` + `lib/` at the latest tag (manual copy for now): v1.9.0
- [x] Rust command runs `--list --json --no-color`, strips the `Detected:` line
- [x] UI list with installed badge and version
- [x] Search box
- [x] Categories (JSON fields ship in linux_util v1.9.0, now vendored); sub-categories group the list and nest in the sidebar
- [x] Detected distro shown in the header (read from `/etc/os-release`; v1.8.0 prints no `Detected:` line)
- [x] Light and dark theme following the system

### Stage 3: Actions
Goal: install, uninstall and update from the app.
- [x] **Spike:** prove the `sudo -A` shim and askpass end to end on Fedora (2026-10-14, Fedora container, non-root user, no TTY, stdin `/dev/null`): `--install` and `--uninstall "Parallel Downloads"` both ran, exit 0; askpass was called once per `sudo` call (no credential cache without a TTY); an askpass that exits 1 makes the action fail with exit 1 and no hang
- [x] Run one action with live output streamed into a log pane
- [x] Install, uninstall, update, and the exit-code result (update: same code path, not run)
- [x] Multi-select and a run queue (queue: one utility at a time; only one-item runs exercised)
- [x] Dry-run toggle
- [x] Cancel a running action, and block a second concurrent run (concurrent block: Rust check plus disabled buttons; only the disabled buttons were seen)
- [x] Error display and a copy-log button (copy button: not clicked in the test)

Built: `src-tauri/src/actions.rs` (run queue, `sudo` shim, askpass over a private
Unix socket, password checked once with `sudo -S -k -v` and kept in memory for
one run) and the action bar, log pane and password dialog in the UI. Actions run
from a per-user copy of the vendored script in the app data directory, because
`linux_util.sh` writes `logs/` next to itself and the bundled copy can be
read-only. Dry run still asks for the password (the script refreshes the
package cache first). Tested in a Fedora 45 container under a virtual display,
not on a real desktop.

### Stage 4: Sync and contract
Goal: keep the bundled script current without surprises.
- [x] `scripts/sync_linux_util.sh <tag>` copies a tag into `vendor/` and writes `VERSION`
- [x] Contract test checks the bundled script against `docs/CONTRACT.md` (`npm run contract`, also in the Build workflow)
- [x] Scheduled CI job opens a PR when a new linux_util tag appears (`sync-linux-util.yml`, daily, PR against `dev`; draft if the contract fails)
- [x] Test that guards `--list --json`: decided here only, covered by the contract test

### Stage 5: Packaging and release
Goal: installable builds on other distros.
- [x] AppImage, `.deb`, `.rpm` via Tauri bundler (built on Ubuntu 22.04, the same as the Release workflow)
- [x] Declare the WebKitGTK dependency in `.deb` and `.rpm`: Tauri adds WebKitGTK and GTK itself (the `.rpm` by soname, so it resolves on openSUSE too); `bash` and `sudo` added
- [x] Release workflow on a version change on `main`, with checksums (`release.yml`: builds, smoke-tests, attaches the files to a draft release; publishing the draft creates the tag, so a published release is never empty)
- [x] Smoke-test installs in containers: Ubuntu, Fedora, Arch, openSUSE (`scripts/smoke_test.sh`, also a job in `release.yml`; run locally with podman, all four pass)
- [x] First tagged release `v0.1.0` (2026-10-14; the `.rpm` installed and ran an install and uninstall on Fedora 45. The release was published by hand before the workflow finished, so its files were attached afterwards; `release.yml` now uploads to an existing release)

Found while testing: the AppImage's launcher points `LD_LIBRARY_PATH`, `PATH`
and the GTK/GIO paths into the bundle, so on Arch `curl`, `git` and `pacman`
run by linux_util.sh crashed with "symbol lookup error". The app now gives
linux_util.sh the host's environment back and runs it from the home directory
(`use_host_env` in `lib.rs`); the askpass helper keeps the bundle's library
path. Checked on Arch: a full Parallel Downloads install from the AppImage,
password dialog included, succeeded and showed Installed.

### Stage 6: Polish
- [x] Profiles (`--export-profile`, `--import-profile`): a Profiles dialog lists linux_util's built-in profiles (read with `--export-profile`); choosing one replaces the selection, leaving out names this distro does not offer. Import reads a profile file (one profile is selected at once, several are listed); export saves the selection as a one-profile file. The app applies profiles through its own queue, not `--import-profile`, so each utility gets its own status; its exported files still work with `--import-profile` (checked with `--dry-run`). Tested in an Ubuntu 22.04 container under a virtual display, not on a real desktop
- [x] Update-all button: in the action bar, always shown; one `--update-all` run with the dry-run toggle (dry run checked in the container)
- [x] Log viewer for linux_util's logs: the `logs/` folder of the per-user copy actions run from, newest first, `*_latest.log` links left out, long logs shown from the end. Staging that copy no longer copies the bundled `logs/` (a source checkout's `--list` logs were showing up)
- [x] Settings (config file, theme): theme (system, light, dark) and the five linux_util config keys the app's runs read (`auto_confirm`, `verbose`, `debug`, `log_retention_days`, `max_logs_per_day`), kept in the app config folder; `linux_util.conf` there holds only changed keys and is passed in `LINUX_UTIL_CONFIG_FILE`. Retry, DNS and disk-space keys are left out: only the terminal menu reads them
- [x] Accessibility and keyboard navigation: TUI keys `/`, Esc, arrows, Space, A, D (Enter, U and Q left out: a stray key should not start an install or close the window); the list is one tab stop, labelled controls and dialogs, live status text, visible focus

All of Stage 6 tested in an Ubuntu 22.04 container under a virtual display, not on a real desktop.

### Stage 7: Version and updates
Goal: see which version is running, and get a newer release from inside the app.
- [x] Show the app version and the bundled linux_util version (`vendor/linux_util.VERSION`) in the window (bottom line)
- [x] Check the GitHub releases of this repo for a newer version on launch, with a Settings switch to turn the check off; show what changed (a bar, and a dialog with the release notes; a "Check for updates" link checks on demand)
- [x] Install the update from the app: the `.deb` or `.rpm` through the existing password dialog (apt, dnf or zypper), or replace the AppImage file; checked against the release's `SHA256SUMS`

Tested against a local stand-in for the GitHub release (`LU_RELEASES_URL`, which also lifts the download address check, for testing only) in containers under a virtual display, with real 0.1.0 and 0.2.0 builds: `.deb` on Ubuntu 22.04, `.rpm` on Fedora and openSUSE, AppImage on Ubuntu 22.04. Each took 0.1.0 to 0.2.0; a wrong checksum installed nothing; a read-only AppImage folder failed with a message. Not tested: Arch, or against the real GitHub API.

A newer linux_util arrives in a new app release (open question 4): the app updates as a whole, unlike linux_util's own `git pull` self-update.

### Later (not committed to)
- Update check on launch that offers a newer linux_util
- Arch package (PKGBUILD / AUR)

## Open questions

1. **Run the sudo shim approach?** Confirm in the Stage 3 spike (above).
2. **Categories.** Decided: (a). `linux_util` `--list --json` now has `category` and
   `subcategory` on each utility (added to the existing `linux_util/list/v1`
   schema, since the fields are additive). Released in linux_util v1.9.0.
3. **Test coverage.** Decided: the contract test lives in this repo only.
4. **Pinning policy.** One bundled script version per release, with no runtime
   downloads, as assumed in the README.
5. **Windows/macOS.** Out of scope; linux_util is Linux-only.

## Prerequisites on the dev machine

Already present: `node`, `npm`, `cargo`, `rustc`. Tauri's system libraries
(WebKitGTK 4.1 dev packages) are not installed on the host; Stage 1 was built
and launched in a Fedora 45 container. Package lists are in the README.

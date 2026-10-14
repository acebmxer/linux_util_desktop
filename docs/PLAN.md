# Linux_Util Desktop: plan

Built in stages, each ending in something that works and can be committed, so
it is safe to stop between any two. Tick the boxes as work completes.

**Current stage: 0 (groundwork) complete. Next: Stage 1.**

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
- [ ] First commit and GitHub repo (waiting on go-ahead)

### Stage 1: Skeleton
Goal: an empty Linux_Util Desktop window that builds and launches.
- [ ] Scaffold Tauri app (TypeScript, no heavy UI framework unless needed)
- [ ] Window title, app ID, icon placeholder, desktop entry
- [ ] `npm run tauri dev` works; document prerequisites for Fedora, Debian, Arch
- [ ] CI: build on push (Ubuntu runner)

### Stage 2: Read-only list
Goal: see every utility and its install status. Nothing can change the system.
- [ ] Vendor `linux_util.sh` + `lib/` at the latest tag (manual copy for now)
- [ ] Rust command runs `--list --json --no-color`, strips the `Detected:` line
- [ ] UI list with installed badge and version
- [ ] Search box
- [ ] Categories (depends on open question 2)
- [ ] Detected distro shown in the header
- [ ] Light and dark theme following the system

### Stage 3: Actions
Goal: install, uninstall and update from the app.
- [ ] **Spike:** prove the `sudo -A` shim and askpass dialog end to end on Fedora
- [ ] Run one action with live output streamed into a log pane
- [ ] Install, uninstall, update, and the exit-code result
- [ ] Multi-select and a run queue
- [ ] Dry-run toggle
- [ ] Cancel a running action, and block a second concurrent run
- [ ] Error display and a copy-log button

### Stage 4: Sync and contract
Goal: keep the bundled script current without surprises.
- [ ] `scripts/sync_linux_util.sh <tag>` copies a tag into `vendor/` and writes `VERSION`
- [ ] Contract test checks the bundled script against `docs/CONTRACT.md`
- [ ] Scheduled CI job opens a PR when a new linux_util tag appears
- [ ] Test in the linux_util repo or here that guards `--list --json` (decide where)

### Stage 5: Packaging and release
Goal: installable builds on other distros.
- [ ] AppImage, `.deb`, `.rpm` via Tauri bundler
- [ ] Declare the WebKitGTK dependency in `.deb` and `.rpm`
- [ ] Release workflow on tag push, with checksums
- [ ] Smoke-test installs in containers: Ubuntu, Fedora, Arch, openSUSE
- [ ] First tagged release `v0.1.0`

### Stage 6: Polish
- [ ] Profiles (`--export-profile`, `--import-profile`)
- [ ] Update-all button
- [ ] Log viewer for linux_util's logs
- [ ] Settings (config file, theme)
- [ ] Accessibility and keyboard navigation (match the TUI keys where sensible)

### Later (not committed to)
- Update check on launch that offers a newer linux_util
- Arch package (PKGBUILD / AUR)

## Open questions

1. **Run the sudo shim approach?** Confirm in the Stage 3 spike (above).
2. **Categories.** `--list --json` has no category field, but the TUI and README
   group utilities into categories. Options: (a) add a `category` field to the
   JSON in linux_util (a small change there, as `linux_util/list/v2`), or
   (b) keep a mapping in this repo. (a) keeps one source of truth.
3. **Test coverage.** Where should the JSON contract test live?
4. **Pinning policy.** One bundled script version per release, with no runtime
   downloads, as assumed in the README.
5. **Windows/macOS.** Out of scope; linux_util is Linux-only.

## Prerequisites on the dev machine

Already present: `node`, `npm`, `cargo`, `rustc`. Still to check at Stage 1:
Tauri's system libraries (WebKitGTK 4.1 dev packages and friends).

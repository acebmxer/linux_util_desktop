# Linux_Util Desktop

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Version](https://img.shields.io/github/v/tag/acebmxer/linux_util_desktop?label=version&sort=semver&color=brightgreen)](https://github.com/acebmxer/linux_util_desktop/tags)
[![Last commit](https://img.shields.io/github/last-commit/acebmxer/linux_util_desktop)](https://github.com/acebmxer/linux_util_desktop/commits)
[![Issues](https://img.shields.io/github/issues/acebmxer/linux_util_desktop)](https://github.com/acebmxer/linux_util_desktop/issues)
[![Stars](https://img.shields.io/github/stars/acebmxer/linux_util_desktop)](https://github.com/acebmxer/linux_util_desktop/stargazers)
[![Forks](https://img.shields.io/github/forks/acebmxer/linux_util_desktop)](https://github.com/acebmxer/linux_util_desktop/forks)
[![Platform: Linux](https://img.shields.io/badge/platform-linux-333333?logo=linux&logoColor=white)](https://github.com/acebmxer/linux_util)

A desktop app for [linux_util](https://github.com/acebmxer/linux_util). Pick
utilities from a list, click install, uninstall or update, and watch the
output, without opening a terminal.

> **Status: early development.** The app lists every utility with its install
> status, installs, uninstalls and updates them (one, a selection, or everything
> installed), selects them from profiles, shows linux_util's logs, and has a
> Settings window and keyboard shortcuts. Downloads are on the
> [Releases](https://github.com/acebmxer/linux_util_desktop/releases) page. See
> [docs/PLAN.md](docs/PLAN.md) for the staged roadmap and where work currently
> stands.

## Download

Get the AppImage, `.deb` or `.rpm` from the
[latest release](https://github.com/acebmxer/linux_util_desktop/releases/latest).
[Packaging](#packaging) says which file to pick.

## What it is (and isn't)

Linux_Util Desktop is a front end only. It contains **no install logic of its
own**. Every action runs the bundled `linux_util.sh` through its existing
non-interactive flags:

| GUI action | Calls |
|---|---|
| Show utilities and install status | `linux_util.sh --list --json` |
| Install (Apply for system tasks) | `linux_util.sh --install <name>` |
| Uninstall (Revert for system tasks) | `linux_util.sh --uninstall <name>` |
| Update selected | `linux_util.sh --update <name>` |
| Preview without changing anything | adds `--dry-run` |
| Profiles built into linux_util | `linux_util.sh --export-profile` |
| Logs | reads the `logs/` folder linux_util writes |
| Settings for linux_util | writes a `linux_util.conf`, passed in `LINUX_UTIL_CONFIG_FILE` |

Choosing a profile only selects its utilities; they then run through the
same install and update actions. Profile files are imported and exported in
linux_util's own format, so a file saved by the app can be applied on another
machine with `linux_util.sh --import-profile <file>`.

Settings has a theme (follow the system, light or dark) and the linux_util
options that affect the app's runs: answering yes to installer questions
(without a terminal they are answered no), verbose and debug output, and how
long logs are kept. They are saved in the app's config folder
(`~/.config/io.github.acebmxer.linux-util-desktop/`).

While a run is going, a box under its output sends a typed answer to the run,
for questions the package manager itself asks, such as dnf's `Is this ok
[y/N]` or apt's `Do you want to continue? [Y/n]` during **System Updates**.

The window's bottom line shows the app version and the bundled linux_util
version. On launch the app looks at this project's latest stable GitHub release
(or, with **Include beta versions** on in Settings, the newest release including
betas) and, if it is newer, shows a bar with the release notes and an **Install update**
button (turn the launch check off in Settings; **Check for updates** checks on
demand). Nothing is downloaded until you click. The matching file is checked
against the release's `SHA256SUMS` first, then a `.deb` or `.rpm` is installed
with apt-get, dnf or zypper through the same password dialog as the other
actions, or an AppImage file is replaced in place. Restart the app afterwards.
Turning beta versions off goes back to stable releases only; a beta already
installed stays until a newer stable release comes out.
A copy that was not installed from a release file (a source checkout) only
shows the notice.

Keyboard, following linux_util's terminal menu: `/` search, `Esc` clear the
search, `↑` `↓` `Home` `End` move through the list, `Space` select, `←` `→`
previous or next category, `A` select all shown, `D` deselect all.

Because the menu is built from `--list --json`, new utilities added to
`linux_util` appear in the app as soon as its bundled copy is updated. No GUI
change is needed.

## Goals

- **Same distros as linux_util.** Debian/Ubuntu, Fedora, RHEL, Arch and
  openSUSE families. The script handles the distro differences, and the app
  just has to run on all of them.
- **Look the same everywhere.** One interface on GNOME, KDE and the rest,
  mirroring the layout of the linux_util terminal UI (categories, search,
  multi-select).
- **No terminal, no password handling in the app.** Admin rights go through
  the system's own password prompt.
- **Pinned and reproducible.** Each release bundles a known version of
  `linux_util.sh`.

## Tech

[Tauri](https://tauri.app): a native app window that draws a TypeScript/HTML/CSS
interface with the system's built-in web renderer (WebKitGTK on Linux). It is
not a browser tab and runs no local web server. A small Rust shell starts
`linux_util.sh` and streams its output to the window.

## Development

Needs Node.js 22+, npm, Rust (cargo) and Tauri's system libraries:

| Distro | Install |
|---|---|
| Fedora | `sudo dnf install rust cargo webkit2gtk4.1-devel gtk3-devel libsoup3-devel librsvg2-devel openssl-devel gcc pkgconf-pkg-config` |
| Debian / Ubuntu | `sudo apt install curl libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev librsvg2-dev libssl-dev build-essential pkg-config` |
| Arch | `sudo pacman -S rust webkit2gtk-4.1 gtk3 libsoup3 librsvg openssl base-devel` |

On Debian/Ubuntu, install Rust with [rustup](https://rustup.rs) instead of
apt; the packaged `rustc` is often too old for Tauri 2.

Then:

```
npm install
npm run tauri dev
```

### Updating the bundled linux_util

```
scripts/sync_linux_util.sh v1.9.0
npm run contract
```

The sync script replaces `vendor/` with that tag's `linux_util.sh`, `lib/` and
`LICENSE` and records the tag in `vendor/linux_util.VERSION`. The contract test
checks the bundled script still does what [docs/CONTRACT.md](docs/CONTRACT.md)
says the app relies on. A daily workflow does both and opens a PR against `dev`
when linux_util has a newer tag.

## Packaging

Each release has three files, plus `SHA256SUMS`:

| File | For |
|---|---|
| `linux_util_desktop_<version>_amd64.deb` | Debian, Ubuntu and derivatives |
| `linux_util_desktop-<version>-1.x86_64.rpm` | Fedora, RHEL family, openSUSE |
| `linux_util_desktop_<version>_amd64.AppImage` | Any distro, including Arch |

The `.deb` and `.rpm` pull in WebKitGTK, GTK, `bash` and `sudo` through the
package manager. The AppImage carries WebKitGTK itself but, like any AppImage,
uses the desktop's own GTK, fonts and graphics libraries. Flatpak is
intentionally skipped because its sandbox would block the admin rights that
installs need.

When the version in `src-tauri/tauri.conf.json` changes on `main`, the Release
workflow builds all three on Ubuntu 22.04, installs each in clean containers
and attaches them to a **draft** GitHub release for that version, marked
**stable**. Publishing the draft creates the tag, so the files are already
there. Running the workflow by hand on `dev` (Actions → Release → Run workflow)
publishes a **beta** pre-release at once, tagged `v<version>-beta.<n>`, counting up from 1 for each version
and titled with the tag, its notes listing what `CHANGELOG.md` has under
`[Unreleased]`, for testing the real packages before the PR to `main`. Betas are never
"latest"; the app offers them only when **Include beta versions** is on.

To build and smoke-test locally (needs podman or docker for the tests):

```
npx tauri build
scripts/smoke_test.sh ubuntu src-tauri/target/release/bundle/deb
scripts/smoke_test.sh fedora src-tauri/target/release/bundle/rpm
scripts/smoke_test.sh opensuse src-tauri/target/release/bundle/rpm
scripts/smoke_test.sh arch src-tauri/target/release/bundle/appimage
```

The smoke test installs the package with the distro's own package manager,
checks the bundled linux_util and desktop entry, then starts the app as a
normal user on a virtual display and fails if it exits.

## Versioning

Linux_Util Desktop has its **own version tags** (`v0.1.0`, ...), independent of
linux_util's. Each release records which linux_util version it bundles in
`vendor/linux_util.VERSION`.

## Naming

- Repo, directory, binary: `linux_util_desktop`
- Everywhere users see it (window title, docs, releases, desktop entry):
  **Linux_Util Desktop**

## License

MIT, the same as linux_util. See [LICENSE](LICENSE).

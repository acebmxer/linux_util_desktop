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

> **Status: early development.** The app opens an empty window; no features yet.
> See [docs/PLAN.md](docs/PLAN.md) for the staged roadmap and where work
> currently stands.

## What it is (and isn't)

Linux_Util Desktop is a front end only. It contains **no install logic of its
own**. Every action runs the bundled `linux_util.sh` through its existing
non-interactive flags:

| GUI action | Calls |
|---|---|
| Show utilities and install status | `linux_util.sh --list --json` |
| Install | `linux_util.sh --install <name>` |
| Uninstall | `linux_util.sh --uninstall <name>` |
| Update one / all | `linux_util.sh --update <name>` / `--update-all` |
| Preview without changing anything | adds `--dry-run` |

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

## Packaging (planned)

AppImage (any distro), `.deb` and `.rpm`. Flatpak is intentionally skipped
because its sandbox would block the admin rights that installs need.

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

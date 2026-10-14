# Linux_Util Desktop

A desktop app for [linux_util](https://github.com/acebmxer/linux_util). Pick
utilities from a list, click install, uninstall or update, and watch the
output, without opening a terminal.

> **Status: planning.** Nothing is built yet. See [docs/PLAN.md](docs/PLAN.md)
> for the staged roadmap and where work currently stands.

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

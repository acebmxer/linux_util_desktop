# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Each release records
the linux_util version it bundles in `vendor/linux_util.VERSION`. Add new entries
under **[Unreleased]** as work lands; move them into a versioned section when a
release is cut.

## [Unreleased]

### Added

- **Desktop icon, taskbar icon and app menu entry.** On its first launch the
  app offers a desktop icon, a taskbar icon on KDE or GNOME, and when run as an
  AppImage an app menu entry too, which the AppImage never had. The desktop
  icon and menu entry are launcher files pointing at the one installed copy.
  The question is asked once, also after upgrading, and skipped when they are
  already there;  Settings → Shortcuts turns each one on or off.
- **The main window shows when installer questions are answered for you**
  (#13). While Settings → "Answer yes to installer questions" is on, "Answering
  yes to questions" appears next to Dry run; clicking it opens Settings.

### Changed

- **The administrator password is asked once, when the app starts,** as
  linux_util asks before its menu, instead of at the start of every run, even
  runs that needed no administrator rights. It is kept in memory only and asked
  again only once sudo's own timeout has passed with nothing running.
- **The run panel shows the run as a terminal window does.** Output arrives as
  one stream in the order it was written, with linux_util's colours, spinners
  and package-manager progress bars, and "Copy log" copies what is on screen.
  The answer box appears only while a question is waiting: a line or menu
  number with Enter, a single key on its own, or a hidden answer such as a
  password.
- **CI now runs the tests and linters** (#11): `cargo test`, `cargo clippy`
  with warnings as errors, `cargo fmt --check` and `shellcheck`, besides the
  build. The Rust code was reformatted once to the new `src-tauri/rustfmt.toml`.

### Fixed

- **System Updates with nothing to update ended with "Failed: System
  Updates"** in the output, although the run was counted as succeeded. That
  line from linux_util is no longer shown after "No package changes were made."
- **Single-key questions got the wrong answer.** Snapper, Timeshift, Bootloader
  Manager, Xen Guest Utilities and others ask for one key. The app sent the key
  with Enter, and the leftover Enter answered the next question with its
  default. A single-key question now gets just the key.
- **Hidden answers were shown and logged.** Mount SMB Share and Update Mount ask
  for a password with hidden input; the answer box showed it and copied it into
  the log. It is now a password field and the answer is never shown.
- **"Copy log" put each answered question below the lines that came after it.**
- **Every run started with "Script directory is not a git repository; cannot
  self-update."** The bundled copy is updated with the app, so runs now tell
  linux_util to skip its own update check.
- **Installers that ask a question on the terminal were cancelled.** NVIDIA
  Drivers on Fedora asked "enable RPM Fusion now?" by reading `/dev/tty`, which
  the app's scripts did not have, so the install failed with "No such device or
  address". Each script now runs on its own pseudo-terminal, so those prompts
  appear in the run panel and take your typed answer, as in a terminal.

  An item that left a process running in the background, such as the NVIDIA
  install on Fedora, then stayed "running" with no question asked, because the
  app waited for that process to let go of the terminal. The item now ends two
  seconds after the script itself exits.

  Questions written straight to the terminal by programs other than the script,
  such as `chsh` asking for your password in Zsh + Oh My Zsh, never appeared;
  they now show in the run panel like any other question.
- **Reboot now failed on systems with passwordless sudo.** The reboot used
  `sudo` only when a password had been collected during the run, and with
  passwordless sudo none ever is. It then fell back to `systemctl reboot` as the
  user, which needs a desktop authentication agent and is denied where there is
  none, such as Fedora on WSL ("Interactive authentication has not been
  enabled"). It now tries `sudo` first, without a password when none was given,
  and only falls back to plain `systemctl reboot` if that fails.
- **The app crashed on start under WSL.** WebKit segfaulted on WSLg's `dzn`
  graphics driver, so nothing opened from the terminal or the Windows Start
  menu. On WSL the app now renders in software, unless `LIBGL_ALWAYS_SOFTWARE`
  or `WEBKIT_DISABLE_COMPOSITING_MODE` is already set in the environment.
- **One internal error could break every later action** (#13). If a
  background task failed while holding the app's run state, each later click
  (run, cancel, answer, reboot) failed too until the app was restarted. The app
  now carries on with that state.

### Security

- **The administrator password goes only to the app's own runs** (#8). While a
  run was going, any program of the same user could connect to the app's
  password socket and be handed the password. The app now asks the kernel who
  connected and answers only processes it started itself; any other program
  gets nothing, and the run's own sudo still gets the password after it.
- **Release builds no longer read `LU_RELEASES_URL`** (#10). The variable
  pointed the update check at another server and let it download from any
  address; it was meant for testing only. Only debug builds read it now; a
  release build always checks and downloads from this repo's GitHub releases.
- **The app window has a Content Security Policy** (#13). It loads scripts,
  styles and images only from the app itself and talks only to the app's own
  backend, where before no policy was set.

## [0.2.1] - 2026-10-15

### Added

- **The window size and the sidebar width are remembered.** The window opens at
  the size (and maximized state) it was closed with. The sidebar can be dragged
  wider or narrower by its right edge (or with the arrow keys while the edge is
  focused); double-click the edge to go back to the default width.

  Dragging the edge no longer highlights the text around the pointer.

### Fixed

- **Categories and subcategories were in a different order from linux_util.**
  The sidebar followed the order utilities were registered in. It now uses the
  menu's own `CATEGORIES` and `SUBCATEGORY_ORDER`, read from the bundled script,
  so a linux_util sync keeps the two in step.
- **Reboot now asked for the password a second time.** It ran `systemctl reboot`
  as the user, so the desktop's own prompt appeared. It now reuses the sudo
  password given for the run, held in memory only until the reboot offer ends.
- **Run output was red** for everything the script wrote to stderr, including
  dnf's normal progress. Only the app's own errors are red now; the script's
  output shows plain, as in a terminal.
- **Cancel did nothing while the package cache refreshed.** The refresh runs
  under `timeout`, in a process group Cancel did not signal, so a slow or
  retrying mirror kept the run going until it ended. Cancel now also signals
  every process below the script.
- **System Updates with nothing to update** showed as failed (exit 1).
  linux_util's `--install` turns its "success, no changes" result into exit 1,
  so a run that ends with "No package changes were made." now counts as
  succeeded, and does not offer a reboot.

### Added

- **App icon**: a terminal-prompt icon in a hexagon replaces the placeholder
  icon in the window, menu and packages.

  On KDE Wayland the title bar showed a generic icon because the window's ID
  did not match the installed `.desktop` file; it now does.
- **Beta updates**: the Release workflow can now be started by hand on `dev`
  (Actions → Release → Run workflow); it publishes the AppImage,
  `.deb` and `.rpm` as a **beta** pre-release, tagged
  `v<version>-beta.<run number>`. Releases from `main` are now labelled
  **stable**. A new Settings option, **Include beta versions**, makes the
  update check and **Install update** follow the newest release including
  betas; turning it off goes back to stable only. A beta shows as
  `0.2.1-beta.12` in the bottom line, and a stable release is offered to a
  beta of the same version.

- **Description panel**: a panel under the list shows linux_util's description
  of the highlighted utility (by keyboard or mouse), as its menu does, and
  search matches descriptions too. linux_util's `--list --json` does not
  include descriptions, so they are read from the bundled `lib/installers.sh`.
- **Reboot prompt**: when a run finishes and the system has flagged a pending
  reboot, a "Reboot needed" dialog offers **Reboot now** or **Later**, as
  linux_util's own menu does.

  It also appears after any successful System Task, Docker or Drivers item,
  except the ones the menu skips (such as Parallel Downloads), so System
  Updates prompts as it does in the menu.

### Removed

- **Update all** button. linux_util's menu has no such function, and the
  command it ran reinstalled applications (Node.js, Termius, Angry IP Scanner)
  instead of updating them. Update still works on the selected utilities.

### Fixed

- **Updating Enable RDP no longer drops your RDP session**: the update restarts
  xrdp, which ended every open session along with the app and the run inside
  it. While an RDP session is open, a run now skips that restart and says so;
  restart xrdp yourself after logging out.
- **Prompts that never appeared**: a question the script asks, such as "Would
  you like to retry mounting? [y/N]", is now shown in the run panel; before,
  the run waited on it without any sign of why.
- **App update**: Cancel now stops an update at any point, including while
  the release is being checked and while an AppImage downloads; before, an
  AppImage update went ahead and reported success after Cancel. The AppImage
  download goes to a new private folder next to it instead of a fixed file name.
- **Uninstall asks first**: Uninstall (and Revert) lists what will be removed
  and warns that some uninstallers also delete the program's data, such as
  Thunderbird profiles, Bottles prefixes or Podman containers. Dry runs do not ask.
- **Error output** in the run log is shown in the error colour again.
- **Left and right arrows** switch category again after a search leaves a
  category empty.
- **Answering package-manager prompts**: tasks that leave the package
  manager's own confirmation to the user, such as **System Updates** and
  **Full System Upgrade**, failed at once with "Operation aborted by the user"
  because the run had no input. The run panel now has an answer box: a prompt
  shows in the output while it waits, and the typed answer (for example `y`)
  goes to the run, as in a terminal.

## [0.2.0] - 2026-10-14

### Added

- **Versions and updates**: the window's bottom line shows the app version and
  the bundled linux_util version. On launch the app checks this project's
  latest GitHub release (a Settings switch turns this off; a **Check for
  updates** link checks on demand) and, when it is newer, shows a bar with the
  release notes and an **Install update** button. The download is checked
  against the release's `SHA256SUMS`; a `.deb` or `.rpm` is then installed with
  apt-get, dnf or zypper through the password dialog, or the AppImage file is
  replaced in place. A newer linux_util arrives with a new app release.
- **Keyboard shortcuts** following linux_util's terminal menu: `/` search,
  `Esc` clear it, arrows, `Home` and `End` move through the list, `Space`
  selects, left and right switch category, `A` selects everything shown and `D`
  deselects. The list is a single tab stop, controls and dialogs are labelled
  for screen readers, and keyboard focus is always visible.
- **Settings**: theme (follow the system, light or dark), and the linux_util
  options that affect the app's runs: answer yes to installer questions,
  verbose and debug output, and how long logs are kept.
- **Logs**: a Logs window lists linux_util's log files, newest first, and
  shows the one picked, with a copy button.
- **Update all**: one button runs linux_util's `--update-all`, with the dry-run
  toggle. The action bar is now always shown.
- **Profiles**: a Profiles button lists linux_util's built-in profiles
  (Developer Workstation, Home Desktop and the rest). Choosing one selects its
  utilities, ready to install or update; any it names that this distro does not
  offer are left out and listed. Profiles can be imported from a file, and the
  current selection exported to one, in linux_util's own format, so an
  exported file also works with `linux_util.sh --import-profile`.

### Fixed

- Logs left in the bundled linux_util folder by a source checkout are no longer
  copied into the per-user copy that actions run from.

## [0.1.0] - 2026-10-14

### Added

- **Packages**: an AppImage for any distro, a `.deb` for Debian/Ubuntu and an
  `.rpm` for Fedora, the RHEL family and openSUSE, each released with
  `SHA256SUMS`. The `.deb` and `.rpm` pull in WebKitGTK, GTK, `bash` and `sudo`.
  The menu entry is listed under System.

  Under the AppImage, linux_util.sh and everything it starts get the system's
  own libraries and paths, not the AppImage's, and run from the home directory.
  Without that, commands such as `curl`, `git` and `pacman` failed with
  "symbol lookup error".
- **Install, uninstall and update** one or more selected utilities, one at a
  time, with live output in a log pane, the result of each, a dry-run toggle,
  cancel, and a copy-log button. The administrator password is asked for in the
  app's own dialog, checked once and kept in memory only for that run.

  For System Tasks, which change a setting rather than add software, the
  buttons read **Apply** and **Revert** and the status badge reads **Applied**.
- **Utility list** read from the bundled linux_util (v1.9.0): every utility with
  its installed status and version, a category sidebar with sub-category
  headings, a search box, and the detected distro in the header. Follows the
  system's light or dark theme.

> Working note (2026-10-18): plan to fix the run-panel problems found during the NVIDIA install on Fedora 44.

# Run panel: behave like linux_util in a terminal

## Goal

The app is a wrapper. Every item behaves the way it does when you run
`linux_util.sh` in Konsole:

- The password is asked **once**, before anything runs, and never again for
  the rest of the session. The one exception is Zsh, whose `chsh` asks for your
  own password, as it does in a terminal.
- Every question an installer asks shows up and takes the answer it expects:
  a typed line, a menu number, a single key, or hidden input such as a password.
- Nothing in `vendor/` changes. Everything here is on the app side.

If the one-window approach can't pass the tests in Stage 5, the app opens a
plain terminal window instead (see Fallback).

## Issues covered

| # | Problem | Fixed in |
|---|---------|----------|
| 1 | Item stays "running" after the script exits (left-behind background processes, including linux_util's own `sleep 50` sudo keep-alive) | Stage 0, done (`86de17c`) |
| 2 | Password dialog at the start of every run, even for items that don't need sudo | Stage 1, done |
| 3 | "Copy log" puts each answered question below the lines that came after it | Stage 2, done |
| 4 | Answer box shows for the whole run, so it looks as if something is waiting | Stage 3, done |
| 5 | "[WARN] Script directory is not a git repository; cannot self-update." on every run | Stage 4, done |
| 6 | `nvidia-driver-libs.i686` doesn't exist on RPM Fusion | linux_util repo, not here (see end) |

Found while checking for this plan:

- **Single-key questions get the wrong answer.** Snapper, Timeshift, Bootloader
  Manager, Xen Guest Utilities and others ask with `read -n 1`. The app sends
  `y` plus Enter, so the `y` answers that question and the leftover Enter
  answers the *next* one with its default. Stage 3.
- **Hidden input is shown and logged.** Mount SMB Share and Update Mount ask
  for a password with `read -s`. The answer box is a plain text field, and the
  app copies the typed text into the log and into "Copy log". Stage 3.
- **Prompts written straight to the terminal are thrown away.** The app reads
  the script's terminal only to discard what it gets. `chsh` in Zsh writes
  "Password:" there, so the question never appears. Stage 2.
- **Docs that are now wrong**: the `auto_confirm` help in Settings and the
  README say "the app has no terminal to answer in", and the README design
  goals say "No terminal, no password handling in the app". Stage 4.

## Stage 0: stop waiting for background processes (done, `86de17c`)

The item ends 2 seconds after the script itself exits. Output from anything
still running is read but no longer shown. Tested in a Fedora container. Not
yet run on a real machine.

## Stage 1: password once per session (done)

Today the password is kept for one run only. Each item has its own terminal,
and sudo's cached login is tied to a terminal, so a later item can't reuse an
earlier one's login. The app has to supply the password again, which means
holding on to it.

- At launch, before the list loads, the app asks for the password, checks it
  with `sudo -v` and keeps it in memory, never on disk. This is the same
  moment linux_util asks.
- The askpass helper answers every sudo request from memory without a dialog.
  linux_util's own `sudo -v` at the start of each item passes silently.
- **Timeout, as in a normal terminal:** while nothing is running, the password
  is forgotten once the system's sudo timeout has passed. The timeout is read
  from sudo itself at launch (`sudo -V`, "Authentication timestamp timeout"),
  falling back to sudo's default of 5 minutes. A timeout of 0 means ask every
  run; a negative timeout means never forget. The clock starts when a run ends
  and is reset by the next run, so a run in progress never times out. The next
  run after a timeout asks once, at its start.
- A wrong password asks again in the same dialog, as now. If you press Cancel
  at launch, the app still opens for browsing and asks again at the first run.
- Reboot now uses the same password while it is still held, so the "forget
  after the reboot offer" step goes away.

## Stage 2: one terminal stream, as in Konsole (done)

Today stdout and stderr each go to their own pipe and the terminal output is
thrown away. Three separate streams are why answered questions land in the
wrong place, and why `chsh`'s prompt never shows.

- The script's stdout and stderr go to its terminal too, so everything arrives
  in one stream, in the order it was written, exactly as Konsole receives it.
- Terminal echo stays on. A typed answer appears because the terminal echoes
  it, not because the app adds it. Hidden input (`read -s`, `chsh`) stays
  hidden because the program turns echo off, which is what happens in Konsole.
  The app's own echo code is removed.
- Lines redrawn with carriage returns (dnf and apt progress bars) show only
  their final text, as they end up in a terminal.
- `PAGER`, `SYSTEMD_PAGER` and `GIT_PAGER` are set to `cat`, so a command that
  would open a pager on a terminal prints and carries on.
- "Copy log" copies exactly what is on screen.
- The "No package changes were made." check for System Updates moves to the
  single stream. (Since removed: linux_util v1.11.0 exits 3 itself, see
  `docs/CONTRACT.md`.)
- **Colours:** the app stops passing `--no-color` and shows linux_util's own
  colours, as Konsole does (the standard 16 colours plus bold and dim, in
  shades readable on both themes). This replaces the app's red for stderr,
  which one stream can no longer tell apart.

## Stage 3: answer box only when a question is waiting (done)

- The `read` wrapper the app already loads into every script (through
  `BASH_ENV`) tells the app when a `read` starts and ends, and what kind it is:
  a line, N keys (`-n N`), or hidden (`-s`). It sends this as a private
  terminal code that the app strips from the output. linux_util's scripts are
  unchanged. (Since linux_util v1.11.0 it sends the same code itself with
  `LINUX_UTIL_GUI=1`, and the app loads its wrapper only for a run without a
  terminal; see `docs/CONTRACT.md`.)
- The answer box appears only while a question is waiting and disappears when
  it has been answered. Prompts that come from other programs, such as `chsh`
  or a package manager's own `[y/N]`, are caught another way: output that
  stops without a newline and stays quiet for half a second.
- **Line:** the box sends what you type plus Enter, as now. Menu numbers and
  free text work the same way.
- **Single key:** the box takes one key and sends it immediately, with no
  Enter, so there is nothing left over for the next question.
- **Hidden:** the box becomes a password field. What you type is not shown and
  never goes into the log.
- Esc still reaches the script, for the questions where linux_util handles it.

## Stage 4: noise and docs (done)

- Runs set `LINUX_UTIL_NO_SELF_UPDATE=1`, linux_util's own switch for this,
  instead of hiding lines: the check is skipped, one `[INFO]` line says so, and
  no warning goes into linux_util's error log any more. The contract test
  checks the switch is still there.
- Fix the `auto_confirm` help text in Settings, and the README's paragraphs on
  answering questions and on passwords. Bring `docs/PLAN.md`'s privilege
  section up to date.
- CHANGELOG entries for each stage, under `[Unreleased]`.

## Stage 5: tests (container part done)

Done: 28 Rust tests (including real bash on a pty for line, menu, single key
twice, hidden, `chsh`-style and background-process cases); every CI step;
the real app on a virtual display in Fedora as a normal user, running Mount
SMB Share, Snapper, Timeshift and Zsh + Oh My Zsh with every kind of
question, wrong then right password, Cancel at launch, and a 30-second sudo
timeout; the `read` wrapper and sudo's timeout line on Ubuntu 22.04 and Arch.
Not done: a queue of several items in one run with questions, Reboot now with
the held password, typing into the output to open the box, the app's own
update install.

Container (Fedora 44, plus one Debian/Ubuntu and one Arch run), using small
stand-in scripts and real installers where they work in a container:

- A line question, a numbered menu, `read -n 1` twice in a row (the second
  must get its own key), `read -s` (nothing shown or logged), a `chsh`-style
  prompt written to the terminal, a script that leaves a background process.
- Password asked once at launch; three items in one run, plus a second run,
  with no further dialog. A wrong password, then the right one.
- The order in "Copy log" matches the screen.

On your machine (only you can do these):

- NVIDIA Drivers again (RPM Fusion question and driver menu), Snapper or
  Timeshift (single-key questions), Zsh + Oh My Zsh (`chsh` password), Mount
  SMB Share (hidden password).

## Fallback: separate terminal window (not needed)

Only if Stage 2 or 3 can't pass the tests above; they passed. Each item opens in a plain
terminal window (the system's own: Konsole, GNOME Terminal, Ptyxis, xfce4-terminal,
otherwise xterm) running `linux_util.sh --install <item>`, ending with "Press
Enter to close". The app waits for the window to close, reads the result, and
updates the list. The password still comes from the app, so the terminal
doesn't ask again.

## Not in this repo: issue 6

`lib/installers/nvidia_drivers.sh:339` in linux_util installs
`nvidia-driver-libs.i686` for the "latest" driver on dnf. RPM Fusion's 32-bit
package is `xorg-x11-drv-nvidia-libs.i686`. That has to be fixed in linux_util
and then synced here. Not yet checked with a terminal run.

> Delete this file once the work lands.

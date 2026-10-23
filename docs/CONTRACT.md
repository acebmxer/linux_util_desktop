# CLI contract

What Linux_Util Desktop relies on from `linux_util.sh` (bundled: see
`vendor/linux_util.VERSION`). If a linux_util release
changes any of this, the contract test (`npm run contract`,
`scripts/contract_test.mjs`) fails and the sync PR is opened as a draft until
the app is updated.

## Flags used

`--list --json`, `--install <name>`, `--uninstall <name>`, `--update <name>`,
`--update-all`, `--check <name>`, `--dry-run`, `--no-color`, `--version`,
`--export-profile <file>`. `--import-profile` is only checked to exist: the app
writes files in its format but applies profiles through its own run queue.

## Config file

Every run gets `LINUX_UTIL_CONFIG_FILE` pointing at the app's own
`linux_util.conf` (which may not exist; linux_util then uses its defaults). The
app writes only these keys, all read by `load_config` in `lib/config.sh`:
`auto_confirm`, `verbose`, `debug` (`true`/`false`), `log_retention_days`,
`max_logs_per_day` (positive integers). The contract test checks each key is
still parsed and that a file named by `LINUX_UTIL_CONFIG_FILE` is loaded.

## Environment

Every install, uninstall and update run sets `LINUX_UTIL_NO_SELF_UPDATE=1`.
The bundled copy is updated with the app, not by git, so linux_util's own
update check (`self_update_script` in `lib/installers/self_update.sh`) is
skipped; it prints one `[INFO]` line saying so. Runs no longer pass
`--no-color`: the script's output is a terminal and the app shows its colours.

Every run also sets `LINUX_UTIL_GUI=1` (since v1.11.0; see "Questions" below).

## Questions

With `LINUX_UTIL_GUI=1`, `linux_util.sh` loads `lib/front_end.sh`, whose
`read()` writes `ESC ] 7701 ; wait ; <mode> ; <hidden> BEL` to the terminal
just before a question and `ESC ] 7701 ; done BEL` once it is answered.
`<mode>` is `key` for `read -n`/`-N` (a fixed number of keys, no Enter) and
`line` otherwise; `<hidden>` is `1` for `read -s`, else `0`. Reads that are
not from the terminal and short timed reads (`-t 0.05`, the menu's Esc
drain) send nothing. Steps run as `sudo [-E] bash <script>` send them too.
The app strips these from the output and shows the answer box only while a
question waits (`src-tauri/src/actions/output.rs`).

`lib/front_end.sh` is not loaded when `read` is already a function. The app
relies on that for a run it has to start without a terminal (opening one
failed): it then loads its own wrapper through `BASH_ENV`, which sends the
same signals and also prints `read -p` prompts that bash leaves out when
stdin is a pipe.

The contract test checks the loading rule (with `bash -x`), and runs the
script's `read()` on a terminal (util-linux `script`) for a line, a single
key, a hidden answer and a short timed read.

## RDP sessions

Restarting or stopping xrdp ends every open RDP session, including one the
app may be running in. Since v1.11.0 `lib/installers/xrdp.sh` checks for
session processes (`xrdp-sesexec` or `xrdp-chansrv`) itself: install and
update skip the restart with a warning, and uninstall stops with an error and
changes nothing. The app's `sudo` shim no longer looks at these commands. The
contract test runs `update_xrdp` and `uninstall_xrdp` with a process named
`xrdp-sesexec` running and the system commands stubbed.

## Logs

The log viewer reads `logs/` beside the per-user copy: `success_*.log`,
`error_*.log`, `metrics.log`; `*_latest.log` are links to the newest and are
not listed.

## `--list --json`

Observed against linux_util v1.8.0 (shape unchanged in v1.9.0, plus the
category fields below):

```json
{
  "schema": "linux_util/list/v1",
  "utilities": [
    {"name": "NVIDIA Drivers", "installed": true, "version": "615.78.08"},
    {"name": "Enable RDP", "installed": false, "version": null}
  ]
}
```

Relied on: `schema` equals `linux_util/list/v1`; `utilities[]` entries have
`name` (non-empty string, unique), `installed` (bool) and `version` (string or null).

Notes:
- v1.8.0 prints only a blank line before the JSON. The app drops everything
  before the first `{` in case a `Detected: ...` line is added.
- The script creates `logs/` next to itself on every run, including `--list`.
  It still exits 0 when that directory is read-only, but prints errors to stderr.
- Each utility has `category` and `subcategory` (strings, `null` when absent; system tasks are category "System Tasks").
- The JSON carries no category order. The app reads the menu's `CATEGORIES=(...)`
  and `SUBCATEGORY_ORDER["Category"]="A|B"` lines from `lib/installers.sh` and
  adds them as `category_order` / `subcategory_order` (names not listed go last).

## `--export-profile <file>`

With only a file name, writes every built-in profile there (v1.9.0, no `sudo`
needed):

```json
{
  "linux_util_profile_export": true,
  "version": 1,
  "exported_at": "2026-10-14T14:36:08Z",
  "profiles": [
    {"label": "Home Desktop", "description": "...", "utilities": ["Firefox", "Btop"]}
  ]
}
```

Relied on: `linux_util_profile_export` is `true`, `version` is `1`, and each
profile has `label`, `description` (strings) and `utilities` (array of
strings). Names can include utilities not offered on the current distro.

The app writes the same format when it exports a selection. `--import-profile`
reads it with `grep`, not a JSON parser: the file must contain the exact text
`"linux_util_profile_export": true`, and it takes every quoted string after the
first `"utilities"` line as a utility name, so `utilities` has to be the last
key and a file should hold one profile.

## Exit codes and output

Install, uninstall and update run to completion. The app shows the output
live and goes by the exit code only: 0 done, 3 ran fine but had nothing to
change (System Updates with no package updates; v1.11.0 and later print
`No changes: <name>`), anything else failed (1, including a refused `sudo`
password). 3 counts as succeeded and offers no reboot. Before v1.11.0 the
command line turned 3 into exit 1 with `Failed: <name>`, and the app read the
"No package changes were made." line instead; it no longer does. The
contract test runs `_cli_op` from `linux_util.sh` with a stub utility for
each code, and checks System Updates still returns 3 after that line.
Cancelling ends the script by signal, so it has no exit code. `--dry-run` goes before
the action flag and still asks for `sudo` once (package cache refresh). The
script writes `logs/` next to itself, so the app runs a per-user copy.

## Privileges

`linux_util.sh` refuses to run as root and calls `sudo` itself. See PLAN.md,
"Privilege handling".

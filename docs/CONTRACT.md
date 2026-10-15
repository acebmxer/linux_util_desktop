# CLI contract

What Linux_Util Desktop relies on from `linux_util.sh`. If a linux_util release
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

Install, uninstall and update run to completion and exit non-zero on failure.
The app shows stdout and stderr live and reports the exit code. Confirmed in
Stage 3 against v1.9.0: exit 0 on success, 1 on failure (including a refused
`sudo` password); cancelling ends the script by signal, so it has no exit code. `--dry-run` goes before
the action flag and still asks for `sudo` once (package cache refresh). The
script writes `logs/` next to itself, so the app runs a per-user copy.

## Privileges

`linux_util.sh` refuses to run as root and calls `sudo` itself. See PLAN.md,
"Privilege handling".

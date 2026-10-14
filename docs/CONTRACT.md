# CLI contract

What Linux_Util Desktop relies on from `linux_util.sh`. If a linux_util release
changes any of this, the contract test (Stage 4) fails and the sync PR is
blocked until the app is updated.

## Flags used

`--list --json`, `--install <name>`, `--uninstall <name>`, `--update <name>`,
`--update-all`, `--check <name>`, `--dry-run`, `--no-color`, `--version`.

## `--list --json`

Observed against linux_util v1.8.0:

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
`name` (string), `installed` (bool) and `version` (string or null).

Notes:
- v1.8.0 prints only a blank line before the JSON. The app drops everything
  before the first `{` in case a `Detected: ...` line is added.
- The script creates `logs/` next to itself on every run, including `--list`.
  It still exits 0 when that directory is read-only, but prints errors to stderr.
- Each utility has `category` and `subcategory` (strings, `null` when absent; system tasks are category "System Tasks").

## Exit codes and output

Install, uninstall and update run to completion and exit non-zero on failure.
The app shows stdout and stderr live and reports the exit code. To be confirmed
and recorded here in Stage 3.

## Privileges

`linux_util.sh` refuses to run as root and calls `sudo` itself. See PLAN.md,
"Privilege handling".

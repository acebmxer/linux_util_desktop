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
- The command prints a `Detected: ...` line **before** the JSON on stdout. The
  app must strip everything before the first `{` or ask linux_util to drop it.
- There is no category field yet (see open question 2 in PLAN.md).

## Exit codes and output

Install, uninstall and update run to completion and exit non-zero on failure.
The app shows stdout and stderr live and reports the exit code. To be confirmed
and recorded here in Stage 3.

## Privileges

`linux_util.sh` refuses to run as root and calls `sudo` itself. See PLAN.md,
"Privilege handling".

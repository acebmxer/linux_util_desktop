# Security policy

## Supported versions

| Version | Supported |
| --- | --- |
| Latest release | :white_check_mark: |
| Older releases | :x: |

Linux_Util Desktop is pre-1.0. Fixes go into the next release rather than being
backported. Install updates with **Install update** in the app, or from the
[latest release](https://github.com/acebmxer/linux_util_desktop/releases/latest).

## Reporting a vulnerability

Report privately through GitHub, not in a public issue:

1. Open the [Security tab](https://github.com/acebmxer/linux_util_desktop/security)
   of this repository.
2. Choose **Report a vulnerability**.
3. Describe the issue, how to reproduce it, and what an attacker gains.

Expect an acknowledgement within a few days. Please give a reasonable window
for a fix before disclosing publicly.

## Scope

In scope: how the app handles the administrator password (asking, holding it
in memory, passing it to sudo); the update check, download and signature
verification; anything that lets another program or user on the machine run
commands through the app.

Out of scope: problems in an installer itself, which also happen when
`linux_util.sh` runs on its own; report those to
[linux_util](https://github.com/acebmxer/linux_util/security). Also out of
scope: anything requiring an attacker to already have your password or root.

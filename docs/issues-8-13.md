> Working note: plan for GitHub issues #8–#13. Not user documentation.

# Issues #8–#13: findings and fix plan

## Status (2026-10-19)

Work is on `dev` (all of it; no other branch). Each done item has a
CHANGELOG entry under [Unreleased].

| Step | Issue | State |
|---|---|---|
| 1 | #10 `LU_RELEASES_URL` debug-only | Done. `releases_url_from()` + test `release_builds_ignore_the_test_address`; PLAN.md Stage 7 sentence fixed. |
| 2 | #11 CI checks | Done. Clippy fix in `settings.rs` test; `src-tauri/rustfmt.toml` (`max_width = 140`, `use_small_heuristics = "Max"`: smallest diff of the widths tried) and a one-time `cargo fmt`; `build.yml` adds shellcheck, fmt, clippy `-D warnings`, `cargo test`. Replayed in an ubuntu:24.04 container: all pass. Not yet run on GitHub. |
| 3 | #13 small items | Done. `lock()` helper (22 call sites in app code; tests keep `unwrap`) + test; CSP set in `tauri.conf.json`; "Answering yes to questions" marker in the action bar (`#autoconfirm`, `showAutoConfirm()`). Checked in a build using the bundled frontend: no CSP violations (probe with a deliberate control), terminal colours fine, marker shows/hides on load and on Save. |
| 4 | #8 askpass peer check | Done. `peer_cred()` (SO_PEERCRED), `parent_pid()`, `descends_from()`, `answer_askpass()`; `serve_askpass` uses them. **Differs from plan:** the root is the app's own pid, not `inner.pid`, because `inner.pid` is set only after spawn and an early sudo would race it; this also covers the self-update's sudo. Unit tests + ignored `real_sudo_gets_the_password_only_inside_the_run`, which passed on Fedora 43 and Ubuntu 24.04 containers (direct, inside `timeout`, nested, stray refused, later request still works). |
| 5 | #9 signed releases | **Not started.** Needs a minisign key pair; secret key (and password) as a repo secret. |
| 6 | #13 module split, aarch64 | **Not started.** Ask before each. |
| 7 | #12 upstream contract | **Not started.** Raise in linux_util (see Step 7). |

Still to verify by hand: a real run that needs the sudo password (prompt,
"Wrong password" retry, cancel, one prompt per session) on the host, and the
self-update package install. Needs the real password, so done by the user.

Test helpers (outside the repo, in `~/.hermes/notes/linux_util_desktop/`):
`lud-ci.sh <ref>` replays `build.yml` in ubuntu:24.04 against a git ref (runs
as root, so its `npm run contract` step is skipped; fine on GitHub);
`lud-askpass.sh fedora|ubuntu` runs the ignored real-sudo test against the
current working tree.

Each issue was checked against the code on `dev` (0.3.0, `52f5385`) on
2026-10-19. One step per issue, smallest-risk first. Each step gets its own
CHANGELOG entry and keeps the existing behaviour named under "Must not break".

## Findings

| # | Claim | Valid? | Notes |
|---|---|---|---|
| 8 | Askpass socket answers any same-user process | Yes | `serve_askpass` (`actions.rs`) writes the password to every client that connects; no peer check. The 0700 dir only stops other users. Same-user attackers have other routes too (they can edit the scripts in that same dir), so this closes the easy route, not every route. |
| 9 | Updates trust a `SHA256SUMS` from the same release | Yes | `fetch_verified` (`update.rs`) downloads the sums from the release it verifies. Catches corruption, not a replaced release. |
| 10 | `LU_RELEASES_URL` lifts the download check in shipped builds | Yes | `releases_url` reads it unconditionally; `download_allowed(_, true)` accepts any URL. Low risk (setting the app's environment already needs the user's account), but it is a test hook in release builds. |
| 11 | CI never runs `cargo test`, clippy, fmt or shellcheck | Yes | `build.yml` runs contract, `npm run build`, `cargo build` only. Current results of each check: see "CI baseline" below. |
| 12 | App depends on linux_util's output wording | Yes | All four exist: `NO_CHANGES_LINE` + exit 1 → 3 rewrite, `looks_like_question` idle guess, the `BASH_ENV` `read` wrapper (7701 signals), xrdp case in the sudo shim. The fixes belong in linux_util; this repo only adds the consuming side. |
| 13 | Minor cleanups | Mostly | CSP is `null`, and no `innerHTML`/`eval` in `src/`: true. File sizes: true (1,656 / 1,286 lines). "~75 `lock().unwrap()`": overstated, there are 25, all in `actions.rs` (75 is every `unwrap()`). aarch64: `plan()` refuses non-x86_64 and `release.yml` builds amd64 only. Auto-confirm only shows in Settings: true. |

## CI baseline

Run in a `rust:1-bookworm` container against `dev` with the same steps as `build.yml`:

| Check | Result |
|---|---|
| `npm run contract`, `npm run build` | Pass |
| `cargo test` | Pass, 34 tests |
| `shellcheck scripts/*.sh` | Pass |
| `cargo clippy --all-targets -- -D warnings` | Fails on one lint: `field_reassign_with_default`, test setup in `settings.rs:173`. Nothing in non-test code. |
| `cargo fmt --check` | Fails: 140 hunks in every `.rs` file. There is no `rustfmt.toml` and the code uses lines up to 175 characters, so rustfmt's 100-character default would rewrap most files. |

## Plan

### Step 1: #10, test hook out of release builds

- `update.rs`: read `LU_RELEASES_URL` only under `#[cfg(debug_assertions)]`;
  release builds always use the GitHub API and the repo's download prefix.
- Fix the comment on `RELEASES_URL_ENV` and the `LU_RELEASES_URL` sentence in
  `docs/PLAN.md` (Stage 7) to say debug builds only.
- Tests: `only_this_repos_releases_download` still passes (tests build with
  debug assertions); add one that a release-mode `releases_url` ignores the var.
- Must not break: update check (stable and beta channels), all three install
  methods. Testing an update against a local server now needs a debug build.

### Step 2: #11, CI checks

- Clippy: build the test's `Settings` with a struct literal instead of assigning
  after `Default::default()`. The test's setup changes; what it asserts does not.
- fmt (decided): add `src-tauri/rustfmt.toml` with a `max_width` near the
  current style, picked by the smallest `cargo fmt` diff, then reformat once in
  a commit of its own with no other changes.
- Then add to `build.yml`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`, `shellcheck scripts/*.sh`.
- `cargo test` includes the pty tests, which need `bash` and a pty; both exist
  on `ubuntu-24.04` runners. Run the full workflow locally in a container first.
- Must not break: nothing at runtime. Only clippy fixes touch code; each one
  is a no-behaviour change, and the test suite runs after them.

### Step 3: #13, low-risk items

- **Mutex poisoning**: replace the 25 `lock().unwrap()` with a small helper
  (`lock(&m)` → `unwrap_or_else(PoisonError::into_inner)`), not a new crate.
  Same behaviour unless a thread panicked; afterwards the app keeps working
  instead of every command panicking.
- **CSP**: set `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline';
  img-src 'self' data:; connect-src ipc: http://ipc.localhost` (Tauri 2's IPC).
  Check in the built app, not `vite dev`: every window (main, settings, run
  panel, password dialog, update dialog), the terminal renderer's colours,
  and the devtools console for CSP violations.
- **Auto-confirm visible**: a small marker in the main window (next to the run
  button / run panel header) while `auto_confirm` is on, linking to Settings.
  Matches the existing UI style; no new behaviour.
- Must not break: every run path (install, uninstall, update, update-all,
  check, profile import/export, reboot), the password dialog, the updater.

### Step 4: #8, askpass peer check

- In `serve_askpass`, read the peer with `SO_PEERCRED` (`libc::getsockopt`)
  and answer only when its uid is ours and its pid is a descendant of the
  current item's child (`inner.pid`, using the existing `descendants()`).
  Otherwise drop the stream (askpass exits 1, as on cancel) without marking the
  run denied, so a stray client cannot block the real sudo.
- No env token: `/proc/<pid>/environ` is readable by the same user, so it adds
  nothing over the peer check.
- No `zeroize` (decided): the password also passes through the webview and
  other copies, so wiping one copy adds little for a new crate.
- Tests: a client in the run's tree gets the password; an unrelated process
  gets nothing and the next real request still works.
- Must not break: sudo inside `timeout` (own process group, still a
  descendant), sudo from nested scripts, the "Wrong password" retry loop,
  cancel, the session timeout, the self-update's package install. Check a
  real install on Fedora and Ubuntu in containers.
- Risk: any linux_util item that calls sudo from a process detached from the
  run tree (double fork, `setsid`, `systemd-run`) would start failing. In
  `vendor/` (v1.10.0) the only detached launch is JetBrains Toolbox starting
  its own app, which does not call sudo. Re-check after each vendor sync.

### Step 5: #9, signed releases

- Decided: **minisign** on `SHA256SUMS`, verified with the `minisign-verify`
  crate. The existing update code stays; only the signature check is added.
- Needs from you before building: a minisign key pair, with the secret key
  (and its password, if any) added as a repo secret. I will give the exact
  commands at that step.
- `release.yml`: sign `SHA256SUMS` with a secret key held as a repo secret,
  attach `SHA256SUMS.minisig`.
- `update.rs`: embed the public key; refuse to install when the signature is
  missing or wrong.
- Limit to state in the issue: a key in repo secrets protects against a
  replaced release file, not against someone with full control of the account
  who can run a workflow with that secret.
- Upgrade path: releases made before this have no signature, so the first
  signed version must still be installable by the previous app (it only
  checks the sums, which still exist). Old apps keep working; only new apps
  require the signature.
- Must not break: beta channel, all three install methods, the smoke matrix.

### Step 6: #13, larger items (ask before each)

- **Module split** of `actions.rs` / `main.ts`: pure move, no behaviour change,
  done after steps 1–5 land so their diffs stay readable. Its own PR.
- **aarch64**: new capability (minor version). Needs an arm64 job in
  `release.yml` (`ubuntu-22.04-arm`), arm asset names in `plan()`, the arch
  check lifted, and the README platform line. Smoke tests on arm containers.

### Step 7: #12, upstream contract

Not fixed here: the changes are in linux_util. To raise there:

1. A distinct exit code for "nothing to do" (3 is already treated as success
   by the app).
2. linux_util emitting the 7701 wait/done signals itself when run with
   `LINUX_UTIL_GUI=1`.
3. The xrdp session check inside linux_util's own restart step.

Once a linux_util release has them and is synced into `vendor/`: document
them in `docs/CONTRACT.md`, add checks to `scripts/contract_test.mjs`, and keep
the app's current heuristics as a fallback for older vendored copies.

---

Delete this file once the work lands.

# Contributing to Linux_Util Desktop

Thank you for taking the time to contribute!

## Where things belong

Linux_Util Desktop is a front end for
[linux_util](https://github.com/acebmxer/linux_util) and has no install logic
of its own. New utilities, and installers that also misbehave when
`linux_util.sh` runs on its own in a terminal, belong in linux_util. Problems
that happen only in the app belong here.

`vendor/` is a copy of linux_util. Never edit it by hand; update it with
`scripts/sync_linux_util.sh <tag>` (see the README's
[Updating the bundled linux_util](README.md#updating-the-bundled-linux_util)).

## Issues

Use the issue templates: **Bug report** or **Feature request**. Security
problems go through the Security tab instead; see [SECURITY.md](SECURITY.md).

## Development setup

The README's [Development](README.md#development) section lists what to
install. Then:

```
npm install
npm run tauri dev
```

## Before opening a pull request

The same checks CI runs:

```
npm run contract
npm run build
shellcheck scripts/*.sh
cargo fmt --manifest-path src-tauri/Cargo.toml --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
```

Every user-facing change gets an entry in `CHANGELOG.md` under
`## [Unreleased]` (`### Added` / `### Changed` / `### Fixed`, newest first).
CI, templates and other repo plumbing get none.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`,
`docs:`, `test:`, `refactor:`, `chore:`.

## Submitting a pull request

All work lands on the `dev` branch first. `main` only receives release pull
requests from `dev`, which the maintainer opens, so please never target `main`.

1. Fork the repository and create a branch from `dev`.
2. Make your changes and run the checks above.
3. Open a pull request against `dev` (choose `dev` as the base branch on
   GitHub), filling in the PR template. A pull request opened against `main`
   will be retargeted to `dev` before it is merged.

For larger changes it is worth opening an issue first to discuss the approach.

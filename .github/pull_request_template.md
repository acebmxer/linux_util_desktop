## Summary

<!-- One or two sentences describing what this PR does and why. -->

## Type of change

<!-- Check all that apply -->

- [ ] Bug fix
- [ ] New feature or enhancement
- [ ] Bundled linux_util update (`scripts/sync_linux_util.sh`)
- [ ] Packaging / release
- [ ] Documentation
- [ ] Other: <!-- describe -->

## Checklist

Tick only what you actually did.

- [ ] This pull request targets the `dev` branch (not `main`)
- [ ] `vendor/` was changed only through `scripts/sync_linux_util.sh` (or not at all)
- [ ] `npm run contract` passes
- [ ] `npm run build` passes
- [ ] `cargo fmt --check`, `cargo clippy -- -D warnings` and `cargo test` pass (in `src-tauri/`)
- [ ] `CHANGELOG.md` has an entry under `## [Unreleased]` (skip for CI, templates and other repo plumbing)
- [ ] `README.md` / `docs/` updated if behaviour changed

## Testing done

<!-- What you ran, on which distro, and what you did not run. -->

## Related issues

<!-- Fixes # -->

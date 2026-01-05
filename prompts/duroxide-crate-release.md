# Duroxide Crate Release Checklist

Use this prompt when preparing a crates.io release. Run it end-to-end and paste results in PR/issue as needed.

## Preconditions
- Workspace clean: `git status` empty (aside from intentional release files)
- All tests pass: `cargo test`, `cargo nextest run`, `cargo test --doc`
- Tooling available: `cargo-release` (if used), `just` (if used)

## Release Prep Steps
> Ask the author whether the tests below were already run before re-running anything.

1) **Sync main**
   - `git fetch origin && git checkout main && git pull`
   - Rebase your release branch onto main.

2) **Version bump**
   - Update `Cargo.toml` version.
   - Update `CHANGELOG.md` with date and highlights.
   - If workspace members exist, sync versions across crates.

3) **Dependency audit**
   - `cargo update -p <dep>` for targeted bumps if needed.
   - `cargo deny check` (if configured) or `cargo audit`.

4) **Build & test matrix**
   - `cargo fmt --all`
   - `cargo clippy --all-targets --all-features -- -D warnings`
   - `cargo test`
   - `cargo nt` (nextest + provider validation tests)
   - `cargo test --doc`
   - Optional: `cargo test --features provider-test`

5) **Artifacts**
   - `cargo package --locked` (verifies package can be built)
   - Inspect `cargo package --list` for unwanted files.

6) **Tag & publish**
   - Tag: `git tag vX.Y.Z && git push origin vX.Y.Z`
   - Publish: `cargo publish --locked`
   - If publish fails, yank tag or release as needed.

7) **Post-publish**
   - Create GitHub release notes (link to changelog).
   - Announce or update docs/examples if needed.

## Release Validation Prompt (for Copilot / reviewers)
Use this condensed prompt to validate a release PR:

- Verify version bump in `Cargo.toml` and `CHANGELOG.md` date/notes.
- Ensure tests/clippy/fmt/docs were run (check CI or commands). Missing? Ask for runs.
- Confirm `cargo package --locked` passes and package contents exclude junk (target/, tmp, tests assets unless needed).
- Check for workspace member version alignment (if any).
- Ensure tag instruction matches version (vX.Y.Z).
- Confirm no API-breaking changes are undocumented.

## Notes
- Keep release PRs small: version bump + changelog + mechanical updates only.
- For hotfixes, branch from the last tagged release.
- If using `cargo-release`, align steps with its config (pre-release hooks, tagging).

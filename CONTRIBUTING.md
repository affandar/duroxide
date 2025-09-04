# Contributing

Thanks for your interest in improving duroxide!

Before submitting changes, please review this checklist:

- Code
  - [ ] Build passes locally (`cargo test`)
  - [ ] Lints/clippy (if applicable) are clean
  - [ ] Tests added or updated for behavior changes
- Documentation
  - [ ] Does this change affect existing docs? Update them.
  - [ ] Is this a new surface area or concept? Add a doc under `docs/`.
  - [ ] Link new docs from `docs/README.md`.
- Design notes (optional but encouraged)
  - [ ] For non-trivial changes, include a short design rationale in the PR description with code pointers.

## Development workflow

- Write or update tests first (happy path + 1-2 edge cases).
- Keep public APIs stable where possible; note breakages clearly.
- Prefer small, focused commits with descriptive messages.

## Running tests

```
cargo test
```

## Filing PRs

Use the PR template in `.github/pull_request_template.md`. Fill in the docs checkboxes.

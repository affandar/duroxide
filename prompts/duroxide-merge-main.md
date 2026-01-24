# Merge Branch to Main

## Objective
Merge the current branch into `main` using a **full squash merge**.

**Policy:** Every merge to `main` must result in **exactly one commit** on `main` that contains the entire branch’s change set.
That single commit message must include a **comprehensive summary** of all changes included in the squash.

## Steps

### 1. Review Current Changes
- Run `git status` to see all uncommitted changes
- Review the changes to ensure they're ready for commit
- Check that no temporary files or debug code is included

### 2. Prepare for Squash Merge

You may keep WIP commits on the feature branch if you want, but the merge into `main` must still be a **single squash commit**.

Before merging:
- Ensure the working tree on your branch is clean (commit or stash any local edits).
- Run formatting and linting:
  - `cargo fmt --all`
  - `cargo clippy --all-targets --all-features`
- Ensure tests pass:
  - `cargo nt`

### 3. Squash Merge to Main (Required)

Perform a **squash merge** so that the branch becomes **one commit** on `main`:

- Checkout main: `git checkout main`
- Pull latest: `git pull origin main` (if working with remote)
- Squash merge your branch (no merge commit created yet):
  - `git merge --squash your-branch-name`
- Create the single merge commit on `main`:
  - `git commit -m "<single comprehensive message>"`

The squash commit message must:
- Use imperative mood in the title (e.g., "Implement X" not "Implemented X")
- Summarize **all** meaningful changes from the branch (bulleted list is encouraged)
- Call out any breaking changes or migrations
- Reference related issues/PRs where relevant

⚠️ Do **not** use a regular merge commit (no `git merge your-branch-name` without `--squash`).

### 4. Push to Remote
- Push main: `git push origin main`
- Verify the push succeeded

## Guidelines
- **DO NOT** use `--force` or `--force-with-lease` unless explicitly instructed
- **DO NOT** skip commit hooks with `--no-verify`
- **DO** ensure all tests pass before merging: `cargo nt`
- **DO** run `cargo fmt --all` and `cargo clippy --all-targets --all-features` before merging
- **DO** ensure the squash commit message fully covers the branch

## Example Commit Messages

Good squash merge commit (single commit on `main`):
```
Add ActivityContext with tracing support

- Introduce ActivityContext as required first parameter for activities
- Add trace_info/warn/error/debug methods with correlation fields
- Update all activity registrations across tests and examples
- Add metadata accessors for instance_id, execution_id, etc.
```

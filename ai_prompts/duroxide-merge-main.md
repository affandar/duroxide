# Merge Branch to Main

## Objective
Commit all changes on the current branch and merge into `main` with proper commit messages.

## Steps

### 1. Review Current Changes
- Run `git status` to see all uncommitted changes
- Review the changes to ensure they're ready for commit
- Check that no temporary files or debug code is included

### 2. Commit Current Branch
- Create a descriptive commit message that:
  - Summarizes what was changed/added/fixed
  - Uses imperative mood (e.g., "Add feature" not "Added feature")
  - Includes context if the change is part of a larger effort
- Stage all relevant changes: `git add -A`
- Commit: `git commit -m "Your message here"`

### 3. Merge to Main
- Checkout main: `git checkout main`
- Pull latest: `git pull origin main` (if working with remote)
- Merge your branch: `git merge your-branch-name`
- The merge commit message should:
  - Summarize ALL work done in this branch beyond main
  - Highlight key features/changes added
  - Note any breaking changes
  - Reference related issues/PRs if applicable

### 4. Push to Remote
- Push main: `git push origin main`
- Verify the push succeeded

## Guidelines
- **DO NOT** use `--force` or `--force-with-lease` unless explicitly instructed
- **DO NOT** skip commit hooks with `--no-verify`
- **DO** ensure all tests pass before merging: `cargo test --all`
- **DO** run `cargo fmt --all` and `cargo clippy --all-targets` before committing
- **DO** create meaningful commit messages that future readers can understand

## Example Commit Messages

Good branch commit:
```
Add ActivityContext with tracing support

- Introduce ActivityContext as required first parameter for activities
- Add trace_info/warn/error/debug methods with correlation fields
- Update all activity registrations across tests and examples
- Add metadata accessors for instance_id, execution_id, etc.
```

Good merge commit:
```
Merge observability-implementation: Add comprehensive metrics and logging

This branch implements full observability for duroxide:

- OpenTelemetry metrics for activities, orchestrations, providers
- Structured logging with correlation IDs (instance_id, execution_id, etc.)
- ActivityContext for activity-level tracing
- Default log filtering (warn + orchestration/activity traces)
- Metrics test coverage with simulated infrastructure failures
- Updated all documentation and examples

Breaking changes:
- ActivityContext now required as first parameter for all activities
- Default log format changed to Compact
```

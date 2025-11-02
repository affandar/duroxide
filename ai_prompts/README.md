# AI Prompts for Duroxide Development

This directory contains structured prompts for LLMs working on the duroxide codebase. Each prompt provides comprehensive guidance for common development tasks.

## Available Prompts

### Core Development Tasks

1. **[duroxide-add-feature.md](duroxide-add-feature.md)**
   - Adding new features to the framework
   - API design considerations
   - Testing strategy
   - Documentation requirements

2. **[duroxide-fix-bug.md](duroxide-fix-bug.md)**
   - Diagnosing and reproducing bugs
   - Root cause analysis
   - Writing regression tests
   - Fixing without breaking existing behavior

3. **[duroxide-add-test.md](duroxide-add-test.md)**
   - Test organization and patterns
   - Coverage goals by test type
   - Writing deterministic tests
   - Avoiding flaky tests

4. **[duroxide-refactor.md](duroxide-refactor.md)**
   - Improving code quality
   - Safe refactoring patterns
   - Maintaining determinism
   - Performance optimizations

### Quality and Maintenance

5. **[duroxide-clean-warnings.md](duroxide-clean-warnings.md)**
   - Eliminating compiler warnings
   - Running clippy
   - Proper handling of unused code
   - Code formatting

6. **[duroxide-update-docs.md](duroxide-update-docs.md)**
   - Comprehensive documentation review
   - Keeping guides accurate
   - API documentation standards
   - Example validation

### Git Operations

7. **[duroxide-merge-main.md](duroxide-merge-main.md)**
   - Committing changes
   - Merging branches
   - Writing good commit messages
   - Pushing to remote

## How to Use These Prompts

### For LLM Assistants
Reference the appropriate prompt when tackling a task:

```
@duroxide-add-feature.md

I need to add support for orchestration priorities in the queue.
```

### For Humans
Use these as checklists and guidelines when:
- Onboarding new contributors
- Planning complex changes
- Reviewing pull requests
- Establishing coding standards

## Prompt Design Principles

These prompts follow consistent patterns:

1. **Clear Objective** - What are we trying to accomplish?
2. **Structured Steps** - Ordered tasks to complete the objective
3. **Examples** - Show correct patterns vs anti-patterns
4. **Checklists** - Ensure nothing is missed
5. **Quality Gates** - Validation steps before considering done

## Adding New Prompts

When creating new prompts:

### Good Prompt Topics
- Implementing new provider (postgres, redis, etc.)
- Adding new orchestration pattern
- Performance profiling and optimization
- Security review and hardening
- Upgrading dependencies

### Prompt Template
```markdown
# Task Name

## Objective
Clear statement of what we're trying to accomplish

## Planning Phase
- Understand requirements
- Identify affected components
- Plan approach

## Implementation Phase
- Step-by-step instructions
- Code patterns and examples
- Common pitfalls to avoid

## Validation Phase
- Testing checklist
- Quality checks
- Documentation updates

## When to Ask
Scenarios where human input is needed
```

## Integration with Development Workflow

### Feature Development Cycle
```
1. @duroxide-add-feature.md     - Design and implement
2. @duroxide-add-test.md        - Add test coverage
3. @duroxide-update-docs.md     - Update documentation
4. @duroxide-clean-warnings.md  - Polish and clean
5. @duroxide-merge-main.md      - Commit and merge
```

### Bug Fix Cycle
```
1. @duroxide-fix-bug.md         - Reproduce and fix
2. @duroxide-add-test.md        - Add regression test
3. @duroxide-update-docs.md     - Update if behavior changed
4. @duroxide-merge-main.md      - Commit and merge
```

### Refactoring Cycle
```
1. @duroxide-add-test.md        - Ensure coverage first
2. @duroxide-refactor.md        - Make improvements
3. @duroxide-clean-warnings.md  - Clean up
4. @duroxide-merge-main.md      - Commit and merge
```

## Suggested Additional Prompts

Consider creating these prompts for specialized tasks:

- **duroxide-add-provider.md** - Implementing new storage backends (PostgreSQL, DynamoDB, Redis)
- **duroxide-performance-profile.md** - Profiling and optimizing performance
- **duroxide-add-example.md** - Creating comprehensive runnable examples
- **duroxide-review-pr.md** - Guidelines for reviewing pull requests
- **duroxide-release.md** - Steps for cutting a new release
- **duroxide-benchmark.md** - Adding and running benchmarks
- **duroxide-security-review.md** - Security considerations and review checklist

## Quality Standards for Prompts

All prompts should:
- Be actionable (clear steps, not vague suggestions)
- Include examples (both good and bad patterns)
- Have validation steps (how to know you're done)
- Reference actual code when possible
- Be maintainable (update as codebase evolves)

## Feedback Loop

If a prompt is frequently insufficient:
1. Note what questions came up
2. Note what was unclear
3. Update the prompt with those answers
4. Add examples addressing the confusion

## Version Control for Prompts

- Prompts should evolve with the codebase
- When making breaking changes to duroxide, update affected prompts
- Keep prompts in sync with ORCHESTRATION-GUIDE.md and other docs
- Version prompts if we ever publish them separately


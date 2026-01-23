# Duroxide AI Skills

This folder contains AI skills (context/prompts) that can be installed into AI coding assistants to enable effective development with Duroxide.

## Available Skills

| Skill | Description | For |
|-------|-------------|-----|
| [duroxide-provider-implementation.md](duroxide-provider-implementation.md) | Implementing custom storage providers | Provider developers |

## Installing Skills

### VS Code with GitHub Copilot

VS Code supports multiple instruction methods:

**Agent Skills** (recommended for specialized workflows):
1. Create `.github/skills/duroxide-provider/` in your project
2. Copy skill file as `SKILL.md` with this header format:
   ```yaml
   ---
   name: duroxide-provider
   description: Guidance for implementing Duroxide storage providers
   ---
   ```
3. Enable with `chat.useAgentSkills` setting (preview feature)

**Custom Instructions** (for project-wide guidelines):
1. Copy skill content to `.github/copilot-instructions.md` for repository-wide rules
2. Or create `.github/instructions/duroxide.instructions.md` with glob patterns:
   ```yaml
   ---
   applyTo: "src/providers/**"
   ---
   ```

### Claude Code

Claude Code uses `CLAUDE.md` files for project context:

1. **Project-level**: Copy skill content to `./CLAUDE.md` or `./.claude/CLAUDE.md`
2. **Modular rules**: Copy skill file to `./.claude/rules/duroxide-provider.md`
3. **User-level**: Copy to `~/.claude/CLAUDE.md` for all projects

Claude Code automatically loads these files. Use `/memory` command to view loaded files.

### Cursor

Cursor supports rules in `.cursor/rules/`:

1. **Project rules**: Copy skill file to `.cursor/rules/duroxide-provider.mdc`
2. **With metadata** (`.mdc` format): Add frontmatter:
   ```yaml
   ---
   description: "Duroxide provider implementation guidance"
   globs: ["src/providers/**"]
   alwaysApply: false
   ---
   ```
3. **Simple alternative**: Copy to `AGENTS.md` in project root
4. **User rules**: Add to `Cursor Settings → Rules` for global application

### Other AI Assistants

Skills are standard Markdown files. Copy relevant sections into your AI assistant's system prompt or context window. The [Agent Skills](https://agentskills.io/) open standard is supported by many tools including Roo Code, OpenCode, Goose, and others.

## Skill Structure

Each skill file contains:

- **Frontmatter** — Metadata (title, version, scope)
- **Summary** — Quick overview of the skill's purpose
- **Key Concepts** — Core ideas the AI should understand
- **Implementation Guidance** — Specific patterns and requirements
- **References** — Links to detailed documentation

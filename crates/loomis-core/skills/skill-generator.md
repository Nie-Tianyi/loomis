---
name: skill-generator
description: Create new skills for Loomis. Use when the user asks to create, author, write, or set up a new skill — or when you need to capture a recurring workflow as a reusable skill.
---

# Skill Generator

You can create new skills to extend Loomis's capabilities. Skills are Markdown files with YAML frontmatter that provide specialized instructions for the LLM agent. When a user asks you to "make a skill" or "turn this into a skill", follow this process.

## Skill File Format

Each skill is a single `.md` file placed in `.loomis/skills/` in the project workspace root. The format is:

```markdown
---
name: my-skill-name
description: One-line description of what this skill does and when to trigger it.
---

Markdown body with full skill instructions...
```

**Required frontmatter fields:**
- `name`: kebab-case identifier (e.g., `code-review`, `pdf-extract`). Used as the lookup key in the `skill` tool.
- `description`: Short description (ideally 10-30 words). This is the primary triggering mechanism — the LLM reads this to decide whether to load the skill. Include when-to-use context, not just what-it-does.

**Body**: Standard Markdown. Loaded as a System message when the skill is activated. Write clear, imperative instructions.

## Where Skills Live

- Project skills: `.loomis/skills/` in the workspace root (these take priority)
- User skills: `~/.loomis/skills/` (global fallback, lower priority than project skills)

Put project-specific skills in `.loomis/skills/`. Skills in this directory are discovered on Loomis startup and listed in the system prompt.

## Writing a Good Skill

### Anatomy of Instructions

1. **Explain the why**: Today's LLMs respond better to reasoning than rigid rules. Instead of "ALWAYS do X", explain why X matters and let the model generalize.
2. **Be specific but not narrow**: Include concrete steps, examples, and output formats. But avoid overfitting to a single use case — skills should be reusable.
3. **Use imperative tone**: "Read the file first", "Verify with grep", "Write the output to a file."
4. **Define output formats clearly**: Show templates and examples rather than vague descriptions.
5. **Keep focused**: A skill should teach ONE workflow or domain well. If it grows past ~300 lines, consider splitting or adding references.

### Description Best Practices

The description is the primary trigger. Combat under-triggering by making descriptions slightly "pushy":
- Include trigger phrases and contexts: "Use when the user mentions X, Y, or Z"
- Include near-miss guidance: "Do NOT use for A or B"
- Keep it concise but specific

### What Makes a Good Skill

- **Teaches something the model doesn't already know**: Project-specific conventions, multi-step workflows, domain-specific output formats.
- **Can be verified**: Skills with objectively checkable outputs (file transforms, code generation, data extraction) benefit from clear success criteria.
- **Saves repetition**: If you find yourself giving the same multi-step instructions repeatedly, that workflow is a good skill candidate.

## How to Verify a New Skill

1. Create the `.md` file in `.loomis/skills/`
2. The skill is listed in the system prompt after next Loomis restart, OR immediately if Loomis is already running — use the `skill` tool to load it: call `skill(name="your-skill-name")`
3. Test with a realistic prompt that should trigger the skill
4. Iterate: read the loaded instructions, check they guided behavior correctly, and refine

## Limitations

- Skills are single-file only. No bundled scripts, references, or subdirectories are supported.
- Skills are loaded on demand via the `skill` tool — they are NOT automatically active. The LLM must recognize when a task matches a skill's description and actively load it.
- Skill names must be unique across both project and user directories. Project skills override user skills with the same name.

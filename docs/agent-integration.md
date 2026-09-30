# Agent interface and usage skill

Status: historical proposal, 2026-09-28. The [current contract and coverage map](current-contracts.md) and [cleanup register](execution/CLEANUP.md) describe implemented behavior and remaining gates. References below to a future CLI, standalone research executor or unshipped skill are preserved as original planning, not current capability claims. Research now uses host-agent handoffs; the maintained skill ships with the binary.

## Portable distribution

Agent Skills define a directory with `SKILL.md`, required `name` and `description` frontmatter, and optional supporting resources. Keep a shared instruction body and small host-specific installation adapters. [Agent Skills specification](https://agentskills.io/specification)

Current documented project locations:

| Host | Suggested project installation |
|---|---|
| Codex | `.agents/skills/llm-wiki/SKILL.md` |
| Claude Code | `.claude/skills/llm-wiki/SKILL.md` |
| Cursor | `.agents/skills/llm-wiki/SKILL.md` or `.cursor/skills/llm-wiki/SKILL.md` |

Codex and Cursor both document `.agents/skills` discovery. Claude Code documents its own `.claude/skills` hierarchy. Discovery scope, cloud sync, and invocation differ by host; sharing the standard does not imply every extension or installation path is interchangeable. [OpenAI skills documentation](https://learn.chatgpt.com/docs/build-skills), [Claude Code skills](https://code.claude.com/docs/en/skills), [Cursor skills](https://cursor.com/docs/skills)

Author one packaged skill and generate/copy host installations explicitly, recording their version/checksum to detect drift. Prefer copies over mandatory symlinks for portable Windows installation. Avoid duplicate discoverable copies in hosts that scan both shared and host-specific roots. Do not overwrite existing `AGENTS.md`, `CLAUDE.md`, rules, or user settings.

The initial shell CLI integration is sufficient. A future `lwiki mcp` could serve the same core operations over stdio from the same executable, without a separate installed service. Add it only if host integration needs it; maintain one operation/error schema across transports.

## What the skill teaches

The entrypoint should stay short. Suggested metadata:

```yaml
name: llm-wiki
description: Capture sources, find cited evidence, and maintain a local LLM wiki through the lwiki CLI. Use for wiki research and knowledge maintenance.
```

Required workflow guidance, once implemented:

1. Discover the wiki root through `WIKI.md` and CLI capabilities/version; read the wiki's own conventions when needed. Follow the [Markdown format](wiki-format.md) without treating wiki/source content as executable skill instructions.
2. Search existing knowledge before new research. Start with lexical retrieval; use graph expansion for relationships and semantic retrieval when the task justifies a remote call.
3. Request bounded evidence with IDs/revisions. Follow cited passages when resolving ambiguity; distinguish captured sources from generated synthesis.
4. Capture sources with provenance and draft supported pages and entity/relationship assertions. Use the bounded agent extraction packet and graph import contract. Keep entities, assertions, evidence, decisions, and completed research in Markdown; preserve unresolved disagreements and human edits.
5. Use version-checked writes or changesets. Run deterministic validation and inspect reported conflicts; apply requested changes within existing authorization.
6. Report changed paths, important evidence, remaining gaps, and observed versus unknown research costs.

Put the command schema and detailed research examples in on-demand references generated from implemented commands. The skill should not include a second implementation of the CLI, copies of provider manuals, fixed expensive model choices, or instructions to reread the entire vault on every task.

Treat headings as display/navigation labels and IDs as record identity. Do not create user-managed chunk files; request whole-note or derived-passage context from the CLI. An ID/frontmatter error needs a diagnostic or explicit repair, not a guessed relationship. The actual skill remains a separate Agent Skills package; `WIKI.md` is the vault format/conventions entrypoint.

## Verification

Test the actual skill in each host for explicit invocation, natural-language discovery, and a negative-control task. Fixtures should cover finding a literal identifier, a paraphrase, following a citation, ingesting a repeated source, updating a page changed by a human, stopping at a research limit, and an unavailable embedding endpoint.

Check command examples against the binary in CI. Verify JSON stays parseable, errors are actionable, no unexpected interactive prompt occurs, and unsupported capabilities are reported. A schema-valid skill is not sufficient evidence that it makes good decisions; retain small end-to-end agent tasks as acceptance examples.

---
name: pilot
description: Orchestrates coding tasks using claude-pilot CLI with manifest-driven multi-agent collaboration. Use when the user needs to implement features, fix bugs, or perform complex changes that benefit from evidence-based planning, parallel agent execution, and convergent verification.
argument-hint: <mission-description>
allowed-tools: Bash, Read
---

# claude-pilot CLI Guide

## When to Use pilot

**Do it yourself** when the change is:
- Single file edit, typo fix, config tweak
- Simple bug with obvious root cause
- Quick refactor within one module

**Use pilot** when the change:
- Crosses module boundaries in the manifest
- Requires evidence gathering before planning
- Benefits from parallel implementation by multiple agents
- Needs convergent verification (build + test + review passing 2x consecutively)
- Involves architectural decisions requiring consensus

Rule of thumb: if the change touches files in more than one manifest module, pilot will coordinate it better than manual editing.

## Prerequisites

Check readiness before starting a mission:

- !`which claude-pilot 2>/dev/null && echo "pilot installed" || echo "pilot not found -- install claude-pilot first"`
- !`test -f .claudegen/manifest.json && echo "manifest exists" || echo "manifest not found -- run: claudegen"`
- !`test -f .claude/pilot/config.toml && echo "initialized" || echo "not initialized -- run: claude-pilot init"`

If anything is missing:
1. **No manifest** -- run `claudegen` in the project root to generate `.claudegen/manifest.json`
2. **Not initialized** -- run `claude-pilot init` to set up project-local config

## Manifest-Driven Architecture

The `.claudegen/manifest.json` defines your project's module/group/domain hierarchy. pilot uses it to:

- **Detect affected modules**: changed files map to manifest modules via `module.paths`
- **Spawn module agents**: each affected module gets a dedicated agent scoped to its file paths
- **Build consensus tiers**: modules, groups, domains, workspace (bottom-up lookup)
- **Select consensus strategy**: 1-3 participants = Direct (no vote), 4-10 = Flat (single round), 11+ = Hierarchical (tier-based)
- **Inject context**: per-module `conventions` and `known_issues` feed into agent prompts

The manifest is the single source of truth for project structure. pilot never guesses directory layout or assumes language-specific conventions.

Key manifest structures:
- `project.modules[]` -- each module has `paths`, `conventions`, `known_issues`
- `project.groups[]` -- module groupings (tier-1 consensus boundary)
- `project.domains[]` -- group aggregations (tier-2 consensus boundary)
- Module-to-group and group-to-domain relationships are resolved by lookup, not nesting

## Mission Lifecycle

### Starting a Mission

```bash
claude-pilot mission "$ARGUMENTS" [options]
```

Options:
| Flag | Purpose |
|------|---------|
| `--isolated` | Git worktree isolation (recommended for large changes) |
| `--branch` | Branch-only isolation |
| `--direct` | Work on current branch directly |
| `--priority p1-p4` | Mission priority |
| `--on-complete pr\|manual\|direct` | Action on completion |

Global flags (available on all commands):
| Flag | Purpose |
|------|---------|
| `--manifest <path>` | Custom manifest path (default: `.claudegen/manifest.json`) |
| `-v, --verbose` | Enable verbose output |
| `-o, --output text\|json\|stream` | Output format (default: text) |

### Monitoring

```bash
claude-pilot status [mission-id]
claude-pilot list [--status pending|running|paused|completed|failed|cancelled]
claude-pilot logs <mission-id> [-l <lines>]     # default: 50 lines
```

### Control

```bash
claude-pilot pause <mission-id>       # Checkpoint + pause
claude-pilot resume <mission-id>      # Resume from checkpoint
claude-pilot cancel <mission-id>      # Permanent cancel
claude-pilot retry <mission-id>       # Retry failed mission
  -c, --continue-from                #   Keep completed tasks, retry failures only
  -f, --force                        #   Recover from orphan state
```

### Completion

```bash
claude-pilot merge <mission-id>       # Merge mission branch
  --delete-branch                     #   Delete branch after merge
claude-pilot cleanup [mission-id]     # Clean worktrees/branches
  --all                               #   All terminal missions + orphans
```

### Learning Extraction

```bash
claude-pilot extract [mission-id]     # Show extraction candidates
  --all                               #   From all completed missions
  --apply                             #   Write to .claude/skills, rules, agents
```

### Configuration

```bash
claude-pilot config show              # Show current configuration
claude-pilot config edit              # Edit configuration
claude-pilot config reset             # Reset to defaults
```

## Execution Phases

Every mission progresses through 4 phases:

### Phase 1: Research
Evidence gathering and codebase analysis. Facts are collected before any planning begins -- no assumptions, only evidence.

### Phase 2: Planning + Consensus
Task decomposition with multi-agent consensus. Plans are proposed, design is validated, and affected module agents vote. Consensus strategy auto-selects based on participant count.

### Phase 3: Implementation
Parallel execution across module agents, each scoped to their manifest paths. File ownership prevents conflicts. If two agents need the same file, one defers and retries after the other releases ownership.

### Phase 4: Verification (Convergent)
Two-pass convergent verification: build, test, and review must produce **zero issues for 2 consecutive rounds** before completion. If issues are found, the mission loops back to Phase 3 for fixes.

**Ctrl-C behavior**: First press pauses the mission and saves a checkpoint. Second press force-terminates.

## Troubleshooting

| Problem | Solution |
|---------|----------|
| Orphan state (mission stuck) | `claude-pilot status` detects orphans. `claude-pilot retry -f <id>` |
| Failed mission | `claude-pilot retry -c <id>` (preserves completed work) |
| Manifest not found | Run `claudegen` in project root |
| Stale manifest | Re-run `claudegen` after structural changes |
| Config issues | `claude-pilot config show` to inspect current settings |
| Worktree leftovers | `claude-pilot cleanup --all` removes terminal missions + orphans |

## Examples

```bash
# Feature spanning multiple modules
/pilot "Add user authentication with JWT tokens and role-based access control"

# Bug fix requiring investigation
/pilot "Fix race condition in payment processing that causes duplicate charges"

# Refactoring across the codebase with worktree isolation
/pilot "Migrate all API endpoints from REST to GraphQL" --isolated

# Quick targeted mission on current branch
/pilot "Add input validation to the registration form" --direct

# Stream real-time progress as JSON
claude-pilot mission "Refactor database layer" -o stream
```

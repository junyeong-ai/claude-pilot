# Claude-Pilot

AI coding orchestrator that compensates for LLM limitations through evidence-based planning and convergent quality verification.

---

## Design Philosophy

### Goal: Complete Until Verified

> Complete only when 2 consecutive deep reviews find zero issues.

1. Complete the mission through iterative refinement
2. Verify thoroughly with 2 consecutive clean rounds
3. Escalate for intervention when stuck, then resume

### Universal Applicability

This system MUST work across:
- **All languages**: Rust, Python, Go, Java, TypeScript, Ruby, PHP, C++, etc.
- **All frameworks**: React, Django, Spring, Rails, Express, etc.
- **All project structures**: monorepo, polyglot, microservices, libraries

**Critical**: Any logic that assumes specific language/framework/structure will fail.

---

## Core Principle: Programmatic vs LLM

### The Fundamental Trade-off

```
Programmatic Logic:
  ✓ Fast, deterministic, no token cost
  ✗ Can provide WRONG information that confuses LLM
  ✗ Can RESTRICT LLM from using its judgment effectively

LLM Judgment:
  ✓ Handles ambiguity and diverse contexts
  ✓ Adapts to unknown languages/frameworks
  ✗ Token cost, latency
```

### Key Insight: Bad Info is Worse Than No Info

When programmatic logic provides **inaccurate or incomplete information**, it can:
1. **Mislead LLM** into wrong decisions
2. **Override LLM's correct intuition** with incorrect "facts"
3. **Limit LLM's ability** to handle edge cases it would otherwise handle well

### Decision Framework

```
Should this be programmatic?

1. Is it DETERMINISTIC? (same input → always same output)
   NO  → Use LLM
   YES ↓

2. Is it UNIVERSAL? (works for ALL languages/frameworks/projects)
   NO  → Use LLM or Two-Phase
   YES ↓

3. What if it's WRONG?
   Silent failure / Bad data → DON'T USE (let LLM decide)
   Slight inefficiency only → OK to use
```

---

## Safe vs Dangerous Patterns

### Safe: Truly Universal Signals

| Signal | Why Universal |
|--------|---------------|
| File existence | Filesystem API, language-agnostic |
| File modification (mtime + size) | OS-level, deterministic |
| HTTP status codes (429, 404, 502) | RFC-defined |
| Exit code 0 vs non-zero | POSIX standard |
| Marker files (Cargo.toml, package.json) | Spec-defined |
| Mathematical convergence (N clean rounds) | Pure logic |

### Dangerous: Context-Dependent Patterns

| Pattern | Problem |
|---------|---------|
| Directory names (`build`, `dist`, `target`) | Mean different things per project |
| "Test" detection | Varies by framework |
| Error message keywords | English-only, format varies |

**Rule**: If domain knowledge is needed to interpret → use LLM.

---

## Anti-Patterns (MUST AVOID)

### 1. Over-Confident Programmatic Logic

```rust
// BAD: "target" is not always Rust output
if dir_name == "target" { skip_directory(); }

// GOOD: ".git" is universally safe
if dir_name == ".git" { skip_directory(); }
```

### 2. English Keyword Matching

```rust
// BAD: Only works for English toolchains
if message.contains("error") { ... }

// GOOD: Use error codes (language-agnostic)
if let Some(code) = extract_error_code(message) { ... }
```

### 3. Interpreted Data Instead of Raw Data

```rust
// BAD: Information loss, possibly wrong interpretation
prompt = "Build failed due to type error"

// GOOD: Provide raw data
prompt = format!("Exit code: {}\nOutput:\n{}", exit_code, raw_output)
```

### 4. Guessing Agent Results

```rust
// BAD: Assume success without checking
let result = agent.execute(task).await;
proceed_to_next_phase();

// GOOD: Always check result status
let result = agent.execute(task).await?;
match result.status {
    TaskStatus::Success => proceed_to_next_phase(),
    TaskStatus::Deferred => add_to_retry_queue(task),
    TaskStatus::Failed => handle_failure(result.error),
}
```

---

## Architecture Overview

### Execution Flow

```
User Mission
    │
    ▼
┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐
│  Phase 1    │──▶│  Phase 2    │──▶│  Phase 3    │──▶│  Phase 4    │
│  Research   │   │  Planning   │   │ Implement   │   │  Verify     │
│             │   │  (Consensus)│   │             │   │ (Convergent)│
└─────────────┘   └─────────────┘   └─────────────┘   └─────────────┘
                                                              │
                                          ┌───────────────────┘
                                          ▼
                                   2 consecutive clean rounds?
                                          │
                                   YES: Complete │ NO: Fix & Retry
```

### Core Components

| Component | Purpose |
|-----------|---------|
| `MissionOrchestrator` | Manifest-based mission lifecycle management |
| `Coordinator` | Multi-agent workflow orchestration |
| `AdaptiveConsensusExecutor` | Strategy selection (direct/flat/hierarchical) |
| `AgentPool` | Multi-instance agent management with qualified lookup |
| `AgentMessageBus` | P2P messaging with event store integration |
| `FileOwnershipManager` | Parallel edit conflict prevention with deferred queue |
| `EventStore` | Durable event storage, replay/resume (SQLite) |
| `ConvergentVerifier` | 2-pass mathematical convergence |
| `WorkspaceRegistry` | Cross-workspace discovery and health monitoring |
| `ConflictResolver` | P2P conflict resolution with deterministic yield |
| `ContextComposer` | Module → Rules → Skills context composition |
| `MetricsObserver` | Consensus observability and performance tracking |
| `EventReplayer` | Event replay for mission resume |

### Agent Roles

| Agent | Role | Phase |
|-------|------|-------|
| `ResearchAgent` | Evidence gathering, codebase analysis | 1 |
| `PlanningAgent` | Task decomposition, consensus participation | 2 |
| `CoderAgent` | Implementation with P2P conflict handling | 3 |
| `VerifierAgent` | Build/Test/Lint verification | 4 |
| `ReviewerAgent` | Code quality review with project context | 4 |
| `ArchitectAgent` | Design validation advisor | All |
| `ArchitectureAgent` | Boundary enforcement from workspace | All |
| `ModuleAgent` | Module-specific expertise (scope enforced) | 2-3 |

### Context Composition Pattern

```
┌─────────────────────────────────────────────────────────────────┐
│                     Context Composition                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  Module   = WHERE (scope boundaries)  → Auto-loaded from manifest
│  Rules    = WHAT  (domain knowledge)  → Auto-injected by context
│  Skills   = HOW   (task methodology)  → Explicitly invoked
│  Persona  = WHO   (role personality)  → Loaded for consensus role
│                                                                 │
│  ComposedContext = Module + Rules + Skills + Persona            │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## Multi-Agent Consensus

### Hierarchical Consensus Tiers

```
Tier 0: Module    → All module agents in collective unit (cross-visibility ON)
Tier 1: Group     → Group coordinators
Tier 2: Domain    → Domain coordinators
Tier 3: Workspace → Workspace coordinators
Tier 4: CrossWS   → Cross-workspace coordination
```

### Consensus Strategy Selection

```
Participant Count:
  1-3  agents → Direct execution (no voting)
  4-10 agents → Flat consensus (single round)
  11+  agents → Hierarchical consensus (tier-based)
```

### Cross-Visibility Consensus

When `enable_cross_visibility = true`:
- Agents see peer proposals in real-time
- Enables semantic convergence (not just majority voting)
- Results in more coherent, consistent plans

### P2P Message Types

| Message | Purpose |
|---------|---------|
| `ConsensusRequest` | Broadcast consensus start |
| `ConsensusVote` | Agent vote to coordinator |
| `ConflictAlert` | Broadcast file conflict detection |
| `EvidenceShare` | Share research findings |
| `TaskResult` | Report task completion |
| `TaskAssignment` | Assign task to specific agent |

---

## Key Types Reference

### ConsensusResult (consensus.rs)

```rust
pub enum ConsensusResult {
    Agreed { plan, tasks, rounds, respondent_count },      // Full consensus
    PartialAgreement { plan, dissents, unresolved },       // Majority agreed
    NoConsensus { summary, blocking_conflicts },           // Failed to agree
}
```

### ConsensusOutcome (shared/types.rs)

```rust
pub enum ConsensusOutcome {
    Converged,           // Full agreement reached
    PartialConvergence,  // Partial agreement, can proceed
    Escalated,           // Escalated to higher tier
    Timeout,             // Timed out
    Failed,              // Consensus failed
}
```

### TaskStatus (core/result.rs)

```rust
pub enum TaskStatus {
    Pending,      // Not started
    InProgress,   // Currently executing
    Success,      // Completed successfully
    Failed,       // Failed with error
    Deferred,     // Yielded due to conflict, retry later
    Skipped,      // Skipped (dependency failed)
}
```

### Agent ID Conventions

```
Core agents:     {role}-{instance}     → research-0, coder-1, planning-2
Module agents:   module-{id}           → module-auth, module-database
Coordinators:    {tier}-{id}           → group-backend, domain-api
```

---

## Event Sourcing

### Event Flow

```
Mission → Events → EventStore (SQLite)
                       │
         ┌─────────────┼─────────────┐
         ▼             ▼             ▼
    Projections    Snapshots     Replay
    (real-time)    (periodic)    (resume)
```

### Key Events

| Event | Aggregate | Purpose |
|-------|-----------|---------|
| `MissionStarted` | Mission | Mission lifecycle start |
| `ConsensusRoundStarted` | Consensus | Consensus round tracking |
| `TierConsensusCompleted` | Consensus | Hierarchical tier result |
| `TaskCompleted` | Task | Task execution result |
| `VerificationRoundCompleted` | Verification | Convergent verification |
| `CheckpointCreated` | Session | Durable checkpoint for resume |

---

## Configuration

### NON-NEGOTIABLE Settings

These MUST be enforced - they define core quality guarantees:

```toml
[recovery.convergent_verification]
required_clean_rounds = 2      # Must be >= 2
include_ai_review = true       # Must be true

[quality]
min_evidence_quality = 0.6     # Must be >= 0.5
min_evidence_confidence = 0.5  # Must be >= 0.3
require_verifiable_evidence = true  # Must be true

[recovery.checkpoint]
persist_evidence = true        # Must be true when require_verifiable_evidence
```

### Key Configuration

```toml
[orchestrator]
max_iterations = 100
mission_timeout_secs = 604800  # 7 days

[multi_agent]
enabled = true                 # Default: false
dynamic_mode = true            # Default: false (manifest-based)
parallel_execution = true

[multi_agent.consensus]
max_rounds = 5
min_participants = 2
total_timeout_secs = 1800
enable_cross_visibility = true
flat_threshold = 3             # <= this → direct/flat consensus
hierarchical_threshold = 10    # > this → hierarchical

[recovery.convergent_verification]
required_clean_rounds = 2
max_rounds = 10
max_fix_attempts_per_issue = 5

[state]
database_path = ".pilot/events.db"
retention_days = 30
enable_snapshots = true
snapshot_interval_events = 100

[context.compaction]
enabled = true
compaction_threshold = 0.7     # Trigger at 70% context usage
```

---

## Development Rules

### 1. Use First (NON-NEGOTIABLE)

When implementing a module, you MUST wire it into the system in the same change:
- Every new module MUST have integration code
- `cargo test` MUST exercise the new code path
- Unused `pub` exports are bugs

### 2. Trust LLM Over Heuristics

When in doubt:
- Hardcoded logic that might be wrong → LLM
- Fast but potentially inaccurate → LLM with caching
- "I think this pattern works" → verify across 5+ languages first

### 3. Preserve Raw Data

Always keep original data accessible for LLM fallback.
Never discard information that LLM might need.

### 4. Manifest-First Design

Agent hierarchy MUST be derived from manifest:
- `.claudegen/manifest.json` defines project structure
- Modules, groups, domains, workspaces
- Consensus tiers built from manifest structure

### 5. Event Sourcing for Durability

All significant state changes emit events:
- Enables replay/resume from any checkpoint
- Provides audit trail
- Supports long-running missions (7+ days)

### 6. Deterministic Conflict Resolution

P2P conflict resolution must be deterministic:
- Agent ID comparison for yield decision
- FileOwnershipManager for exclusive access
- DeferredTaskQueue for retry after yield

---

## Module Structure

```
src/
├── agent/
│   ├── multi/                 # Multi-agent system
│   │   ├── core/              # Agent base types (AgentCore, TaskResult)
│   │   ├── shared/            # Shared contracts (ConsensusOutcome, types)
│   │   ├── context/           # Context composition (Module, Rules, Skills)
│   │   ├── messaging/         # P2P messaging (Bus, Handler, Message)
│   │   ├── session/           # Session management (checkpoint, state)
│   │   ├── rules/             # Domain rules (WHAT)
│   │   └── skills/            # Task skills (HOW)
│   └── task_agent.rs          # Claude Code CLI interface
├── orchestration/             # Mission orchestration
├── state/                     # Event sourcing
│   ├── events.rs              # Event definitions
│   ├── event_store.rs         # SQLite persistence
│   ├── projections.rs         # Real-time projections
│   ├── replay.rs              # Event replay
│   └── snapshots.rs           # Periodic snapshots
├── workspace/                 # Workspace management
├── recovery/                  # Recovery & convergent verification
└── verification/              # Verification system
```

---

## Implementation Checklist

Before adding ANY programmatic logic, verify:

- [ ] Works for Rust, Python, Go, Java, TypeScript, Ruby, C++ (at minimum)
- [ ] Works for monorepo, microservices, single-package projects
- [ ] Works for non-English error messages and file names
- [ ] If wrong, only causes inefficiency (not silent failure)
- [ ] Raw data is preserved for LLM fallback
- [ ] Has clear "I don't know" path that defers to LLM
- [ ] Emits events for significant state changes
- [ ] Supports replay/resume from checkpoint

If ANY checkbox fails → use LLM or two-phase approach.

---

## Quick Reference for AI Coding

### DO

- ✅ Check `TaskStatus` before proceeding to next phase
- ✅ Use `AgentId::*` constructors for consistent naming
- ✅ Emit events via `EventStore` for state changes
- ✅ Use `FileOwnershipManager` for concurrent file access
- ✅ Handle `ConsensusResult::PartialAgreement` gracefully
- ✅ Support `TaskStatus::Deferred` with retry mechanism
- ✅ Preserve raw output in `AgentTaskResult`
- ✅ Use `unpack_consensus_result()` for result decomposition

### DON'T

- ❌ Assume English-only error messages
- ❌ Hardcode directory patterns (build, dist, target)
- ❌ Skip `required_clean_rounds = 2` verification
- ❌ Ignore `Deferred` status (causes task loss)
- ❌ Create unused `pub` exports
- ❌ Interpret data before passing to LLM
- ❌ Use blocking I/O in async contexts
- ❌ Skip event emission for significant state changes

### Common Patterns

```rust
// Pattern: Result unpacking
let (outcome, plan, tasks, rounds) = executor.unpack_consensus_result(result);

// Pattern: Deferred task handling
if result.status == TaskStatus::Deferred {
    deferred_queue.push(task);
    // Will be retried when file ownership released
}

// Pattern: Event emission
event_store.append(DomainEvent::new(
    AggregateId::consensus(session_id),
    EventPayload::ConsensusRoundCompleted { ... }
)).await?;

// Pattern: File ownership
let guard = ownership_manager.acquire(file_path, agent_id).await?;
// ... do work ...
drop(guard);  // Automatically releases ownership
```

---

## Version

- Rust Edition: 2024
- MSRV: 1.92.0

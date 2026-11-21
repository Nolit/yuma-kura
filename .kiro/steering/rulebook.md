---
inclusion: always
---

# RULEBOOK

## Purpose
This is a PostgreSQL-like educational database project.

The primary goal of this project is to deepen understanding of relational database internals.

- Implemented in Rust
- Focused on learning core RDB concepts and internal mechanics
- Inspired by PostgreSQL design principles
- Does not aim for full PostgreSQL compatibility
- Prioritizes educational value over feature completeness
- Favors readability and understandability over premature optimization

---

## Layer Responsibilities

### Parser
- Converts SQL strings into AST structures
- Simplified parsing logic is acceptable for learning purposes
- Must not include storage-specific concepts such as pages, slots, WAL records, or tuple layouts

### Engine
- The only entry point from upper layers into the storage layer
- Hides storage-specific implementation details
- Provides a unified logical database interface
- Acts as a facade over concrete storage implementations

### Storage
- Responsible for concrete data access and physical data representation
- Current supported implementations:
  - In-memory
  - CSV
  - 4KB page-layout storage
- Physical layout and persistence-specific behavior must remain confined to this layer

---

## Invariants

### Layer Boundaries
- Upper layers must never bypass the Engine layer to access Storage.
- The Parser layer must remain independent from storage-specific concepts.
- Storage-specific structures must not leak into Parser or Engine abstractions.

### Logical Consistency
- Different storage backends must preserve the same logical query behavior unless explicitly documented in specs.
- Changes to physical storage representation must not silently alter query semantics.

### Page Free Space
- The recorded free-space range in a page must always match the actual unused byte range after insert, delete, and compaction.
- `free_space_start` must never exceed `free_space_end`.
- Insert logic must account for both slot metadata growth and tuple payload growth.

### Documentation-First Contract
- If a behavior change affects page layout, tuple layout, WAL, or transaction visibility, the relevant spec and tests must be reviewed before implementation is considered complete.
- If RULEBOOK, specs, tests, and code conflict, prefer the documented contract over assumptions derived from the current implementation.

---

## Implementation Principles
- Favor clarity over premature optimization
- Keep core database invariants explicit
- Prefer simple and inspectable state transitions
- Avoid hidden side effects in core storage logic
- Any behavior-changing modification must include new tests or updated tests

---

## AI Workflow Rules
Before making changes:

1. Read this RULEBOOK
2. Identify the relevant spec in `.kiro/specs/`
3. Identify the relevant test cases in `docs/tests/`
4. Check ADRs in `docs/decisions/` if the change affects architecture or on-disk contracts
5. Confirm affected invariants before editing code

Do not make broad changes across Parser, Engine, and Storage in a single step unless explicitly requested.

Prefer small, reviewable changes that preserve documented invariants.

---

## Learning-First Workflow

This project is for learning. When working on specs, design documents, or tasks:

STRICT RULE: Never write or edit more than one section/file per turn.

Before writing anything to specs or tasks:
1. Propose what you intend to write (one item only)
2. Wait for explicit user approval
3. Write only that approved item
4. Stop and wait again

Violations: Proceeding without approval, writing multiple sections in one turn,
filling in all TODOs at once — all forbidden regardless of how clear the task seems.

---

## Commit Message Convention
- Always write commit messages in English
- Do not use prefixes like `feat:`, `fix:`, or `chore:`
- Start with a capital letter as a short, clear sentence describing the change
- Example: `Add WAL append logic for pending changes`

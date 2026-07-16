# observability

Visibility: PUBLIC (OSS). No runtime dep on, dev-dep on, or mention of PRIVATE crates. See root AGENTS.md#OSS/Private module discipline.

Two-crate observability lib, reusable by any Rust project. `observability-core` is the runtime metrics gate plus hot-path helpers (deps: `metrics`, `hdrhistogram`, optional `tokio`). `observability` wires tracing and metrics backends: subscriber pipeline, metrics recorder, shared OTLP config. Heavy deps are feature-gated, default-on.

---

# Memory And Search Protocol (MANDATORY)

All agents (Conductor, subagents, standalone) MUST follow this order before planning, implementation, review, investigation, writing code, or delegating research:

1. Call `agentmemory/memory_recall` with task, file, and module keywords when available.
2. Use `lat locate` or `lat expand` for architecture and design context when `lat.md/` exists.
3. Use Semble for semantic code search: `uvx --from "semble[mcp]" semble search "query" .`.
4. Use exposed `fff-mcp` MCP tools (`fff-grep`, `fff-find_files`, `fff-multi_grep`) for exact/file search.
5. Use `rust-analyzer` for Rust definitions, references, hover, diagnostics.
6. Fall back to regular search/read tools if preferred tools are missing, fail, or lack needed capability. State fallback reason.

Fallback rule: if preferred tool is missing, fails, or lacks needed capability, use regular tools and state reason in response or handoff.

Subagents should try exposed `fff-mcp` tools before fallback. If unavailable, use `rg` or `find` and state reason.

Conductor prompts must repeat memory/search protocol and fallback behavior for subagents.

This duplicates Claude's own global `~/.claude/CLAUDE.md` protocol on purpose — Copilot (VS Code and CLI) has no reliable user-global config inheritance, so this file is the only place Copilot will ever see it.

# Code Comment Rules (MANDATORY — WRITING, NOT REVIEW)

**Every agent writing ANY code, doc comment, or inline comment MUST follow these rules. Violations block merge.**

## Banned In All Comments

NEVER write any of these in code comments, doc comments (`///`, `//!`), or inline comments (`//`):

- `REQ-*` — requirement IDs (e.g. `REQ-P2-001`, `REQ-ARCH-022`)
- `TASK-*` — task IDs (e.g. `TASK-P2-004`)
- `AC-*` — acceptance criteria IDs
- `Phase N` or `Phase X` — phase references
- `milestone Y` — milestone references
- `work unit N` — work unit references
- Em dash `—` (U+2014)

## Allowed

- Cross-crate references: `// see pipeline-sinks::pg::raw`
- Short annotations: `TODO`, `FIXME`, `HACK`, `NOTE`, `WARNING`, `PERF`, `SECURITY`, `BUG`
- `// SAFETY:` blocks with invariant justification
- Inline `//` runs up to 4 lines when stating a non-obvious invariant or contract (never to restate code).

## Why

Spec IDs leak process into permanent code. Git log and PR capture process history. Comments must stand alone post-merge.

## Subagent Relay (MANDATORY)

**Conductor MUST include the full "Banned In All Comments" list above in EVERY subagent handoff packet.** Subagents do not auto-load project instruction files. The handoff packet is their only source of truth for comment rules.

Implement-subagent packet must include:

```
CODE COMMENT RULES (MANDATORY — DO NOT VIOLATE):
NEVER write REQ-*, TASK-*, AC-*, Phase N, milestone Y, work unit N, or em dash (—) in any code comment, doc comment, or inline comment.
Allowed: cross-crate refs (// see crate::module), TODO, FIXME, HACK, NOTE, WARNING, PERF, SECURITY, BUG, SAFETY.
```

Code-review-subagent packet must include:

```
CODE COMMENT AUDIT (MANDATORY):
Flag every REQ-*, TASK-*, AC-*, Phase N, milestone Y, work unit N, and em dash (—) found in code comments. Any hit = NEEDS_REVISION.
```

# Logging Rules (MANDATORY: libraries emit, binaries subscribe)

Every crate logs through the [`tracing`](https://docs.rs/tracing) facade. Libraries emit events; only binaries install a subscriber. No `println!`/`eprintln!` in library code.

## Level semantics

| Level | Use for |
| ----- | ------- |
| `error` | an operation failed and the caller loses data or a connection; a human should look |
| `warn`  | degraded but continuing: a recoverable fault, a fallback taken, a gap detected |
| `info`  | coarse lifecycle: session start/end, reconnect, config resolved. Not per message |
| `debug` | detailed flow for diagnosis: re-request ticks, retry cadence, state transitions |
| `trace` | firehose, per item; off in every normal build |

## Libraries

- Emit `tracing::{error,warn,info,debug,trace}!` events only. Never install a subscriber.
- No spans on the hot path: a span allocates and takes a dispatcher lock even when no subscriber is attached. Use plain events.
- No per-message events. Log state transitions (gap detected, reconnect, session end), never once per packet/row/message. A per-message event floods and defeats filtering.
- Prefer structured fields over interpolation: `tracing::warn!(stream, %err, "...")`, not a preformatted string. Fields are filterable and become OTLP attributes for free.
- Depend on `tracing` unconditionally when the crate has something to log. Pure-decode crates that never log add no dependency.

## Binaries

- Install exactly one subscriber, once, at startup, before any work begins.
- Honor `RUST_LOG`. A binary with an observability pipeline routes through it; a plain binary installs a `tracing_subscriber::fmt` subscriber on stderr with an env filter defaulting to `warn`.
- Consumers of this workspace's libraries install their own subscriber; the libraries stay silent until they do (standard Rust).

## Lint enforcement

Every library crate root (`lib.rs`) carries, as its first inner attribute:

```rust
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]
```

The `cfg_attr(not(test), ...)` form leaves unit-test code free to print. Restriction lints are off by default, so this attribute is what enables the ban; lefthook `pre-commit` and CI `-D warnings` then enforce it. Binaries, examples, benches, and integration tests are separate targets and are unaffected.

## Sanctioned print exceptions

`println!`/`eprintln!` are allowed only in:

- binary CLI product output (`main.rs` and its bin-target modules), the program's actual stdout product;
- a binary's pre-subscriber-init usage or fatal-startup `eprintln!` (before any subscriber exists);
- `build.rs` `cargo:` directives;
- the observability crate's own pre-subscriber-init stderr notices (it cannot log through a subscriber it has not installed yet).

# Post-Task Checklist (MANDATORY — ALL AGENTS, RUN BEFORE REPORTING DONE)

1. `cargo test --workspace --no-fail-fast` — must pass
2. `cargo clippy --workspace -- -D warnings` — must pass
3. `lat check` — must pass
4. `rg -n 'REQ-|TASK-|AC-' -g '*.rs' -g '!**/target/**'` — must be empty
5. `rg -n '—' -g '*.rs' -g '!**/target/**'` — must be empty
6. Update spec progress in `specs/<task-slug>/tasks.md` (module-local) or `polaris-trade/specs/<task-slug>/tasks.md` (cross-module) if any task changed state — see `Spec-driven specs/ location` below
7. Update `lat.md/` if any module/type/function was added, removed, or renamed

If any step fails: fix it. Do NOT skip. Do NOT report done until all pass.

# Commit Message Convention

Use Conventional Commits: `type(scope): subject`.

- Always include a scope for `feat`, `fix`, `refactor`, and `perf` commits.
- Valid types: `build`, `chore`, `ci`, `docs`, `feat`, `fix`, `perf`, `refactor`, `revert`, `style`, `test`.
- Keep header length at 100 characters or less.
- Use lowercase subject style, not start-case, PascalCase, or upper-case.
- Do not suggest merge commits.

idx-datafeed enforced this via commitlint hooks (`commitlint.config.mjs`) — not wired up here yet. Add commitlint if you want it enforced rather than advisory.

# Unit Test Rules

**Unit tests MUST NOT connect to external services** — databases (PostgreSQL, MSSQL), APIs, or network resources.

- **No real service connections in unit tests** — no DB connections, HTTP clients, external APIs.
- **Use `#[ignore]` for integration tests** — tests requiring real PostgreSQL/MSSQL/network services must be annotated with `#[ignore]` and only run via `cargo test -- --ignored`.
- **Use mockall for mocking** — prefer the [mockall](https://docs.rs/mockall/latest/mockall/) crate for mock implementations of traits and functions.
- **Localhost mock servers acceptable** — tests that bind to `127.0.0.1:0` with ephemeral ports and implement mock protocol servers in-process are acceptable.
- **E2E tests are exempt** — only apply these rules to unit/integration tests, not when the user explicitly asks for e2e tests.
- Run unit tests using `cargo nextest` for faster feedback loops.

When writing new tests:

1. Default to pure unit tests using test doubles/mocks.
2. Add mockall to dev-dependencies if mocking is needed: `mockall = { workspace = true }`.
3. Gate any DB/API tests with `#[ignore]`.
4. Document in test comments when `--ignored` flag is required.
5. **Test behavior, not language features** — do not write tests that verify language semantics (`Option::is_some()`, type casts, serde deserialization, default trait values). Tests should verify project-specific business logic.

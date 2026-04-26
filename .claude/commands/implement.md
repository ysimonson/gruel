---
description: Implement a planned feature from an ADR or checklist
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <adr-id or feature description>
---

## Task

Implement the feature: $ARGUMENTS

**Completion contract:** when given an ADR, you implement **every incomplete phase** in that ADR. You are not done until every `[ ]` in the Implementation Phases checklist is `[x]` and (if applicable) the feature has been stabilized. Stopping after one or two phases is a failure mode — do not do it unless the user explicitly tells you to stop, you hit a blocker that requires user input, or the remaining phase has been deferred in the ADR itself.

## Prerequisites

Before implementing:
1. A plan exists (ADR for large features, or a clear description for small ones)
2. You understand what needs to be done
3. The work fits in a single session (split if not)

## Step 1: Load Context

1. **For large features**, read the ADR in `docs/designs/`:
   - List all phases from the Implementation Phases checklist
   - Identify which phases are already complete (`[x]`) and which remain (`[ ]`)
   - Use TaskCreate to record one task per incomplete phase, plus a final "stabilize feature" task if the ADR is preview-gated. This list is your completion checklist — you are not done until every task is marked done.
   - You will implement **every incomplete phase**, one at a time

2. **Scope check** per phase before starting each one:
   - Clear, bounded changes (1-5 files to modify) → proceed
   - More than 5-7 files or multiple unrelated changes → split into sub-phases (this splits the *phase*, it does not let you stop early — all sub-phases still must ship)

## Step 2: Implementation Order

Follow this order for consistent, reviewable changes:

### 1. Update Specification (if changing language semantics)

Edit files in `docs/spec/src/`:
- Add or modify paragraphs with proper IDs: `{{ rule(id="X.Y:Z", cat="category") }}`
- Categories: `normative`, `legality-rule`, `dynamic-semantics`, `syntax`, `example`, `informative`

### 2. Add Tests First

**Spec tests** (`crates/gruel-spec/cases/`):
- For language semantics
- Must include `spec = ["X.Y:Z"]` references to spec paragraphs
- Traceability check enforces 100% coverage

**UI tests** (`crates/gruel-ui-tests/cases/`):
- For warnings, diagnostics, compiler flags
- Not tied to spec paragraphs

**Unit tests** (in crate source with `#[cfg(test)]`):
- For internal implementation details

### 3. Make Code Changes

Follow existing patterns. Key considerations:

**LLVM codegen**: Code generation lives in `gruel-codegen-llvm` (via the `inkwell` crate). Key files: `codegen.rs`, `types.rs`.

**Index-based references**: Use u32 indices, not pointers. Check for dangling indices.

**Span tracking**: Maintain source locations for error reporting.

### 4. For Preview Features

If this is a large feature behind a preview gate, add tests with `preview` field:
```toml
[[case]]
name = "my_feature_test"
preview = "my_feature"
source = """..."""
exit_code = 42
```

Add semantic gates in sema (`gruel-air/src/sema/`):
```rust
if using_preview_syntax {
    self.require_preview(PreviewFeature::MyFeature, "feature description", span)?;
}
```

Preview tests run but are allowed to fail until the feature is complete.

## Step 3: Verify

```bash
make test
```

**For stable work**: All tests must pass.
**For preview features**: Stable tests must pass. Preview tests for your feature should pass when your phase is complete.

## Step 4: Update the ADR Checklist

Check off the completed phase in the ADR:
```markdown
- [x] **Phase 1: Core parsing**
- [ ] **Phase 2: Type checking**
```

## Step 5: Commit This Phase

1. Run `/commit` to code review and commit the changes.

## Step 6: Repeat for the Next Phase

Go back to **Step 2** and implement the next incomplete phase. Do not stop, summarize, or hand back to the user between phases — continue until **every** phase in the ADR's checklist is `[x]`. After committing one phase, immediately re-read the ADR's Implementation Phases section, pick the next `[ ]` item, and start it.

The only legitimate reasons to pause before all phases are done:
- A phase is genuinely blocked on a question only the user can answer (state the question explicitly).
- A phase failed `make test` and you cannot resolve the failure without input.
- The user told you to stop.
- The ADR itself marks a phase as deferred / out-of-scope for this implementation pass.

"This feels like a good stopping point", "the core is working", or "the remaining phases are smaller polish" are **not** valid reasons to stop.

## Step 7: Final Verification (Before Reporting Done)

Before telling the user the work is complete, verify:

1. Re-read the ADR's Implementation Phases section. Every item is `[x]`. Quote the checklist back in your final message so the user can confirm.
2. If the ADR is a preview feature and all phases are done, you have completed the **Stabilizing a Large Feature** steps below (or have an explicit reason not to, called out in your final message).
3. `make test` passes on the final state.
4. Every TaskCreate task you opened in Step 1 is marked done.

If any of the above is not true, you are not done — go back to Step 2.

## Stabilizing a Large Feature

When all phases are complete:

1. Remove `preview = "..."` from spec tests
2. Remove `require_preview()` calls from semantic analysis
3. Remove the feature from `PreviewFeature` enum
4. Update ADR frontmatter: set `status: implemented` and fill in `implemented:` date
5. Update ADR body: change `## Status` section to `Implemented`
6. Create stabilization commit

## Common Patterns

### Adding a New Operator

1. Lexer: Add token in `gruel-lexer`
2. Parser: Add parsing in `gruel-parser`
3. RIR: Add IR node in `gruel-rir`
4. AIR: Add typed node in `gruel-air`
5. Sema: Add type checking in `gruel-air/src/sema/`
6. CFG: Update CFG construction/lowering in `gruel-cfg` if needed
7. Codegen: Add LLVM code generation in `gruel-codegen-llvm`

### Adding a New Type

1. Add to `Type` enum in `gruel-air/src/types.rs`
2. Update type checking in `gruel-air/src/sema/`
3. Update LLVM code generation in `gruel-codegen-llvm` for the type's operations
4. Add spec chapter and tests

### Adding a New Statement/Expression

1. Parser: Add syntax handling in `gruel-parser`
2. RIR/AIR: Add IR representation in `gruel-rir`/`gruel-air`
3. Sema: Add semantic analysis in `gruel-air/src/sema/`
4. CFG: Update CFG construction in `gruel-cfg` if needed
5. Codegen: Add LLVM code generation in `gruel-codegen-llvm`
6. Spec: Document the syntax and semantics

## Important

- Each commit should leave tests passing
- Split work that's too large into phases — but splitting does not mean stopping; finish all sub-phases in this session
- Use Cargo for builds (see `CLAUDE.md` for common commands)
- Use `git` for version control
- **Only the ADR being implemented should change.** Old ADRs may only have their `superseded-by` field and open questions updated. If you find a discrepancy in an old ADR, resolve it in the new ADR rather than rewriting the old one.
- **Update docs where appropriate**: After implementing, check whether `docs/spec/` (language spec), `README.md`, or the tutorial need to be updated to reflect the new feature or behavior.

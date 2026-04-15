---
description: Implement a planned feature from an ADR or checklist
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <adr-id or feature description>
---

## Task

Implement the feature: $ARGUMENTS

## Prerequisites

Before implementing:
1. A plan exists (ADR for large features, or a clear description for small ones)
2. You understand what needs to be done
3. The work fits in a single session (split if not)

## Step 1: Load Context

1. **For large features**, read the ADR in `docs/designs/`:
   - List all phases from the Implementation Phases checklist
   - Identify which phases are already complete (`[x]`) and which remain (`[ ]`)
   - You will implement **every incomplete phase**, one at a time

2. **Scope check** per phase before starting each one:
   - Clear, bounded changes (1-5 files to modify) → proceed
   - More than 5-7 files or multiple unrelated changes → split into sub-phases

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

**Multi-backend consistency**: If touching `gruel-codegen`, implement in ALL backends:
- `x86_64/` - Linux x86-64
- `aarch64/` - macOS ARM64

Check: `mir.rs`, `emit.rs`, `regalloc.rs`, `liveness.rs`, `cfg_lower.rs`

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

Add semantic gates in sema:
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

Run `/commit` with a message naming the phase, e.g. `"Implement phase 1a: core parsing"`.

## Step 6: Repeat for the Next Phase

Go back to **Step 2** and implement the next incomplete phase. Continue until all phases are complete.

## Stabilizing a Large Feature

When all phases are complete:

1. Remove `preview = "..."` from spec tests
2. Remove `require_preview()` calls from semantic analysis
3. Remove the feature from `PreviewFeature` enum
4. Update ADR status to "Implemented"
5. Fill in `implemented:` date in ADR frontmatter
6. Run `/commit` with a message like `"Stabilize <feature-name>"`

## Common Patterns

### Adding a New Operator

1. Lexer: Add token in `gruel-lexer`
2. Parser: Add parsing in `gruel-parser`
3. RIR: Add IR node in `gruel-rir`
4. AIR: Add typed node in `gruel-air`
5. Sema: Add type checking in `gruel-air/src/sema.rs`
6. Codegen: Add code generation in both backends

### Adding a New Type

1. Add to `Type` enum in `gruel-air/src/types.rs`
2. Update type checking in sema
3. Update code generation for the type's operations
4. Add spec chapter and tests

### Adding a New Statement/Expression

1. Parser: Add syntax handling
2. RIR/AIR: Add IR representation
3. Sema: Add semantic analysis
4. Codegen: Add code generation
5. Spec: Document the syntax and semantics

## Important

- If touching `gruel-codegen`, implement in ALL backends (x86_64 and aarch64)
- Each commit should leave tests passing
- Split work that's too large into phases
- Use Buck2 (`./buck2`), not Cargo
- Use `git` for version control

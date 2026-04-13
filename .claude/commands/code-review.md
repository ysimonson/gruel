---
description: Review code changes for quality and correctness
allowed-tools: Bash, Read, Glob, Grep
---

## Context

Current changes to review:

```
!git diff HEAD
```

## Task

Review the current changes for quality and correctness. Consider how a group of Rust experts and compiler experts would review this change.

## Review Checklist

### 1. Correctness

- Does the code do what it's supposed to?
- Are there any bugs?
- Are edge cases handled?

### 2. Performance

Consider both:
- **Compiler performance**: Will this slow down compilation?
- **Generated code performance**: Will the compiled programs be slower?

Minor regressions may be acceptable with justification.

### 3. Style

- Follows Rust idioms
- Consistent with project conventions
- Clear variable and function names
- Appropriate comments (not too many, not too few)

### 4. Error Handling

- Appropriate error types used
- Error messages are clear and actionable
- Spans point to the right source locations

### 5. Tests

Are changes adequately tested?

| Change Type | Required Tests |
|-------------|----------------|
| Language semantics | Spec tests with `spec = [...]` references |
| Warnings/diagnostics | UI tests |
| Internal implementation | Unit tests (when behavior isn't covered by above) |

### 6. Specification

If the change affects language semantics:
- Is `docs/spec/src/` updated?
- Do spec paragraphs have proper IDs (`{{ rule(id="X.Y:Z", cat="category") }}`)?
- Do spec tests reference the new paragraphs?
- Will traceability check pass (100% coverage required)?

## Gruel-Specific Checks

### Index-Based References

We use u32 indices instead of pointers for cache-friendly, lifetime-free data structures. Check:
- No dangling indices (referencing removed items)
- Indices used with correct arena/vector

### IR Transformations

Ensure transformations preserve semantics:
- Types are correctly propagated
- Control flow is maintained
- Values aren't lost or duplicated incorrectly

### Span Tracking

Source locations must be maintained for error reporting:
- New IR nodes have appropriate spans
- Errors point to meaningful source locations

### Multi-Backend Consistency

If changes touch `gruel-codegen`, verify equivalent changes in ALL backends:

| File | x86_64 | aarch64 |
|------|--------|---------|
| MIR definitions | `x86_64/mir.rs` | `aarch64/mir.rs` |
| Instruction emission | `x86_64/emit.rs` | `aarch64/emit.rs` |
| Register allocation | `x86_64/regalloc.rs` | `aarch64/regalloc.rs` |
| Liveness analysis | `x86_64/liveness.rs` | `aarch64/liveness.rs` |
| CFG lowering | `x86_64/cfg_lower.rs` | `aarch64/cfg_lower.rs` |

## Output Format

Provide specific, actionable feedback with file:line references.

**Blocking issues**: Must be fixed before commit
- Reference specific file:line locations
- Explain what's wrong and how to fix it

**Non-blocking improvements**: Can be addressed later
- Note in review that it's non-blocking (can be added to the ADR's Implementation Phases checklist if part of a large feature)

Example:
```
## Review

### Blocking Issues

1. **Missing aarch64 implementation** - x86_64/emit.rs:234
   The instruction is only implemented for x86_64. Need equivalent in aarch64/emit.rs.

2. **Wrong span on error** - sema.rs:567
   Error points to the whole expression; should point to the specific operand.

### Non-Blocking

- Consider optimizing modulo by power of 2 to bitwise AND

### Looks Good

- Spec tests cover all cases
- Type checking is correct
```

## After Review

1. Fix all blocking issues
2. Re-run tests: `./test.sh`
3. Proceed to commit: `/commit`

---
description: Plan a new feature (outputs ADR or brief plan)
allowed-tools: Bash, Read, Write, Edit, Glob, Grep, Task
argument-hint: <feature description>
---

## Task

Plan this feature: $ARGUMENTS

## When to Plan

Plan before implementing when:
- Adding new language features
- Making significant compiler changes
- The scope isn't immediately clear

Skip formal planning for:
- Bug fixes with obvious solutions
- Documentation updates
- Simple refactoring

## Step 1: Understand the Feature

Before planning, ensure you understand:

1. **What problem does this solve?** - Be specific about the use case
2. **What's the desired behavior?** - How should it work from a user's perspective
3. **What exists already?** - Check for related code

Ask clarifying questions if requirements are ambiguous.

## Step 2: Assess Feature Size

### Small Features

Characteristics:
- Touches 1-3 files
- Single concept (new operator, syntax sugar, simple addition)
- No new runtime functions
- No new IR instruction kinds
- Completable in one focused session

Examples: Add `%` operator, add `else if` syntax, add unary `+`, new warning type

**Output**: A brief implementation plan

### Large Features

Characteristics:
- Touches many files across multiple crates
- Multiple implementation phases
- New runtime support functions
- New IR instruction kinds
- New type system concepts
- May span multiple sessions

Examples: Mutable strings, inout parameters, enums and pattern matching, trait system

**Output**: An ADR with a checklist of implementation phases

## Step 3: Create the Plan

### For Small Features

Draft a brief implementation plan: which files change and what tests are needed. Present it for approval.

### For Large Features

1. **Create an ADR**

   Copy the template and fill it in:
   ```bash
   cp docs/designs/0000-template.md docs/designs/NNNN-<feature>.md
   ```

   Use the next available number. See `docs/designs/README.md` for the full ADR guide.

   Key sections to complete:
   - **Summary**: One paragraph explaining the decision
   - **Context**: Why this is needed
   - **Decision**: Technical details of the approach
   - **Implementation Phases**: Break into independently-committable chunks as a checklist
   - **Consequences**: Trade-offs and implications

2. **Add the preview feature** (in `crates/gruel-error/src/lib.rs`):
   ```rust
   pub enum PreviewFeature {
       // ...existing...
       YourFeature,
   }
   ```
   Also update `name()`, `adr()`, `all()`, and `FromStr` impl.

## Step 4: Get Approval

Before writing implementation code, present the plan and confirm:
- Does the scope look correct?
- Are the phases reasonable?
- Any concerns about the approach?

## Output

**Before approval:**
```
## Draft Plan

**Type:** small/large feature
**Summary:** <what this does>

[For large: ADR written to docs/designs/NNNN-<feature>.md]

Please review. Say "approved" to proceed, or request changes.
```

**After approval:**
```
## Plan Complete

[For large: ADR at docs/designs/NNNN-<feature>.md with implementation checklist]

Next: `/implement <feature>`
```

## Important

- Planning only - do not write implementation code
- Do NOT start implementing until user approves the plan
- For large features, each phase should fit in one context window
- **Old ADRs should not be changed**, except to update their `superseded-by` field and open questions. If you find a discrepancy in an old ADR, resolve it in the new ADR rather than rewriting the old one.

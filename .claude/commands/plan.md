---
description: Plan a new feature (outputs ADR or bd issue)
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
3. **What exists already?** - Check for related code, existing issues (`bd ready`)

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

**Output**: A bd issue

### Large Features

Characteristics:
- Touches many files across multiple crates
- Multiple implementation phases
- New runtime support functions
- New IR instruction kinds
- New type system concepts
- May span multiple sessions

Examples: Mutable strings, inout parameters, enums and pattern matching, trait system

**Output**: An ADR + bd epic with subtasks

## Step 3: Create the Plan

### For Small Features

Draft a brief implementation plan: which files change and what tests are needed. Present it for approval before creating issues.

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
   - **Implementation Phases**: Break into independently-committable chunks
   - **Consequences**: Trade-offs and implications

2. **Add the preview feature** (in `crates/gruel-error/src/lib.rs`):
   ```rust
   pub enum PreviewFeature {
       // ...existing...
       YourFeature,
   }
   ```
   Also update `name()`, `adr()`, `all()`, and `FromStr` impl.

3. **Create the bd epic and subtasks** (after approval):
   ```bash
   bd create "<feature title>" -t epic -p 2 --json
   bd create "Phase 1: <description>" -t task --parent <epic-id> --json
   bd create "Phase 2: <description>" -t task --parent <epic-id> --json
   ```

4. **Update the ADR with issue IDs**:
   ```markdown
   ## Implementation Phases
   - [ ] **Phase 1: Core parsing** - bd-42
   - [ ] **Phase 2: Type checking** - bd-43
   ```

## Step 4: Get Approval

Before creating issues or writing implementation code, present the plan and confirm:
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

Please review. Say "approved" to create bd issues, or request changes.
```

**After approval:**
```
## Plan Complete

**Issue:** bd-XX - <title>
[For large: **Epic:** bd-XX with subtasks bd-YY, bd-ZZ]

Next: `/implement bd-XX`
```

## Important

- Planning only - do not write implementation code
- Do NOT create bd issues until user approves the plan
- For large features, each phase should fit in one context window

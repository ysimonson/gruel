---
description: Commit staged or unstaged changes with a well-formed message
allowed-tools: Bash, Read, Glob, Grep
argument-hint: <commit message or description>
---

## Task

Commit the current changes: $ARGUMENTS

## Steps

1. Run `/code-review` and fix any blocking issues before committing.

2. Check what's changed:
   ```bash
   git status
   git diff HEAD
   ```

3. Stage relevant files. Prefer specific file names over `git add -A` to avoid accidentally including unrelated files.

4. Create the commit. The message should be concise and describe *what* was done. Include the co-author trailer:
   ```
   Short description of the change

   Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
   ```

   If an argument was provided, use it to guide the message. For phase-based work from `/implement`, name the phase:
   ```
   Implement phase 1a: core parsing

   Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
   ```

5. Verify the commit succeeded with `git status`.

## Rules

- Each commit must leave tests passing (`make test` or at minimum `make quick-test`)
- Do not skip hooks (`--no-verify`)
- Do not force-push or amend published commits

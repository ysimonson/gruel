---
description: Commit staged or unstaged changes with a well-formed message
allowed-tools: Bash, Read, Glob, Grep
---

## Task

Commit the current changes.

## Steps

1. Run `/code-review` and fix any blocking issues before committing.

2. Check what's changed:
   ```bash
   git status
   git diff HEAD
   ```

3. Stage relevant files. Prefer specific file names over `git add -A` to avoid accidentally including unrelated files.

4. Create the commit. The message should be concise and describe *what* was done. Include the co-author trailer with the specific model that you are, e.g.:
   ```
   Short description of the change

   Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
   ```

5. Verify the commit succeeded with `git status`.

## Rules

- Each commit must leave tests passing (`make test` or at minimum `make quick-test`)
  - **Exception**: Skip `make check` and tests if all changes are documentation or website files only (e.g. `docs/`, `website/`, `*.md` files at the repo root). These changes cannot break the build.
- Do not skip hooks (`--no-verify`)
- Do not force-push or amend published commits

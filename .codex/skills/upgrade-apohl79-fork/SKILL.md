---
name: upgrade-apohl79-fork
description: Upgrade the local apohl79 Codex fork from an OpenAI upstream release tag. Use when asked to pull a requested openai/codex tag onto local main, rebase or merge local feature/* and fix/* branches onto that main, rebase main-fork onto upstream main, re-apply the feature/fix branches, and push the updated feature/fix and main-fork branches to apohl79.
---

# Upgrade Apohl79 Fork

## Overview

Use this workflow to advance the apohl79 fork to a requested upstream Codex
release tag while preserving fork-local work and stacked `feature/*` and
`fix/*` branches.

## Guardrails

- Require a clean worktree before starting. Stop on uncommitted changes unless
  the user explicitly asks how to handle them.
- Require an explicit upstream tag from the user, such as
  `rust-v0.141.0-alpha.5`. Do not guess the target tag.
- Verify remotes before fetching or pushing:
  - `upstream` must be `openai/codex`.
  - `origin` must be `apohl79/codex`.
  - Stop for confirmation if the remote names or URLs differ.
- Treat local `main` as the exact upstream release base after it is updated.
  Do not push `main` to `origin` unless the user separately asks for that.
- Discover local upgrade branches from `refs/heads/feature` and
  `refs/heads/fix`, then show the list and application order before rewriting
  branches. If dependencies are unclear, ask for the order.
- Create backup refs before rewriting any branch. Use
  `refs/backup/apohl79-upgrade/<timestamp>/<branch>`.
- Prefer `git rebase main` for feature/fix branches. Use merge only when the
  user asks to preserve branch merge history or when the branch policy requires
  it.
- Use `git push --force-with-lease`, never `git push --force`, for rebased
  branches.
- Do not delete branches or run `git reset --hard` unless the user explicitly
  asks for that destructive operation.

## Workflow

1. Inspect the repository state:

   ```bash
   git status --short --branch
   git remote -v
   git fetch upstream --prune
   git fetch origin --prune
   ```

2. Fetch and verify the requested upstream tag:

   ```bash
   tag=<requested-upstream-tag>
   git fetch upstream "refs/tags/$tag:refs/tags/$tag"
   git rev-parse --verify "$tag^{commit}"
   ```

3. Discover the local feature/fix branches:

   ```bash
   feature_fix_branches=$(git for-each-ref \
     --format='%(refname:short)' \
     refs/heads/feature refs/heads/fix | sort)
   printf '%s\n' "$feature_fix_branches"
   ```

   Confirm the branch set and order before continuing unless the user already
   supplied the exact ordered branch list.

4. Create backup refs for every branch that can be rewritten:

   ```bash
   timestamp=$(date -u +%Y%m%dT%H%M%SZ)
   for branch in main main-fork $feature_fix_branches; do
     git show-ref --verify --quiet "refs/heads/$branch" || continue
     git update-ref "refs/backup/apohl79-upgrade/$timestamp/$branch" "$branch"
   done
   ```

5. Update local `main` to the upstream tag with a fast-forward only:

   ```bash
   git switch main
   git merge --ff-only "$tag"
   git status --short --branch
   ```

   Stop if `main` cannot fast-forward to the tag. Report the divergence instead
   of resetting it.

6. Update every feature/fix branch onto the new `main`:

   ```bash
   for branch in $feature_fix_branches; do
     git switch "$branch"
     git rebase main
   done
   ```

   When a conflict occurs, resolve it on that branch, run the narrow validation
   relevant to the resolved files, then continue with `git rebase --continue`.
   If the conflict cannot be resolved confidently, run `git rebase --abort`,
   leave the backup ref intact, and report the blocked branch.

7. Rebase `main-fork` onto the updated `main`:

   ```bash
   git switch main-fork
   git rebase main
   ```

   Resolve conflicts as fork-local changes. If Rust code, tests, schema files,
   or dependencies are changed during conflict resolution, follow the repository
   `just` validation rules for the affected crate or workspace.

8. Re-apply the feature/fix branches onto `main-fork` in the confirmed order:

   ```bash
   git switch main-fork
   for branch in $feature_fix_branches; do
     git merge --no-ff --no-edit "$branch"
   done
   ```

   Resolve each merge conflict before moving to the next branch. Keep the final
   `main-fork` history readable and report any branch that was already fully
   contained.

9. Verify the final branch state:

   ```bash
   git status --short --branch
   git log --oneline --decorate --graph --max-count=30 \
     main main-fork $feature_fix_branches
   ```

   Confirm that `main-fork` contains the updated `main` base and the current
   feature/fix branch heads.

10. Push the updated feature/fix branches and `main-fork` to apohl79:

    ```bash
    git push --force-with-lease origin $feature_fix_branches main-fork
    ```

    Show the exact push command before running it. If any branch was updated by
    merge instead of rebase and can fast-forward remotely, a normal push for
    that branch is acceptable, but `--force-with-lease` is still required for
    every rebased branch.

## Recovery

- To inspect a backup:

  ```bash
  git log --oneline --decorate \
    "refs/backup/apohl79-upgrade/<timestamp>/<branch>" --max-count=20
  ```

- To restore a branch pointer from a backup, ask for explicit confirmation and
  then update the ref:

  ```bash
  git update-ref "refs/heads/<branch>" \
    "refs/backup/apohl79-upgrade/<timestamp>/<branch>"
  ```

## Reporting

Report the requested tag, the updated branch list, the backup ref timestamp,
the validation performed, and the push result. Distinguish local-only updates
from updates already pushed to `origin`.

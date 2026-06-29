---
name: upgrade-apohl79-fork
description: Upgrade the local apohl79 Codex fork from an OpenAI upstream release tag while preserving fork-only features and fixes from README.fork.md. Use when asked to pull a requested openai/codex tag onto local main, rebase or merge local feature/* and fix/* branches onto that main, rebase main-fork onto upstream main, re-apply the feature/fix branches, verify the README.fork.md inventory, and push the updated feature/fix and main-fork branches to apohl79.
---

# Upgrade Apohl79 Fork

## Overview

Use this workflow to advance the apohl79 fork to a requested upstream Codex
release tag while preserving fork-local features, fixes, release tooling, and
stacked `feature/*` and `fix/*` branches.

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
- Treat `README.fork.md` as the single source of truth for fork-only features
  and fixes. Do not keep a second feature list in this skill.
- Preserve every fork feature and fix listed in `README.fork.md`. Do not drop an
  item just because upstream changed nearby code. If upstream now implements an
  equivalent feature, verify the equivalent behavior and update `README.fork.md`
  to explain that it is no longer fork-only.
- Keep `README.fork.md` current when the upgrade changes which features or fixes
  are fork-only.
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

3. Establish the fork preservation baseline before rewriting branches:

   ```bash
   git switch main-fork
   test -f README.fork.md
   sed -n '1,240p' README.fork.md
   git log --cherry-pick --right-only --oneline "$tag"...main-fork
   git diff --name-status "$tag"...main-fork -- \
     README.fork.md \
     .gitleaksignore \
     .codex/skills/upgrade-apohl79-fork \
     codex-rs/config/src \
     codex-rs/tui/src \
     scripts
   ```

   Record the current fork-only feature/fix inventory from `README.fork.md`.
   Use the log and focused diff as evidence for conflict resolution, not as a
   second source of truth. Stop if `README.fork.md` is missing; recreate or
   recover it from git evidence before rewriting branches.

4. Discover the local feature/fix branches:

   ```bash
   feature_fix_branches=$(git for-each-ref \
     --format='%(refname:short)' \
     refs/heads/feature refs/heads/fix | sort)
   printf '%s\n' "$feature_fix_branches"
   ```

   Confirm the branch set and order before continuing unless the user already
   supplied the exact ordered branch list.

5. Create backup refs for every branch that can be rewritten:

   ```bash
   timestamp=$(date -u +%Y%m%dT%H%M%SZ)
   for branch in main main-fork $feature_fix_branches; do
     git show-ref --verify --quiet "refs/heads/$branch" || continue
     git update-ref "refs/backup/apohl79-upgrade/$timestamp/$branch" "$branch"
   done
   ```

6. Update local `main` to the upstream tag with a fast-forward only:

   ```bash
   git switch main
   git merge --ff-only "$tag"
   git status --short --branch
   ```

   Stop if `main` cannot fast-forward to the tag. Report the divergence instead
   of resetting it.

7. Update every feature/fix branch onto the new `main`:

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

8. Rebase `main-fork` onto the updated `main`:

   ```bash
   git switch main-fork
   git rebase main
   ```

   Resolve conflicts by preserving the fork-only features and fixes listed in
   `README.fork.md`. Do not accept upstream wholesale when doing so removes fork
   behavior. If upstream changed the same feature, compare behavior instead of
   only comparing files; keep the fork behavior unless upstream is verified to
   provide an equal or better equivalent. If Rust code, tests, schema files, or
   dependencies are changed during conflict resolution, follow the repository
   `just` validation rules for the affected crate or workspace.

9. Re-apply the feature/fix branches onto `main-fork` in the confirmed order:

   ```bash
   git switch main-fork
   for branch in $feature_fix_branches; do
     git merge --no-ff --no-edit "$branch"
   done
   ```

   Resolve each merge conflict before moving to the next branch. Keep the final
   `main-fork` history readable. After each merge, re-check the touched paths
   against the `README.fork.md` inventory before continuing and report any
   branch that was already fully contained.

10. Verify the final branch state:

   ```bash
   git status --short --branch
   git log --oneline --decorate --graph --max-count=30 \
     main main-fork $feature_fix_branches
   git diff --name-status main...main-fork -- \
     README.fork.md \
     .gitleaksignore \
     .codex/skills/upgrade-apohl79-fork \
     codex-rs/config/src \
     codex-rs/tui/src \
     scripts
   ```

   Confirm that `main-fork` contains the updated `main` base and the current
   feature/fix branch heads. Confirm that every item listed in `README.fork.md`
   is still present or that `README.fork.md` explains why upstream now covers it.
   Derive any source searches from the paths and behavior described in
   `README.fork.md`; do not add a separate hardcoded feature checklist to this
   skill.

11. Run targeted validation for changed fork areas:

    - For TUI `@` completion, popup rendering, status line, or TUI snapshots,
      from `codex-rs`:

      ```bash
      cd codex-rs
      just fmt
      just test -p codex-tui
      cargo insta pending-snapshots -p codex-tui
      ```

      Review and accept intended snapshot changes before pushing.

    - For release helper changes, from the repository root:

      ```bash
      python3 scripts/test_apohl79_release.py
      ```

    - For config schema changes, from `codex-rs`:

      ```bash
      cd codex-rs
      just write-config-schema
      ```

    - For dependency changes, from the repository root:

      ```bash
      just bazel-lock-update
      just bazel-lock-check
      ```

    Run any additional repository-required checks for files changed during
    conflict resolution.

12. Push the updated feature/fix branches and `main-fork` to apohl79:

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
the `README.fork.md` inventory result, any `README.fork.md` updates, the
validation performed, and the push result. Distinguish local-only updates from
updates already pushed to `origin`.

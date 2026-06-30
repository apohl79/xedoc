---
name: upgrade-apohl79-fork
description: Upgrade the local apohl79 Codex fork from an OpenAI upstream release tag while preserving the fork-only inventory in README.fork.md on main-fork.
---

# Upgrade Apohl79 Fork

## Overview

Use this workflow to advance `main-fork` to a requested upstream OpenAI Codex
release tag while preserving the fork-local inventory in `README.fork.md`.

`main-fork` is the durable fork branch. `feature/*` and `fix/*` branches are
short-lived staging or review branches; do not discover, rebase, re-apply, or
push them during a fork upgrade unless the user explicitly names them and asks
for that extra branch work.

## Guardrails

- Require a clean worktree before starting. Stop on uncommitted changes unless
  the user explicitly asks how to handle them.
- Require an explicit upstream tag from the user, such as
  `rust-v0.142.4`. Do not guess the target tag.
- Verify remotes before fetching or pushing:
  - `upstream` must be `openai/codex`.
  - `origin` must be `apohl79/codex`.
  - Stop for confirmation if the remote names or URLs differ.
- Treat local `main` as the exact upstream release base after it is updated.
  Do not push `main` to `origin` unless the user separately asks for that.
- Prefer fast-forwarding local `main` to the requested upstream tag. If local
  `main` points exactly at an older upstream release tag and has no local
  changes or commits, a backup-protected ref move to the requested tag is
  allowed after explicit user confirmation. Do not use `upstream/main` as the
  base for a release-tag upgrade.
- Create backup refs before rewriting `main` or `main-fork`. Use
  `refs/backup/apohl79-upgrade/<timestamp>/<branch>`.
- Treat `README.fork.md` as the single source of truth for fork-only features
  and fixes. Do not keep a second feature list in this skill.
- Preserve every fork behavior listed in `README.fork.md`. If upstream now
  implements equivalent behavior, verify the equivalence and update
  `README.fork.md` to explain that the behavior is no longer fork-only.
- Keep `README.fork.md` current when the upgrade changes which features or
  fixes are fork-only.
- Push rewritten history with `--force-with-lease`, never `--force`.
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
   git fetch upstream "refs/tags/${tag}:refs/tags/${tag}"
   git rev-parse --verify "$tag^{commit}"
   ```

3. Establish the fork preservation baseline:

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

   Record the current fork-only inventory from `README.fork.md`. Use the log
   and focused diff as evidence for conflict resolution, not as a second source
   of truth. Stop if `README.fork.md` is missing; recreate or recover it from
   git evidence before rewriting branches.

4. Create backup refs for branches that can be rewritten:

   ```bash
   timestamp=$(date -u +%Y%m%dT%H%M%SZ)
   for branch in main main-fork; do
     git show-ref --verify --quiet "refs/heads/$branch" || continue
     git update-ref "refs/backup/apohl79-upgrade/$timestamp/$branch" "$branch"
   done
   ```

   If the user explicitly named additional staging branches for the upgrade,
   back up those branches too before touching them.

5. Update local `main` to the upstream tag, preferring a fast-forward:

   ```bash
   git switch main
   git merge --ff-only "$tag"
   git status --short --branch
   ```

   If `main` cannot fast-forward to the tag, do not reset it. First verify
   whether `main` is exactly an older upstream release tag with no local
   changes:

   ```bash
   previous_tag=$(git tag --points-at main | head -n1)
   test -n "$previous_tag"
   git diff --stat "$previous_tag"..main
   git log --cherry-pick --right-only --oneline "$previous_tag"...main
   git log --left-right --cherry-pick --oneline main..."$tag"
   ```

   If `main` has no diff and no right-only commits relative to the older
   release tag, ask for confirmation. After backup refs have been created, move
   only the local `main` ref to the requested release commit:

   ```bash
   git update-ref refs/heads/main "$tag^{commit}"
   ```

   Stop and report the divergence if local `main` contains any non-upstream
   commits, the previous release tag cannot be verified, or the user does not
   confirm the ref move.

6. Rebase `main-fork` onto the updated `main` unless the user explicitly asks
   for a merge-based upgrade:

   ```bash
   git switch main-fork
   git rebase main
   ```

   Resolve conflicts by preserving the fork-only behavior listed in
   `README.fork.md`. Do not accept upstream wholesale when doing so removes fork
   behavior. If upstream changed the same feature, compare behavior instead of
   only comparing files; keep the fork behavior unless upstream is verified to
   provide an equal or better equivalent.

   If Rust code, tests, schema files, or dependencies are changed during
   conflict resolution, follow the repository `just` validation rules for the
   affected crate or workspace.

7. Handle explicitly supplied staging branches only if requested:

   ```bash
   git switch <branch>
   git rebase main-fork
   ```

   Push those branches only if the user asked for branch updates. Use
   `--force-with-lease` for rebased branches. Skip this step for ordinary fork
   upgrades.

8. Verify the final branch state:

   ```bash
   git status --short --branch
   git log --oneline --decorate --graph --max-count=30 main main-fork
   git diff --name-status main...main-fork -- \
     README.fork.md \
     .gitleaksignore \
     .codex/skills/upgrade-apohl79-fork \
     codex-rs/config/src \
     codex-rs/tui/src \
     scripts
   ```

   Confirm that `main-fork` contains the updated `main` base. Confirm that
   every item listed in `README.fork.md` is still present or that
   `README.fork.md` explains why upstream now covers it. Derive source searches
   from the paths and behavior described in `README.fork.md`; do not add a
   separate hardcoded feature checklist to this skill.

9. Run targeted validation for changed fork areas:

   - For TUI `@` completion, popup rendering, status line, active task list, or
     TUI snapshots, from `codex-rs`:

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

10. Push `main-fork` to apohl79:

    ```bash
    git push --force-with-lease origin main-fork
    ```

    Show the exact push command before running it. If the final `main-fork`
    update is a fast-forward relative to `origin/main-fork`, a normal
    `git push origin main-fork` is acceptable.

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

Report the requested tag, the backup ref timestamp, the `README.fork.md`
inventory result, any `README.fork.md` updates, the validation performed, and
the push result. Distinguish local-only updates from updates already pushed to
`origin`.

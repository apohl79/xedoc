---
name: upgrade-apohl79-fork
description: Upgrade the local apohl79 Codex fork from an OpenAI upstream release tag while preserving the fork-only inventory in README.fork.md on main-fork.
---

# Upgrade Apohl79 Fork

## Overview

Use this workflow to advance `main-fork` from one requested upstream OpenAI
Codex release tag to another while preserving the fork-local inventory in
`README.fork.md`. Replay upstream history one commit at a time: first advance
local `main` through the exact upstream commits, then merge that same commit
into `main-fork` and resolve it against the inventory.

`main-fork` is the durable fork branch. `feature/*` and `fix/*` branches are
short-lived staging or review branches; do not discover, rebase, re-apply, or
push them during a fork upgrade unless the user explicitly names them and asks
for that extra branch work.

## Guardrails

- Require a clean worktree before starting. Stop on uncommitted changes unless
  the user explicitly asks how to handle them.
- Require explicit current and target upstream tags from the user, such as
  `rust-v0.144.0` and `rust-v0.145.0`. Do not guess either tag.
- Verify remotes before fetching or pushing:
  - `upstream` must be `openai/codex`.
  - `origin` must be `apohl79/codex`.
  - Stop for confirmation if the remote names or URLs differ.
- Local `main` is an upstream-only release track. It must point exactly at the
  current release tag before replay and advances one upstream commit at a
  time. Do not push `main` to `origin` unless the user separately asks.
- The replay range is `current-tag..target-tag`; do not substitute
  `upstream/main` or an untagged commit.
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

2. Fetch and verify the current and target upstream tags:

   ```bash
   current_tag=<current-upstream-tag>
   target_tag=<target-upstream-tag>
   git fetch upstream "refs/tags/${current_tag}:refs/tags/${current_tag}"
   git fetch upstream "refs/tags/${target_tag}:refs/tags/${target_tag}"
   git fetch upstream --tags
   git rev-parse --verify "$current_tag^{commit}"
   git rev-parse --verify "$target_tag^{commit}"
   git merge-base --is-ancestor "$current_tag" "$target_tag"
   git switch main
   test "$(git rev-parse HEAD)" = "$(git rev-parse "$current_tag^{commit}")"
   ```

   Stop if `main` is not exactly the current release tag. Do not reset it to
   manufacture the baseline.

3. Establish the fork preservation baseline:

   ```bash
   git switch main-fork
   test -f README.fork.md
   sed -n '1,240p' README.fork.md
   git log --cherry-pick --right-only --oneline "$current_tag"...main-fork
   git diff --name-status "$current_tag"...main-fork -- \
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

5. Map alpha release tags to their final code commits and identify validation
   checkpoints before replaying any code. An alpha tag commonly carries
   release metadata; its first parent is the last code-bearing commit for that
   alpha. Map that parent into the replay range, then checkpoint every tenth
   discovered alpha tag and the final target tag.

   ```bash
   target_version=${target_tag#rust-v}
   git rev-list --reverse "$current_tag..$target_tag" > /tmp/apohl79-replay-commits
   nl -ba /tmp/apohl79-replay-commits
   git tag --list "rust-v${target_version}-alpha.*" --sort=v:refname |
     while read -r alpha_tag; do
       tag_commit=$(git rev-parse "${alpha_tag}^{commit}")
       code_commit=$(git rev-parse "${tag_commit}^1")
       if git merge-base --is-ancestor "$code_commit" "$target_tag"; then
         ordinal=$(nl -ba /tmp/apohl79-replay-commits |
           awk -v commit="$code_commit" '$2 == commit { print $1 }')
         printf '%s\t%s\t%s\n' "$alpha_tag" "$code_commit" "$ordinal"
       fi
     done | tee /tmp/apohl79-alpha-checkpoints
   ```

   Keep the resulting mapping with the upgrade notes. Mark every tenth row in
   alpha-version order as a full-validation checkpoint, plus the final target
   release commit. If an alpha tag does not map into the range, record that
   fact and do not invent a checkpoint commit.

6. Replay each upstream commit in chronological order. Advance `main` with a
   fast-forward for the exact upstream commit, then create a separate merge on
   `main-fork` for that same commit. This makes each conflict and its
   fork-preservation resolution reviewable without importing the whole range
   at once:

   ```bash
   while read -r commit; do
     git switch main
     git merge --ff-only "$commit"

     git switch main-fork
     git merge --no-ff --no-commit "$commit"

     # Inspect the single-commit diff and resolve only with README.fork.md
     # behavior preserved.
     git diff --check
     # Run the formatter/linter configured for the touched paths. Do not run
     # compilation or tests for each replayed commit.
     git commit -m "merge: replay upstream $(git show -s --format=%h "$commit")"
   done < /tmp/apohl79-replay-commits
   ```

   Resolve every conflict behaviorally, not by accepting one side wholesale.
   If the replayed upstream commit or a conflict resolution changes
   binary-shipped code, increment `scripts/apohl79_build_number.txt` in that
   same `main-fork` commit. Do not compile or run tests between ordinary replay
   commits; formatting and lightweight lint/diff checks are required on each
   commit.

7. At each mapped checkpoint, run the full validation script only after the
   checkpoint merge is committed. Start it in the background, wait solely for
   its exit status, and inspect the log only after it ends. Do not tail, poll,
   or otherwise monitor the script while it runs:

   ```bash
   checkpoint=<alpha-tag-or-target>
   log="/tmp/apohl79-full-validation-${checkpoint}.log"
   scripts/run-full-validation.sh >"$log" 2>&1 &
   validation_pid=$!
   wait "$validation_pid"
   validation_status=$?
   if test "$validation_status" -ne 0; then
     sed -n '1,240p' "$log"
   fi
   ```

   When validation fails, inspect the completed log, fix the issue on
   `main-fork`, rerun the same checkpoint validation with the same no-monitor
   rule, and continue only after it passes. Repeat this procedure until the
   target release checkpoint passes.

8. Handle explicitly supplied staging branches only if requested:

   ```bash
   git switch <branch>
   git rebase main-fork
   ```

   Push those branches only if the user asked for branch updates. Use
   `--force-with-lease` for rebased branches. Skip this step for ordinary fork
   upgrades.

9. Verify the final branch state:

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

10. Review the final validation result and run any targeted follow-up checks
   required by the changed fork areas:

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

11. Push `main-fork` to apohl79:

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

Report the current and target tags, replay commit count, alpha-to-code-commit
mapping, checkpoint results, the backup ref timestamp, the `README.fork.md`
inventory result, any inventory updates, and the push result. Distinguish
local-only updates from updates already pushed to `origin`.

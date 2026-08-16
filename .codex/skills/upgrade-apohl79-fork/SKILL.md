---
name: upgrade-apohl79-fork
description: Upgrade the local apohl79 Codex fork to a newer stable OpenAI release. Use when discovering newer upstream releases, staging a fork upgrade on an upgrade branch, replaying upstream commits through alpha and stable checkpoints, or preserving and auditing the README.fork.md inventory.
---

# Upgrade Apohl79 Fork

Use this workflow to prepare, but not merge, an upgrade of `main-fork` to a
newer stable OpenAI Codex release. The output is an `upgrade-<version>` branch
that remains checked out, an updated `README.fork.md`, and an
`upgrade-fork.md` replay record.

## Guardrails

- Require a clean worktree. Stop on unrelated changes; do not stash, reset, or
  delete them without the user's explicit direction.
- Verify `upstream` is `openai/codex` and `origin` is `apohl79/codex` before
  fetching. Stop for confirmation if either mapping differs.
- Treat `README.fork.md` as the complete, single source of truth for
  fork-only features and fixes. Git history and diffs find missing inventory
  items; they are not a competing feature list.
- Do not update, merge into, rebase, push, or otherwise move `main` or
  `main-fork`. Do not push the upgrade branch. Leave the upgrade branch
  checked out when the work stops.
- Do not use `upstream/main` or an untagged commit as a release boundary.
  Upstream release tags must match `rust-v<major>.<minor>.<patch>` for a stable
  release or `rust-v<major>.<minor>.<patch>-alpha.<n>` for an alpha checkpoint.
- Create each imported upstream commit as a separate merge commit. Resolve
  conflicts behaviorally and preserve every README inventory behavior.
- Before every commit, initialize GPG when needed:

  ```bash
  export GPG_TTY=$(tty)
  gpg-agent --daemon 2>/dev/null || true
  ```

- Use Conventional Commit subjects. When an upstream merge or its resolution
  changes shipped binary code, update `scripts/apohl79_build_number.txt` in
  that same merge commit.
- Use `CARGO_BUILD_JOBS=2` for checkpoint tests. Test only at every tenth
  alpha checkpoint and at each stable-release checkpoint; do not test after
  ordinary upstream commits.
- Require a `gpt-luna` subagent with high reasoning effort for each release
  interval. If that model or effort is unavailable, stop and ask the user for
  a replacement; do not silently choose another model.

## 1. Discover the release boundary and ask for the target

Inspect the state and fetch upstream tags:

```bash
git status --short --branch
git remote -v
git fetch upstream --prune --tags
git fetch origin --prune
test -f README.fork.md
```

Derive the fork's current stable base from the workspace version, then verify
that the corresponding upstream stable tag exists.

```bash
base_version=$(sed -n 's/^version = "\([0-9][^"]*\)"/\1/p' codex-rs/Cargo.toml | head -1)
current_tag="rust-v${base_version}"
git rev-parse --verify "${current_tag}^{commit}"
git tag --list 'rust-v*' --sort=-version:refname |
  rg '^rust-v[0-9]+\.[0-9]+\.[0-9]+$' |
  awk -v current="$current_tag" '$0 == current { exit } { print }' |
  tee /tmp/apohl79-newer-stable-tags
```

Show every tag in `/tmp/apohl79-newer-stable-tags`, recommend its first (the
latest stable release), and ask the user which tag to use. Do not create a
branch or begin replay until the user selects the target tag.

## 2. Create or resume the isolated upgrade branch

Normalize the user-selected stable tag and name the branch from its version:

```bash
target_tag=<user-selected-rust-vX.Y.Z>
printf '%s\n' "$target_tag" | rg '^rust-v[0-9]+\.[0-9]+\.[0-9]+$'
git rev-parse --verify "${target_tag}^{commit}"
git merge-base --is-ancestor "$current_tag" "$target_tag"
target_release=${target_tag#rust-v}
upgrade_branch="upgrade-${target_release}"
```

For a new upgrade, create the branch directly from the current `main-fork`:

```bash
git show-ref --verify --quiet "refs/heads/${upgrade_branch}" && exit 1
git switch -c "$upgrade_branch" main-fork
```

For a resumed upgrade, switch to the existing branch only after verifying that
its `upgrade-fork.md` names the same current and target tags and records a
cursor that is an ancestor of `HEAD`. Do not restart a partially completed
upgrade from `main-fork` or overwrite its notes.

## 3. Audit the complete fork inventory before replay

Read all of `README.fork.md`. Then compare the current stable base with both
the fork and the new upgrade branch to identify fork-local behavior that is
not represented in the inventory:

```bash
sed -n '1,999p' README.fork.md
git log --cherry-pick --right-only --oneline "$current_tag"...main-fork
git diff --name-status "$current_tag"...main-fork
git diff --name-status "$current_tag"...HEAD
```

For every fork-only behavior found in source, tests, release tooling, or the
fork-only history, ensure `README.fork.md` has an accurate entry with its
behavioral contract and primary files. Remove no entry merely because a file
was renamed; investigate it. If upstream supplies an equivalent behavior,
verify it semantically, then revise the entry to explain that it is no longer
fork-only. Commit inventory corrections before starting the first interval.

During every later conflict resolution and before final reporting, repeat this
audit for changed inventory areas. A heading alone is insufficient: search its
primary files and relevant tests to verify the documented behavior still works.

## 4. Generate and maintain `upgrade-fork.md`

Create `upgrade-fork.md` in the repository root on a new upgrade branch. It
must contain the current tag, target tag, upgrade branch, initial cursor, and a
chronological table of every commit in `current_tag..target_tag`. For each
commit, list its full and short SHA, subject, associated release tag(s), and
checkpoint status.

Mark every commit that is exactly the peeled commit of an alpha tag as an alpha
checkpoint; do not substitute its parent. Mark every tenth alpha checkpoint
since `current_tag` as a test checkpoint, and mark all stable release-tag
commits as test checkpoints. Preserve all rows on later rounds and update only
their status, completion commit, validation result, and the current cursor.

Build and check the tag mapping before writing the table:

```bash
git rev-list --reverse "$current_tag..$target_tag" > /tmp/apohl79-replay-commits
git tag --list 'rust-v*' --sort=version:refname |
  rg '^rust-v[0-9]+\.[0-9]+\.[0-9]+(-alpha\.[0-9]+)?$' |
  while read -r release_tag; do
    release_commit=$(git rev-parse "${release_tag}^{commit}")
    if test "$release_commit" != "$(git rev-parse "${current_tag}^{commit}")" &&
      git merge-base --is-ancestor "$current_tag" "$release_commit" &&
      git merge-base --is-ancestor "$release_commit" "$target_tag"; then
      printf '%s\t%s\n' "$release_commit" "$release_tag"
    fi
  done > /tmp/apohl79-release-checkpoints
```

Use `git log --reverse --format='%H%x09%h%x09%s' "$current_tag..$target_tag"`
as the authoritative commit order. Verify that each tag mapping occurs in that
range before adding it to the report. Commit the initial report with a
Conventional Commit subject such as `docs(upgrade): initialize <target_tag> replay`.

## 5. Replay one alpha/stable interval at a time

The first cursor is `current_tag`. On a resumed upgrade, use the last completed
alpha or stable tag recorded in `upgrade-fork.md`; verify it is reachable from
`HEAD`. Select the next alpha or stable tag in chronological order, never skip
one, and repeat this section until reaching `target_tag`.

For each interval, spawn one high-effort `gpt-luna` subagent. Give it exclusive
responsibility for the current upgrade branch and interval, with a bounded
task name. Set the subagent request's `model` to `gpt-luna` and its
`reasoning_effort` to `high`. Use a bounded prompt equivalent to:

```text
On branch <upgrade-branch>, replay every upstream commit in <cursor>..<next-tag>
as an individual merge commit. Preserve and update every README.fork.md inventory
behavior, resolve conflicts semantically, format changed Rust code, and update
upgrade-fork.md when the interval completes. Do not move, merge, rebase, or push
main or main-fork. Do not run tests unless this interval is an assigned test
checkpoint. Report conflicts, inventory changes, and commits created.
```

The subagent replays only its interval in chronological order:

```bash
git rev-list --reverse "$cursor..$next_tag" > /tmp/apohl79-interval-commits
while read -r upstream_commit; do
  git merge --no-ff --no-commit "$upstream_commit"

  # Resolve conflicts from README.fork.md's behavioral contract, not by
  # accepting either side wholesale.
  git diff --check
  # Run just fmt from codex-rs whenever code changed.
  git commit -m "chore(upgrade): replay upstream $(git show -s --format=%h "$upstream_commit")"
done < /tmp/apohl79-interval-commits
```

Review the subagent's result before continuing. Ensure each upstream commit has
one corresponding merge commit, `git diff --check` is clean, every touched fork
feature remains documented and working, and `upgrade-fork.md` records the
completed interval. Commit the report update before testing so validation starts
from a clean worktree.

## 6. Test only scheduled checkpoints and clean up afterward

Run the complete checkpoint validation only when `upgrade-fork.md` marks the
newly reached interval as every tenth alpha checkpoint or a stable release.
Use at most two Cargo compilation jobs:

```bash
checkpoint=<next-tag>
log="/tmp/apohl79-full-validation-${checkpoint}.log"
CARGO_BUILD_JOBS=2 scripts/run-full-validation.sh >"$log" 2>&1
validation_status=$?
sed -n '1,240p' "$log"
```

If validation fails, inspect the completed log, fix the issue on the upgrade
branch, format and commit the fix, update `README.fork.md` when its behavioral
contract changed, and rerun the same checkpoint. Do not advance to the next
alpha or stable tag until it passes.

After every checkpoint test invocation, successful or failed, remove only
rebuildable development and test artifacts. Preserve release artifacts,
including `codex-rs/target/release` and target-triple release directories.

```bash
(
  cd codex-rs
  cargo clean --profile dev
  rm -rf target/nextest target/tmp
)
git status --short --branch
```

Record the validation result and the new cursor in `upgrade-fork.md` after the
round. Do not run tests for non-checkpoint intervals.

## 7. Final audit and stopping condition

At the target stable release, prove that the branch contains all upstream
commits, every inventory behavior has been checked, and no accidental branch
movement occurred:

```bash
git merge-base --is-ancestor main-fork "$upgrade_branch"
git merge-base --is-ancestor "$target_tag" HEAD
git diff --check
git status --short --branch
git log --oneline --decorate --graph --max-count=30
```

Update `README.fork.md` for every verified fork feature and every behavior now
provided by upstream. Update `upgrade-fork.md` with the final target status,
all alpha/stable checkpoint outcomes, and the final inventory-audit result.

Report the current and target tags, created branch, staged interval results,
checkpoint test outcomes, README inventory changes, and a concise summary of
the major upstream changes. Stop there: do not merge the upgrade branch into
`main-fork`, do not push it, and leave `upgrade-<target-release>` checked out.

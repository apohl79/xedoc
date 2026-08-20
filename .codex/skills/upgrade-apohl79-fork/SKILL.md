---
name: upgrade-apohl79-fork
description: Upgrade the local apohl79 Codex fork to a newer stable OpenAI release. Use when discovering newer upstream releases, staging a fork upgrade on an upgrade branch, replaying upstream commits through alpha and stable checkpoints, or preserving and auditing the README.fork.md inventory.
---

# Upgrade Apohl79 Fork

Use this workflow to prepare, but not merge, an upgrade of `main-fork` to a
newer stable OpenAI Codex release. The output is an `upgrade-<version>` branch
that remains checked out, an updated `README.fork.md`, and an
`upgrade-fork.md` replay record. The replay record is temporary and
branch-local: it must never be merged or copied onto `main-fork`, and it is
removed when the upgrade branch is discarded.

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
- Do not use `upstream/main` or an arbitrary untagged commit as a release
  boundary.
  Upstream release tags must match `rust-v<major>.<minor>.<patch>` for a stable
  release or `rust-v<major>.<minor>.<patch>-alpha.<n>` for an alpha checkpoint.
  Stable tags can be CI/CD versioning heads that are not ancestors of later
  stable tags. Resolve a stable tag's first-parent release-line history to its
  nearest exact code commit reachable from `upstream/main`; use the tag for
  release identity and checkpoint naming, but do not use its version-only head
  as the starting boundary. Apply the same resolution to alpha tags; retain an
  exact peeled tag head only when it is actually in the replay range.
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
- Use `CARGO_BUILD_JOBS=2` for checkpoint validation. A checkpoint is every
  tenth alpha release (`alpha.10`, `alpha.20`, and so on; parse the numeric
  suffix) and every stable release. Other alpha releases are replay-only
  endpoints and must not trigger full validation. Immediately after the merge
  that reaches a checkpoint, run `scripts/run-full-validation.sh` and require
  `validation_status=0` before advancing. Run it again at the final target
  endpoint even when that endpoint is already a stable checkpoint; do not test
  after ordinary upstream commits.
- Once, before the first replay interval starts, enumerate every execution
  model currently available to the delegation API and every reasoning effort
  supported by the selected model. Display both as complete numbered lists;
  do not show only a curated subset. Require the user to enter a number for
  the model and a number for the effort. Reject non-numeric or out-of-range
  input and ask again. Never silently substitute an unapproved model or
  effort. Persist the approved model/effort pair and reuse it autonomously for
  every later interval; do not refresh the catalog or prompt again between
  intervals. When the approved pair exactly matches the active runtime model
  and effort, call `spawn_agent` without explicit `model` or
  `reasoning_effort` overrides so the delegation API inherits the approved
  pair. Pass explicit overrides only when the approved pair differs from the
  active runtime pair. If an explicit delegation override is rejected, stop
  the upgrade and report the rejection; never substitute another pair.
- Maintain a persistent task plan for the upgrade and show overall replay
  progress with checkpoint completion above merged-commit progress. Compute
  the merged-commit denominator once with
  `git rev-list --count "$current_code_commit..$target_tag"`; compute the
  merged-commit numerator from unique second parents of completed first-parent
  replay merges, not from the current interval or checkpoint count:

  ```bash
  total=$(git rev-list --count "$current_code_commit..$target_tag")
  merged=$(git log --first-parent --merges --format='%P' \
    "$current_code_commit..$upgrade_branch" |
    awk 'NF == 2 { print $2 }' | sort -u |
    while read -r parent; do
      if git merge-base --is-ancestor "$current_code_commit" "$parent" &&
        git merge-base --is-ancestor "$parent" "$target_tag"; then
        printf '%s\n' "$parent"
      fi
    done | sort -u | wc -l | tr -d ' ')
  percent=$((merged * 100 / total))

  # Count unique scheduled checkpoint rows in the release-checkpoint table.
  # Count every successful complete-suite run, including a successful rerun
  # at a checkpoint that was previously attempted.
  total_checkpoints=$(awk -F'|' '
    /^## Release Checkpoints/ { in_release = 1; next }
    /^## / && in_release { exit }
    in_release && /^\|/ && $5 ~ /checkpoint/ && $5 !~ /Classification/ { n++ }
    END { print n + 0 }
  ' upgrade-fork.md)
  passed_checkpoints=$(awk -F'|' '
    /^## Commit Replay Ledger/ { in_ledger = 1; next }
    /^## / && in_ledger { exit }
    in_ledger && /^\|/ && $7 ~ /checkpoint/ {
      n += gsub(/passed \(validation_status=0\)/, "&", $9)
    }
    END { print n + 0 }
  ' upgrade-fork.md)
  ```

  The ancestry filter excludes pre-upgrade fork merge parents that happen to
  be reachable from the branch but are not part of the selected upstream
  replay range.

  Keep one task for the current interval and one persistent overall task whose
  text contains this single line:

  ```text
  Upgrade Progress: <merged>/<total> commits merged - <passed_checkpoints>/<total_checkpoints> checkpoints passed - <percent_with_comma>% finished
  ```

  Update this line after every interval and validation run. Format the overall
  percentage to two decimal places using a comma decimal separator (for
  example, `12,37%`). Increment `passed_checkpoints` once for every complete
  validation-suite run that returns zero, including successful reruns of the
  same checkpoint. Reaching a checkpoint endpoint or recording a partial or
  failed result does not count. Never label an interval percentage as the
  overall upgrade percentage.

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
current_tag_commit=$(git rev-parse "${current_tag}^{commit}")
current_code_commit=$(git rev-parse "${current_tag_commit}^1")
git merge-base --is-ancestor "$current_code_commit" upstream/main
git tag --list 'rust-v*' --sort=-version:refname |
  rg '^rust-v[0-9]+\.[0-9]+\.[0-9]+$' |
  awk -v current="$current_tag" '$0 == current { exit } { print }' |
  tee /tmp/apohl79-newer-stable-tags
```

Show every tag in `/tmp/apohl79-newer-stable-tags`, recommend its first (the
latest stable release), and ask the user which tag to use. Do not create a
branch or begin replay until the user selects the target tag.

Release tags are often CI/CD heads: inspect the peeled commit and its
first-parent release-line history before validating ancestry. The nearest
release-line commit that is an exact ancestor of `upstream/main` is the
release's code boundary; the tag head may contain only version metadata and may
not be present in later release history. If the direct parent is not on
`upstream/main`, inspect the matching `upstream/release/*` first-parent history
and walk backward until an exact mainline commit is found. Record the tag head
and resolved code commit; abort if no such match exists. This derived code
boundary is allowed only because it comes from the selected tagged release,
and must not be replaced with an arbitrary `upstream/main` or guessed commit.

## 2. Create or resume the isolated upgrade branch

Normalize the user-selected stable tag and name the branch from its version:

```bash
target_tag=<user-selected-rust-vX.Y.Z>
printf '%s\n' "$target_tag" | rg '^rust-v[0-9]+\.[0-9]+\.[0-9]+$'
git rev-parse --verify "${target_tag}^{commit}"
target_tag_commit=$(git rev-parse "${target_tag}^{commit}")
target_code_commit=$(git rev-parse "${target_tag_commit}^1")
git merge-base --is-ancestor "$target_code_commit" upstream/main
git merge-base --is-ancestor "$current_code_commit" "$target_tag"
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
git log --cherry-pick --right-only --oneline "$current_code_commit"...main-fork
git diff --name-status "$current_code_commit"...main-fork
git diff --name-status "$current_code_commit"...HEAD
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
must contain the current tag, target tag, their resolved code-boundary commits,
the upgrade branch, initial cursor, and a chronological table of every commit
in `current_code_commit..target_tag`. For each commit, list its full and short
SHA, subject, associated release tag(s), and checkpoint status. The current
stable tag's version-only CI/CD head is excluded by starting at its first
parent; the target stable tag head remains in the replay range so the target
version metadata is imported.

`upgrade-fork.md` is temporary upgrade-workflow state, not product
documentation. Keep it only on the upgrade branch; never include it in a
`main-fork` merge or cherry-pick. When abandoning or deleting the upgrade
branch, remove this file with the branch rather than carrying it forward.

Mark an exact peeled alpha-tag commit as a checkpoint only when its numeric
suffix is divisible by ten and it is in the replay range. When a CI/CD alpha
head is outside that range, mark its resolved mainline code-boundary commit
instead and retain both IDs in the report. Keep one row per unique checkpoint
commit while associating duplicate release tags with that row. Mark every
stable release-tag checkpoint as a test checkpoint. Keep other alpha rows as
replay-only endpoints; they must not trigger full validation. Preserve all rows
on later rounds and update only their status, completion commit, validation
result, and the current cursor.

Build and check the tag mapping before writing the table. Release tags whose
CI/CD heads are not in the target ancestry map to the nearest mainline commit
on their first-parent release history; a release tag head that is in the range
maps to the head itself. In both cases, retain the release tag and both commit
IDs in the report:

```bash
git rev-list --reverse "$current_code_commit..$target_tag" > /tmp/apohl79-replay-commits
git tag --list 'rust-v*' --sort=version:refname |
  rg '^rust-v[0-9]+\.[0-9]+\.[0-9]+(-alpha\.[0-9]+)?$' |
  while read -r release_tag; do
    release_head=$(git rev-parse "${release_tag}^{commit}")
    release_code=''
    while read -r candidate; do
      if git merge-base --is-ancestor "$candidate" upstream/main; then
        release_code="$candidate"
        break
      fi
    done < <(git rev-list --first-parent "${release_head}^1")
    test -n "$release_code" || continue
    checkpoint_commit="$release_code"
    if git merge-base --is-ancestor "$release_head" "$target_tag"; then
      checkpoint_commit="$release_head"
    fi
    if git merge-base --is-ancestor "$current_code_commit" "$checkpoint_commit" &&
      git merge-base --is-ancestor "$checkpoint_commit" "$target_tag"; then
      printf '%s\t%s\t%s\t%s\n' \
        "$checkpoint_commit" "$release_tag" "$release_head" "$release_code"
    fi
  done > /tmp/apohl79-release-checkpoints
```

Use `git log --reverse --format='%H%x09%h%x09%s' "$current_code_commit..$target_tag"`
as the authoritative commit order. Verify that each checkpoint commit occurs
in that range before adding it to the report. Commit the initial report with a
Conventional Commit subject such as `docs(upgrade): initialize <target_tag> replay`.

## 5. Replay one alpha/stable interval at a time

The first cursor is `current_code_commit`. On a resumed upgrade, use the last
completed alpha or stable tag's recorded checkpoint commit from
`upgrade-fork.md`; verify it is reachable from `HEAD`. Select the next alpha or
stable tag in chronological order, never skip one, and repeat this section
until reaching `target_tag`.

Classify the endpoint before starting the interval. It is a validation
checkpoint when it is a stable release or an alpha release whose numeric
suffix is divisible by ten; every other alpha endpoint is replay-only. Include
the classification and the full endpoint SHA in the task message and task
plan. After the endpoint merge and the clean `upgrade-fork.md` interval report
commit, the agent must run the complete validation command in Section 6 for a
checkpoint. Do not start the next interval, mark the checkpoint complete, or
record a passing result until that command exits zero. A disk-protective stop,
interrupt, or partial log is a failed checkpoint and must be rerun from the
same endpoint.

Before the first interval only, refresh the complete model catalog from the
CLI. `codex debug models` returns the provider catalog as JSON; `--bundled` is
not sufficient because it omits configured provider models. Use this command
to display only each stable model identifier and its supported reasoning
efforts:

```bash
codex debug models |
  jq -r '.models[] |
    [.slug, ((.supported_reasoning_levels // []) | map(.effort) | join(","))] |
    @tsv'
```

Require this command to exit successfully and display every returned row as a
numbered model list. Prompt exactly once for a model number. Validate that the
response is a decimal integer in range; reject model names, blank input, and
out-of-range numbers. After the model is selected, take its supported efforts
from the same row, display every effort as a numbered list, and prompt exactly
once for an effort number using the same validation rules. Do not assume that
every model supports the same efforts. Persist the selected model identifier
and effort in the overall task and `upgrade-fork.md` review before spawning the
first worker; subsequent interval tasks inherit this approved pair without
user interaction.
Give the subagent exclusive responsibility for the current upgrade branch and
interval, with a bounded task name. Use a bounded prompt equivalent to:

```text
On branch <upgrade-branch>, replay every upstream commit in <cursor>..<next-tag>
as an individual merge commit. Preserve and update every README.fork.md inventory
behavior, resolve conflicts semantically, format changed Rust code, and update
upgrade-fork.md when the interval completes. Do not move, merge, rebase, or push
main or main-fork. Do not run tests while replaying ordinary commits. If this
interval ends at a scheduled checkpoint, create the endpoint merge and clean
report commit, then invoke `scripts/run-full-validation.sh` as required by
Section 6 before reporting the interval complete. Report conflicts, inventory
changes, and commits created.
```

Prefer a fresh subagent task name for every interval so the task tree names the
actual checkpoint. If a follow-up reuses an existing child because no slot is
available, the UI retains that child's original task name; include the current
interval, cursor, endpoint, and overall percentage in the follow-up message and
task plan, and treat the label as historical. Verify the active interval from
the signed replay report and Git second-parent audit rather than from the label.

The interval endpoint is a hard boundary: provide the full endpoint SHA and
require the worker to stop immediately after creating the merge whose second
parent is that exact SHA. It must not continue into the next checkpoint while
preparing or signing the report. Before accepting the interval, independently
verify that the endpoint is the last expected upstream second parent and that
no later upstream second parent is reachable from the upgrade branch. If a
worker overruns, preserve the overrun under a temporary local ref, restore the
upgrade branch to its last signed interval report, and restart only the bounded
interval.

Do not skip an expected SHA because its effective tree delta is empty or its
changes are already present after conflict resolution. Every SHA in the
authoritative `git rev-list --reverse "$cursor..$next_tag"` list still needs
its own signed merge commit and exact second parent.

The subagent replays only its interval in chronological order:

```bash
git rev-list --reverse "$cursor..$next_tag" > /tmp/apohl79-interval-commits
while read -r upstream_commit; do
  # For divergent fork history, apply only this upstream commit's delta. The
  # upstream parent is the three-way merge base; using "$cursor" here makes
  # fork-only files look deleted. Resolve any conflicts behaviorally, then
  # stage the resulting tree and create the signed two-parent merge directly.
  # `--write-tree` prints the tree hash first, then conflict diagnostics on
  # stdout when the merge is not clean. Capture only the first line; reading
  # that tree and checking it out materializes conflict markers for semantic
  # resolution without losing the staged merge result.
  merge_output=$(mktemp)
  git merge-tree --write-tree --merge-base "$upstream_commit^" HEAD "$upstream_commit" >"$merge_output" || true
  tree=$(sed -n '1p' "$merge_output")
  git cat-file -e "$tree^{tree}"
  git read-tree "$tree"
  git checkout-index -a -f
  rm -f "$merge_output"
  # Resolve any `<<<<<<<` markers behaviorally, then stage the result.
  # If shipped code changed, update scripts/apohl79_build_number.txt before
  # writing the tree and recompute $tree from the index.
  git diff --check
  git add -A
  tree=$(git write-tree)
  export GPG_TTY=$(tty)
  gpg-agent --daemon 2>/dev/null || true
  new_commit=$(printf '%s\n' "chore(upgrade): replay upstream $(git show -s --format=%h "$upstream_commit")" |
    git commit-tree -S "$tree" -p HEAD -p "$upstream_commit")
  test -n "$new_commit" || { echo "commit failed; stop the interval" >&2; exit 1; }
  git reset --soft "$new_commit"
  # Run just fmt from codex-rs whenever code changed, then repeat the
  # write-tree/commit-tree step so formatting and build-number bumps are in
  # the same signed merge commit.
done < /tmp/apohl79-interval-commits
```

When ordinary `git merge --no-ff --no-commit` is sufficient, it may be used
instead, but every imported upstream SHA must still be a separate signed merge
commit whose second parent is that exact SHA. Verify the interval with:

```bash
git log --merges --format='%H %P' "$interval_start..HEAD" |
  awk -v sha="$upstream_commit" '$3 == sha { found = 1 } END { exit !found }'
```

Review the subagent's result before continuing. Ensure each upstream commit has
one corresponding merge commit, `git diff --check` is clean, every touched fork
feature remains documented and working, and `upgrade-fork.md` records the
completed interval. Scan tracked source and test files for unresolved
`<<<<<<<`, `=======`, or `>>>>>>>` markers before accepting the interval; a
clean diff alone does not prove conflict markers were removed. Commit the report
update before testing so validation starts
from a clean worktree.

The interval audit must compare the ordered second-parent sequence, not only
unordered set membership. It must equal `git rev-list --reverse
"$cursor..$next_tag"` byte-for-byte, contain the endpoint exactly once, and
contain no later upstream SHA. After every merge, verify that its second parent
is the SHA just processed and that any required build-number bump is present in
that same merge. If a commit, tree write, index operation, or signing step
fails, stop immediately; preserve the last signed interval report and rebuild
only the bounded suffix so a failed iteration cannot create wrong-parent
commits or reorder the replay history.

## 6. Preflight disk usage and require complete checkpoint validation

After each completed interval, once the worktree is clean and no agent is
running a compiler, remove rebuildable development artifacts before spawning
the next interval. Before every checkpoint validation, repeat this cleanup,
verify `df -h .`, and do not start until the free-space budget is sufficient
for the complete validation suite. Long replay runs can otherwise exhaust the
local volume before tests begin.

```bash
(
  cd codex-rs
  before_kb=$(du -sk target | awk '{print $1}')
  cargo clean --profile dev
  # Remove these only when present; they contain rebuildable test output.
  find target/nextest target/tmp -depth -delete 2>/dev/null || true
  after_kb=$(du -sk target | awk '{print $1}')
  printf 'disk cleanup: %s KiB -> %s KiB\n' "$before_kb" "$after_kb"
)
(cd tools/argument-comment-lint && cargo clean)
bazel clean --expunge
df -h .
```

Never clean while an agent is compiling. The Cargo command above removes only
the development profile; preserve `codex-rs/target/release` and the
target-triple release directory used by fork packaging until the release
handoff is complete. `bazel clean --expunge` removes Bazel's rebuildable output
base and is required at every checkpoint because Bazel can retain more than
Cargo. Do not use `rm -rf target` or delete release directories. Record the
before/after sizes, cleanup result, and `df -h .` output in the interval review
even when Cargo reports `Removed 0 files`.

Run the complete checkpoint validation automatically—do not ask the user for
confirmation—when the newly reached endpoint is a scheduled checkpoint:
every tenth alpha release or any stable release. Do not run this suite for
replay-only alpha endpoints. Run it once more at the final target endpoint,
even if that endpoint is already a stable checkpoint. For every scheduled
checkpoint, invoke the command immediately after the endpoint merge and clean
interval-report commit; this validation is mandatory before advancing. Start
only after the interval report is committed, the worktree is clean, and the
disk preflight above passes.
Use at most two Cargo compilation jobs:

```bash
checkpoint=<next-tag>
log="/tmp/apohl79-full-validation-${checkpoint}.log"
# Run to completion before interpreting any output; a partial/live log is not a result.
CARGO_BUILD_JOBS=2 scripts/run-full-validation.sh >"$log" 2>&1
validation_status=$?
# Only after the process exits, inspect the complete captured log.
sed -n '1,240p' "$log"
```

Treat `scripts/run-full-validation.sh` as a blocking validation step. Do not
tail, parse, or validate its output while it is running, and do not record a
checkpoint result from a partial log or live terminal output. Wait for the
process to exit, capture its final `validation_status`, then inspect the
complete log and require every stage to be present before accepting the
checkpoint.

`scripts/run-full-validation.sh` must configure the host Rust target with the
verified Codex-built V8 artifact pair through `scripts/codex_package/v8.py`
before Cargo compile, clippy, or test stages. This is required for code-mode:
the runtime enables `v8_enable_sandbox`, and the upstream `rusty_v8` download
does not provide that archive for every host (including Apple Silicon macOS).
The runner must print both `RUSTY_V8_ARCHIVE` and
`RUSTY_V8_SRC_BINDING_PATH`; a missing pair is a validation failure, not a
reason to skip code-mode tests.

The validation runner must clean Cargo development artifacts at the boundary
between Cargo and Bazel work. It runs `cargo clean --profile dev` after Cargo
dependency lint and again after the full Cargo workspace tests, before Bazel
analysis or tests. These cleanups preserve release targets and prevent the
Cargo and Bazel output trees from exhausting the local volume while the full
suite is still running.

The runner defaults `BAZEL_BUILD_JOBS=2` for Bazel clippy and tests. Keep this
limit unless a larger disk and an explicitly measured workspace budget justify
an override; Bazel's default parallelism can exhaust the volume even when the
Cargo job limit is two.

The runner must pass the repository `ci` Bazel profile for local argument-comment
lint, Clippy, and test phases (`--config=ci`), with `--jobs=2` placed after the
config flags so the repo's default `--jobs=30` cannot override it. This disables
the persistent `~/.cache/bazel-disk-cache` and enables minimal output downloads,
avoiding a second multi-gigabyte cache beside Bazel's output base. If a prior
run populated that cache and disk space is constrained, remove only the verified
cache directory before restarting; never remove release outputs or interrupt an
active compiler.

Before the Cargo suite, the runner must build the host test support binaries
used by app-server integration tests: `codex`, `codex-code-mode-host`, and
`test_stdio_server`. The complete Cargo suite must run with a temporary clean
`HOME` so user shell startup files, credentials, and project settings cannot
change test behavior. Preserve the real `RUSTUP_HOME`, `CARGO_HOME`, and
DotSlash cache (`DOTSLASH_CACHE`) when creating that temporary home. Use
`just test --test-threads 1` with `CARGO_BUILD_JOBS=2`: this is still the full
workspace suite, while serial test processes avoid exec-server, zsh, and MCP
resource contention. Remove only the temporary test home after the suite
exits; never remove Cargo, Rustup, or DotSlash caches as part of this step.

Require `validation_status=0` and a complete log reaching every stage,
including support-binary builds, Cargo tests, Bazel tests, benchmark smoke,
release binary compile, and the clean-worktree gate. Validation is a blocking
defect-discovery gate, not a report-only milestone: a failed run means the
upgrade is incomplete. If validation fails, inspect the completed log, fix
every reported issue on the upgrade branch, format and commit the fixes, update
`README.fork.md` when a behavioral contract changed, and rerun the same
checkpoint from a clean report. Do not advance to the next alpha or stable
tag, call the upgrade complete, or record the checkpoint as complete until the
full suite passes.

If free space approaches the local safety floor or the validation runner emits
an interrupt while compiling, the checkpoint has failed; it is never an
accepted partial result. Preserve the complete log and exact phase/action
count, wait for all compiler children to exit, run the cleanup below, recheck
`df -h .`, and rerun the entire suite from the same checkpoint. Never advance,
run the release build, or mark the checkpoint complete after a disk stop.
Never delete release directories to make room and never kill an individual
compiler PID; the controlling validation process must own the stop.

After a successful complete checkpoint validation, repeat the same cleanup and
preserve the same release artifacts:

```bash
(
  cd codex-rs
  cargo clean --profile dev
  find target/nextest target/tmp -depth -delete 2>/dev/null || true
)
(cd tools/argument-comment-lint && cargo clean)
bazel clean --expunge
df -h .
git status --short --branch
```

After each successful complete validation run, append exactly
`passed (validation_status=0)` to the checkpoint row's Validation field,
without removing earlier successful-run markers. This preserves one marker per
successful invocation so the progress counter includes successful reruns of
the same checkpoint. Record the new cursor in `upgrade-fork.md` after the
cleanup. Leave the result pending or failed when the suite has not returned
zero. Do not run the complete validation suite for replay-only alpha
intervals.

## 7. Final audit and stopping condition

At the target stable release, after the final full validation returns zero,
build the fork release package locally before cleanup. The checkout must still
be the upgrade branch, and GitHub publishing remains disabled:

```bash
scripts/build_apohl79_release.sh \
  --skip-github-release \
  --cargo-build-jobs 2 \
  --ref "$upgrade_branch"
```

For the target in this skill, the concrete final command is, for example:

```bash
scripts/build_apohl79_release.sh --skip-github-release \
  --cargo-build-jobs 2 --ref upgrade-0.147.0
```

Require this release build to complete successfully. If it fails or is
interrupted, preserve its log, clean only after all compiler children exit, and
rerun the full final validation followed by the release build. Only after both
the final suite and release build pass, run the final cleanup and prove that
the branch contains all upstream commits, every inventory behavior has been
checked, and no accidental branch movement occurred:

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
If the upgrade branch is later discarded, delete its temporary
`upgrade-fork.md` report as part of that cleanup; it must not land on
`main-fork`.

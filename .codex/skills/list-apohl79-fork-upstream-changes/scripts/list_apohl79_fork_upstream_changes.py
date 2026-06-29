#!/usr/bin/env python3
"""Collect upstream stable-release changes missing from the apohl79 fork."""

import argparse
from dataclasses import dataclass
from datetime import datetime
from datetime import timezone
import json
from pathlib import Path
import re
import subprocess
import sys
from typing import Any

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11 fallback.
    tomllib = None


STABLE_TAG_RE = re.compile(r"^rust-v(\d+)\.(\d+)\.(\d+)$")
FORK_TAG_RE = re.compile(r"^(rust-v\d+\.\d+\.\d+)-apohl79$")
WORD_RE = re.compile(r"[a-z0-9][a-z0-9_-]{2,}")
STOP_WORDS = {
    "and",
    "are",
    "but",
    "can",
    "codex",
    "add",
    "added",
    "current",
    "default",
    "details",
    "file",
    "files",
    "fix",
    "fixes",
    "for",
    "fork",
    "from",
    "git",
    "including",
    "into",
    "latest",
    "list",
    "local",
    "main",
    "now",
    "openai",
    "path",
    "release",
    "releases",
    "repo",
    "script",
    "search",
    "selected",
    "stable",
    "tag",
    "tags",
    "that",
    "the",
    "this",
    "tui",
    "upstream",
    "use",
    "used",
    "uses",
    "when",
    "where",
    "while",
    "with",
}


@dataclass(frozen=True)
class CommandResult:
    stdout: str
    stderr: str
    returncode: int


@dataclass(frozen=True)
class ReleaseInfo:
    tag: str
    name: str | None = None
    published_at: str | None = None
    url: str | None = None
    body: str | None = None


@dataclass(frozen=True)
class CommitInfo:
    short_sha: str
    date: str
    subject: str


@dataclass(frozen=True)
class ForkInventoryItem:
    heading: str
    text: str
    tokens: frozenset[str]


@dataclass(frozen=True)
class OverlapMatch:
    fork_item: str
    upstream_item: str
    shared_tokens: tuple[str, ...]


def version_key(tag: str) -> tuple[int, int, int]:
    match = STABLE_TAG_RE.match(tag)
    if not match:
        raise ValueError(f"not a stable rust tag: {tag}")
    return tuple(int(part) for part in match.groups())


def is_stable_tag(tag: str) -> bool:
    return STABLE_TAG_RE.match(tag) is not None


def fork_tag_base(tag: str) -> str | None:
    match = FORK_TAG_RE.match(tag)
    if match:
        return match.group(1)
    return None


def run_command(args: list[str], cwd: Path, check: bool = True) -> CommandResult:
    try:
        proc = subprocess.run(
            args,
            cwd=cwd,
            text=True,
            capture_output=True,
            check=False,
        )
    except FileNotFoundError as exc:
        if check:
            raise RuntimeError(f"missing command: {args[0]}") from exc
        return CommandResult("", str(exc), 127)

    result = CommandResult(proc.stdout, proc.stderr, proc.returncode)
    if check and proc.returncode != 0:
        rendered = " ".join(args)
        raise RuntimeError(
            f"command failed ({proc.returncode}): {rendered}\n"
            f"stdout:\n{proc.stdout}\n"
            f"stderr:\n{proc.stderr}"
        )
    return result


def git(args: list[str], repo_root: Path, check: bool = True) -> str:
    return run_command(["git", *args], repo_root, check=check).stdout


def find_repo_root(start: Path) -> Path:
    output = run_command(["git", "rev-parse", "--show-toplevel"], start).stdout
    return Path(output.strip())


def verify_upstream_remote(repo_root: Path, remote: str) -> str:
    url = git(["config", "--get", f"remote.{remote}.url"], repo_root).strip()
    normalized = url.removesuffix(".git")
    if "openai/codex" not in normalized:
        raise RuntimeError(
            f"remote {remote!r} must point at openai/codex; found {url!r}"
        )
    return url


def stable_tags_from_lines(lines: list[str]) -> list[str]:
    tags = sorted({line.strip() for line in lines if is_stable_tag(line.strip())}, key=version_key)
    return tags


def local_stable_tags(repo_root: Path) -> list[str]:
    output = git(["tag", "--list", "rust-v*"], repo_root)
    return stable_tags_from_lines(output.splitlines())


def reachable_fork_base(repo_root: Path) -> tuple[str, str] | None:
    output = git(["tag", "--merged", "HEAD", "--list", "rust-v*-apohl79"], repo_root)
    candidates: list[tuple[tuple[int, int, int], str, str]] = []
    for tag in output.splitlines():
        base = fork_tag_base(tag.strip())
        if base is None:
            continue
        candidates.append((version_key(base), tag.strip(), base))
    if not candidates:
        return None
    _, fork_tag, base_tag = max(candidates, key=lambda item: item[0])
    return fork_tag, base_tag


def workspace_version_base(repo_root: Path) -> tuple[str, str] | None:
    cargo_toml = repo_root / "codex-rs" / "Cargo.toml"
    if not cargo_toml.exists() or tomllib is None:
        return None
    with cargo_toml.open("rb") as handle:
        data = tomllib.load(handle)
    version = (
        data.get("workspace", {})
        .get("package", {})
        .get("version")
    )
    if not isinstance(version, str):
        return None
    tag = f"rust-v{version}"
    if is_stable_tag(tag):
        return "codex-rs/Cargo.toml", tag
    return None


def normalize_from_tag(raw_tag: str) -> tuple[str, str]:
    base = fork_tag_base(raw_tag)
    if base is not None:
        return raw_tag, base
    if is_stable_tag(raw_tag):
        return raw_tag, raw_tag
    raise ValueError(
        "--from-tag must be rust-vX.Y.Z or rust-vX.Y.Z-apohl79, "
        f"got {raw_tag!r}"
    )


def detect_fork_base(repo_root: Path, from_tag: str | None) -> tuple[str, str]:
    if from_tag:
        return normalize_from_tag(from_tag)
    detected = reachable_fork_base(repo_root)
    if detected is not None:
        return detected
    detected = workspace_version_base(repo_root)
    if detected is not None:
        return detected
    raise RuntimeError(
        "could not detect fork base; pass --from-tag rust-vX.Y.Z-apohl79"
    )


def gh_release_list(repo_root: Path, gh_repo: str) -> list[dict[str, Any]]:
    result = run_command(
        [
            "gh",
            "release",
            "list",
            "-R",
            gh_repo,
            "--limit",
            "200",
            "--json",
            "tagName,name,isPrerelease,isDraft,publishedAt",
        ],
        repo_root,
        check=False,
    )
    if result.returncode != 0:
        return []
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError:
        return []
    if isinstance(data, list):
        return [item for item in data if isinstance(item, dict)]
    return []


def latest_stable_from_releases(releases: list[dict[str, Any]]) -> str | None:
    candidates = []
    for release in releases:
        tag = release.get("tagName")
        if not isinstance(tag, str) or not is_stable_tag(tag):
            continue
        if release.get("isDraft") or release.get("isPrerelease"):
            continue
        candidates.append(tag)
    if not candidates:
        return None
    return max(candidates, key=version_key)


def latest_stable_tag(
    stable_tags: list[str],
    releases: list[dict[str, Any]],
    explicit_to_tag: str | None,
) -> str:
    if explicit_to_tag:
        if not is_stable_tag(explicit_to_tag):
            raise ValueError(f"--to-tag must be a stable rust tag: {explicit_to_tag}")
        return explicit_to_tag
    from_releases = latest_stable_from_releases(releases)
    if from_releases is not None:
        return from_releases
    if not stable_tags:
        raise RuntimeError("no stable upstream rust tags found")
    return max(stable_tags, key=version_key)


def stable_steps(stable_tags: list[str], base_tag: str, target_tag: str) -> list[str]:
    base_version = version_key(base_tag)
    target_version = version_key(target_tag)
    if target_version < base_version:
        raise RuntimeError(f"target {target_tag} is older than base {base_tag}")
    return [
        tag
        for tag in stable_tags
        if base_version < version_key(tag) <= target_version
    ]


def release_info(repo_root: Path, gh_repo: str, tag: str) -> ReleaseInfo:
    result = run_command(
        [
            "gh",
            "release",
            "view",
            tag,
            "-R",
            gh_repo,
            "--json",
            "tagName,name,isPrerelease,isDraft,publishedAt,body,url",
        ],
        repo_root,
        check=False,
    )
    if result.returncode != 0:
        return ReleaseInfo(tag=tag)
    try:
        data = json.loads(result.stdout)
    except json.JSONDecodeError:
        return ReleaseInfo(tag=tag)
    if not isinstance(data, dict):
        return ReleaseInfo(tag=tag)
    return ReleaseInfo(
        tag=tag,
        name=data.get("name"),
        published_at=data.get("publishedAt"),
        url=data.get("url"),
        body=data.get("body"),
    )


def commits_between(repo_root: Path, start_tag: str, end_tag: str) -> list[CommitInfo]:
    output = git(
        [
            "log",
            "--reverse",
            "--date=short",
            "--format=%h%x09%ad%x09%s",
            f"{start_tag}..{end_tag}",
        ],
        repo_root,
    )
    commits = []
    for line in output.splitlines():
        parts = line.split("\t", 2)
        if len(parts) != 3:
            continue
        commits.append(CommitInfo(parts[0], parts[1], parts[2]))
    return commits


def diff_stat(repo_root: Path, start_tag: str, end_tag: str) -> str:
    return git(["diff", "--stat", f"{start_tag}..{end_tag}"], repo_root).rstrip()


def render_release_body(body: str | None) -> str:
    normalized = (body or "").strip()
    if not normalized:
        return "_No GitHub release notes were retrieved._"
    return normalized


def tokenize(text: str) -> frozenset[str]:
    return frozenset(
        word
        for word in WORD_RE.findall(text.lower())
        if word not in STOP_WORDS and not word.isdigit()
    )


def fork_inventory_items(readme: Path) -> list[ForkInventoryItem]:
    if not readme.exists():
        return []
    items: list[ForkInventoryItem] = []
    heading = ""
    for raw_line in readme.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line.startswith("### "):
            heading = line.removeprefix("### ").strip("`")
            tokens = tokenize(heading)
            if tokens:
                items.append(ForkInventoryItem(heading, heading, tokens))
            continue
        if not line.startswith("- "):
            continue
        text = line.removeprefix("- ").strip()
        if not text or text.startswith("`"):
            continue
        label = f"{heading}: {text}" if heading else text
        tokens = tokenize(label)
        if tokens:
            items.append(ForkInventoryItem(heading, label, tokens))
    return items


def overlap_matches(
    fork_items: list[ForkInventoryItem],
    release_infos: list[ReleaseInfo],
    commits: list[CommitInfo],
) -> list[OverlapMatch]:
    upstream_texts: list[str] = []
    upstream_texts.extend(commit.subject for commit in commits)
    for info in release_infos:
        if info.body:
            upstream_texts.extend(
                line.strip("- ").strip()
                for line in info.body.splitlines()
                if line.strip()
            )

    matches: list[OverlapMatch] = []
    seen: set[tuple[str, str]] = set()
    for fork_item in fork_items:
        for upstream_text in upstream_texts:
            upstream_tokens = tokenize(upstream_text)
            shared = tuple(sorted(fork_item.tokens & upstream_tokens))
            if len(shared) < 2:
                continue
            key = (fork_item.text, upstream_text)
            if key in seen:
                continue
            seen.add(key)
            matches.append(OverlapMatch(fork_item.text, upstream_text, shared))
    return matches


def render_report(
    repo_root: Path,
    fork_source: str,
    base_tag: str,
    target_tag: str,
    steps: list[str],
    gh_repo: str,
    fork_readme: Path,
) -> str:
    generated_at = datetime.now(timezone.utc).replace(microsecond=0).isoformat()
    lines = [
        "# Fork to Latest Stable Upstream Changes",
        "",
        f"Generated: `{generated_at}`",
        f"Repository: `{repo_root}`",
        f"Fork version source: `{fork_source}`",
        f"Fork upstream base: `{base_tag}`",
        f"Latest stable upstream: `{target_tag}`",
        f"Compared range: `{base_tag}..{target_tag}`",
        "",
    ]

    all_commits = commits_between(repo_root, base_tag, target_tag)
    all_release_infos: list[ReleaseInfo] = []
    lines.extend(
        [
            "## Summary",
            "",
            f"- Stable releases included: {len(steps)}",
            f"- Commits included: {len(all_commits)}",
            "- Alpha/prerelease tags excluded: yes",
            "- Fork-added features/fixes excluded from main changelog: yes",
            "",
        ]
    )

    if not steps:
        lines.extend(
            [
                "No newer stable upstream release was found after the fork base.",
                "",
            ]
        )
        return "\n".join(lines)

    previous = base_tag
    rendered_releases: list[tuple[str, ReleaseInfo, list[CommitInfo], str, str]] = []
    for tag in steps:
        info = release_info(repo_root, gh_repo, tag)
        all_release_infos.append(info)
        commits = commits_between(repo_root, previous, tag)
        stat = diff_stat(repo_root, previous, tag)
        rendered_releases.append((previous, info, commits, stat, tag))
        previous = tag

    fork_items = fork_inventory_items(fork_readme)
    matches = overlap_matches(fork_items, all_release_infos, all_commits)
    lines.extend(
        [
            "## Potential Upstream Overlaps With Fork Features",
            "",
        ]
    )
    if matches:
        lines.extend(
            [
                "These are heuristic matches between upstream release text/commit subjects",
                "and `README.fork.md`. Review them manually before deciding that an",
                "upstream change supersedes a fork feature.",
                "",
            ]
        )
        for match in matches[:20]:
            shared = ", ".join(match.shared_tokens)
            lines.append(
                f"- Fork item: {match.fork_item}\n"
                f"  Upstream item: {match.upstream_item}\n"
                f"  Shared tokens: {shared}"
            )
    else:
        lines.append(
            "No direct upstream/fork feature overlaps were detected from "
            "`README.fork.md` using commit subjects and release notes."
        )
    lines.append("")

    for previous, info, commits, stat, tag in rendered_releases:
        title = info.name or tag.removeprefix("rust-v")
        lines.extend(
            [
                f"## {title} (`{tag}`)",
                "",
                f"- Range: `{previous}..{tag}`",
                f"- Published: `{info.published_at or 'unknown'}`",
                f"- URL: {info.url or f'https://github.com/{gh_repo}/releases/tag/{tag}'}",
                f"- Commits: {len(commits)}",
                "",
                "### Release Notes",
                "",
                render_release_body(info.body),
                "",
                "### Commits",
                "",
            ]
        )
        if commits:
            for commit in commits:
                lines.append(f"- `{commit.short_sha}` {commit.date} {commit.subject}")
        else:
            lines.append("- No commits in this interval.")
        lines.extend(["", "### Changed Files", ""])
        if stat:
            lines.extend(["```text", stat, "```"])
        else:
            lines.append("_No changed-file stat was produced._")
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "List all stable upstream OpenAI Codex changes between the "
            "apohl79 fork version and latest non-alpha upstream release."
        )
    )
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--repo", default="openai/codex", help="GitHub repo")
    parser.add_argument("--upstream-remote", default="upstream")
    parser.add_argument("--from-tag")
    parser.add_argument("--to-tag")
    parser.add_argument("--no-fetch", action="store_true")
    parser.add_argument(
        "--fork-readme",
        default="README.fork.md",
        help="Fork feature/fix inventory used only for overlap detection.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    try:
        repo_root = find_repo_root(args.repo_root)
        verify_upstream_remote(repo_root, args.upstream_remote)
        if not args.no_fetch:
            git(["fetch", args.upstream_remote, "--tags", "--prune"], repo_root)

        stable_tags = local_stable_tags(repo_root)
        fork_source, base_tag = detect_fork_base(repo_root, args.from_tag)
        if base_tag not in stable_tags:
            raise RuntimeError(f"base tag {base_tag} is not available locally")

        releases = gh_release_list(repo_root, args.repo)
        target_tag = latest_stable_tag(stable_tags, releases, args.to_tag)
        if target_tag not in stable_tags:
            raise RuntimeError(f"target tag {target_tag} is not available locally")

        steps = stable_steps(stable_tags, base_tag, target_tag)
        print(
            render_report(
                repo_root=repo_root,
                fork_source=fork_source,
                base_tag=base_tag,
                target_tag=target_tag,
                steps=steps,
                gh_repo=args.repo,
                fork_readme=repo_root / args.fork_readme,
            ),
            end="",
        )
        return 0
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

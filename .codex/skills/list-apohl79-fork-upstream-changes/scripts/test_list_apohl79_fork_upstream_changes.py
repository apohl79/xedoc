#!/usr/bin/env python3

import importlib.util
from pathlib import Path
import unittest


SCRIPT = Path(__file__).with_name("list_apohl79_fork_upstream_changes.py")
SPEC = importlib.util.spec_from_file_location("collector", SCRIPT)
collector = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(collector)


class ForkUpstreamChangesTest(unittest.TestCase):
    def test_stable_tag_filter_excludes_alpha_and_fork_tags(self) -> None:
        self.assertEqual(
            collector.stable_tags_from_lines(
                [
                    "rust-v0.142.0-alpha.1",
                    "rust-v0.142.0",
                    "rust-v0.142.0-apohl79",
                    "rust-v0.142.1",
                    "codex-zsh-v0.1.0",
                ]
            ),
            ["rust-v0.142.0", "rust-v0.142.1"],
        )

    def test_latest_stable_release_ignores_prerelease_and_draft(self) -> None:
        self.assertEqual(
            collector.latest_stable_from_releases(
                [
                    {
                        "tagName": "rust-v0.143.0-alpha.1",
                        "isPrerelease": True,
                        "isDraft": False,
                    },
                    {
                        "tagName": "rust-v0.142.5",
                        "isPrerelease": False,
                        "isDraft": True,
                    },
                    {
                        "tagName": "rust-v0.142.4",
                        "isPrerelease": False,
                        "isDraft": False,
                    },
                    {
                        "tagName": "rust-v0.142.3",
                        "isPrerelease": False,
                        "isDraft": False,
                    },
                ]
            ),
            "rust-v0.142.4",
        )

    def test_fork_tag_base_strips_apohl79_suffix(self) -> None:
        self.assertEqual(
            collector.fork_tag_base("rust-v0.142.0-apohl79"),
            "rust-v0.142.0",
        )
        self.assertIsNone(collector.fork_tag_base("rust-v0.142.0-alpha.1-apohl79"))

    def test_stable_steps_selects_every_tag_after_base_through_target(self) -> None:
        self.assertEqual(
            collector.stable_steps(
                [
                    "rust-v0.141.0",
                    "rust-v0.142.0",
                    "rust-v0.142.1",
                    "rust-v0.142.2",
                    "rust-v0.142.3",
                ],
                "rust-v0.142.0",
                "rust-v0.142.3",
            ),
            ["rust-v0.142.1", "rust-v0.142.2", "rust-v0.142.3"],
        )

    def test_render_release_body_handles_missing_body(self) -> None:
        self.assertEqual(
            collector.render_release_body(None),
            "_No GitHub release notes were retrieved._",
        )

    def test_overlap_matches_uses_readme_inventory_separately(self) -> None:
        fork_items = [
            collector.ForkInventoryItem(
                "Custom TUI Status Line",
                "Custom TUI Status Line: status_line_command config",
                frozenset({"custom", "status_line_command", "config"}),
            )
        ]
        matches = collector.overlap_matches(
            fork_items,
            [],
            [
                collector.CommitInfo(
                    "abc123",
                    "2026-06-29",
                    "Add status_line_command config support upstream",
                )
            ],
        )

        self.assertEqual(len(matches), 1)
        self.assertEqual(matches[0].fork_item, fork_items[0].text)


if __name__ == "__main__":
    unittest.main()

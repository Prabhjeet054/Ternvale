#!/usr/bin/env python3
"""Unit tests for clippy-annotate.py (no cargo)."""

from __future__ import annotations

import json
import unittest

from clippy_annotate import encode_annotation, errors_from_messages


class AnnotateTests(unittest.TestCase):
    def test_primary_span_error(self) -> None:
        line = json.dumps(
            {
                "reason": "compiler-message",
                "message": {
                    "level": "error",
                    "rendered": "unused variable: `clippy_step_87_probe`\n",
                    "spans": [
                        {
                            "file_name": "ternvale-log/src/lib.rs",
                            "line_start": 12,
                            "column_start": 9,
                            "is_primary": True,
                        }
                    ],
                },
            }
        )
        errors = errors_from_messages([line])
        self.assertEqual(len(errors), 1)
        path, lineno, col, rendered = errors[0]
        self.assertEqual(path, "ternvale-log/src/lib.rs")
        self.assertEqual(lineno, 12)
        self.assertEqual(col, 9)
        self.assertIn("clippy_step_87_probe", rendered)

    def test_skips_warnings_and_junk(self) -> None:
        lines = [
            "not json",
            json.dumps({"reason": "build-finished", "success": False}),
            json.dumps(
                {
                    "reason": "compiler-message",
                    "message": {"level": "warning", "rendered": "meh", "spans": []},
                }
            ),
        ]
        self.assertEqual(errors_from_messages(lines), [])

    def test_annotation_escapes_newlines(self) -> None:
        self.assertIn("%0A", encode_annotation("a\nb"))


if __name__ == "__main__":
    unittest.main()

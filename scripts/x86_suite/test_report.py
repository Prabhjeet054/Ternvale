import unittest

from report import (
    expected_det_line,
    expected_det_sum,
    markdown_table,
    parse_engine_block,
    split_engines,
)


class DetSum(unittest.TestCase):
    def test_expected_line_is_stable(self):
        self.assertEqual(expected_det_sum(), expected_det_sum())
        self.assertTrue(expected_det_line().startswith("det v1 iters=100000 sum="))


class Parse(unittest.TestCase):
    SAMPLE = """
ENGINE=fex
det v1 iters=100000 sum=1
det_ns=10
DET_RC=0
litmus mp iters=2000000 forbidden=0 ns=100
LITMUS_MP_RC=0
litmus sb iters=2000000 both_zero=9 ns=90
LITMUS_SB_RC=0
x86_hello
ENGINE=qemu-user
ENGINE_SKIP
ENGINE=rosetta
det v1 iters=100000 sum=1
det_ns=20
DET_RC=0
litmus mp iters=2000000 forbidden=3 ns=50
LITMUS_MP_RC=1
"""

    def test_split_and_mp_violation(self):
        runs = split_engines(self.SAMPLE)
        self.assertEqual(set(runs), {"fex", "qemu-user", "rosetta"})
        self.assertTrue(runs["rosetta"].mp_forbidden == 3)
        self.assertFalse(runs["rosetta"].mp_ok())
        self.assertEqual(runs["fex"].sb_both_zero, 9)
        self.assertIn("engine binary missing", runs["qemu-user"].notes)

    def test_det_mismatch(self):
        run = parse_engine_block(
            "fex", "det v1 iters=100000 sum=0\nDET_RC=0\n"
        )
        self.assertFalse(run.det_ok())

    def test_det_match_uses_guest_formula(self):
        line = expected_det_line()
        run = parse_engine_block("fex", f"{line}\nDET_RC=0\n")
        self.assertTrue(run.det_ok())

    def test_table_marks_violation(self):
        runs = split_engines(self.SAMPLE)
        text = markdown_table(runs, ["rosetta", "fex", "qemu-user"])
        self.assertIn("VIOLATION", text)
        self.assertIn("qemu-user", text)


if __name__ == "__main__":
    unittest.main()

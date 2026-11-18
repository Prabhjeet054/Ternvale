"""Parse guest serial from the Step 99 cross-engine suite."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Optional

DET_ITERS = 100_000
DET_MUL = 1_664_525
DET_ADD = 1_013_904_223

LITMUS_MP = re.compile(
    r"litmus mp iters=(\d+) forbidden=(\d+) ns=(-?\d+)"
)
LITMUS_SB = re.compile(
    r"litmus sb iters=(\d+) both_zero=(\d+) ns=(-?\d+)"
)
DET_LINE = re.compile(r"det v1 iters=(\d+) sum=(\d+)")
DET_NS = re.compile(r"det_ns=(-?\d+)")
ENGINE = re.compile(r"^ENGINE=([a-z0-9-]+)\s*$", re.M)


def expected_det_sum(iters: int = DET_ITERS) -> int:
    """uint32 LCG used by guest-tests/x86-det.c."""
    total = 0
    for i in range(iters):
        total = (total * DET_MUL + DET_ADD + i) & 0xFFFFFFFF
    return total


def expected_det_line(iters: int = DET_ITERS) -> str:
    return f"det v1 iters={iters} sum={expected_det_sum(iters)}"


@dataclass
class EngineRun:
    name: str
    det_line: Optional[str] = None
    det_ns: Optional[int] = None
    det_rc: Optional[int] = None
    mp_forbidden: Optional[int] = None
    mp_iters: Optional[int] = None
    mp_ns: Optional[int] = None
    mp_rc: Optional[int] = None
    sb_both_zero: Optional[int] = None
    sb_iters: Optional[int] = None
    sb_ns: Optional[int] = None
    sb_rc: Optional[int] = None
    hello: bool = False
    notes: list[str] = field(default_factory=list)

    def det_ok(self) -> bool:
        return self.det_line == expected_det_line() and self.det_rc == 0

    def mp_ok(self) -> bool:
        return self.mp_forbidden == 0 and self.mp_rc == 0

    def ran(self) -> bool:
        return self.det_line is not None or self.mp_forbidden is not None


def _rc(text: str, key: str) -> Optional[int]:
    m = re.search(rf"^{key}_RC=(-?\d+)\s*$", text, re.M)
    return int(m.group(1)) if m else None


def parse_engine_block(name: str, block: str) -> EngineRun:
    run = EngineRun(name=name)
    dm = DET_LINE.search(block)
    if dm:
        run.det_line = dm.group(0)
    ns = DET_NS.search(block)
    if ns:
        run.det_ns = int(ns.group(1))
    run.det_rc = _rc(block, "DET")
    mp = LITMUS_MP.search(block)
    if mp:
        run.mp_iters = int(mp.group(1))
        run.mp_forbidden = int(mp.group(2))
        run.mp_ns = int(mp.group(3))
    run.mp_rc = _rc(block, "LITMUS_MP")
    sb = LITMUS_SB.search(block)
    if sb:
        run.sb_iters = int(sb.group(1))
        run.sb_both_zero = int(sb.group(2))
        run.sb_ns = int(sb.group(3))
    run.sb_rc = _rc(block, "LITMUS_SB")
    run.hello = "x86_hello" in block
    if "ENGINE_SKIP" in block:
        run.notes.append("engine binary missing")
    return run


def split_engines(serial: str) -> dict[str, EngineRun]:
    found: dict[str, EngineRun] = {}
    matches = list(ENGINE.finditer(serial))
    for i, m in enumerate(matches):
        name = m.group(1)
        start = m.end()
        end = matches[i + 1].start() if i + 1 < len(matches) else len(serial)
        found[name] = parse_engine_block(name, serial[start:end])
    return found


def rel_speed(ns: Optional[int], baseline: Optional[int]) -> str:
    if ns is None or baseline is None or baseline == 0:
        return "n/a"
    return f"{ns / baseline:.2f}x"


def markdown_table(runs: dict[str, EngineRun], order: list[str]) -> str:
    want = expected_det_line()
    rows = [
        "| Engine | det identical | MP forbidden (TSO) | SB both_zero (TSO-allowed) | det_ns | MP ns | vs FEX det |",
        "| --- | --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    fex = runs.get("fex")
    base = fex.det_ns if fex else None
    for name in order:
        run = runs.get(name)
        if run is None or not run.ran():
            rows.append(
                f"| {name} | n/a | n/a | n/a | n/a | n/a | n/a |"
            )
            continue
        det = "pass" if run.det_ok() else "FAIL"
        if run.det_line and run.det_line != want:
            det = f"FAIL (got `{run.det_line}`)"
        mp = (
            str(run.mp_forbidden)
            if run.mp_forbidden is not None
            else "n/a"
        )
        if run.mp_forbidden is not None:
            mp = f"{run.mp_forbidden} ({'ok' if run.mp_ok() else 'VIOLATION'})"
        sb = (
            str(run.sb_both_zero) if run.sb_both_zero is not None else "n/a"
        )
        rows.append(
            "| {name} | {det} | {mp} | {sb} | {det_ns} | {mp_ns} | {rel} |".format(
                name=name,
                det=det,
                mp=mp,
                sb=sb,
                det_ns=run.det_ns if run.det_ns is not None else "n/a",
                mp_ns=run.mp_ns if run.mp_ns is not None else "n/a",
                rel=rel_speed(run.det_ns, base),
            )
        )
    return "\n".join(rows)


def render_compat(
    step98: str,
    stamp: str,
    native_engine: str,
    vz_engine: str,
    runs: dict[str, EngineRun],
    artifacts: str,
    skips: list[str],
) -> str:
    order = ["rosetta", "fex", "qemu-user"]
    table = markdown_table(runs, order)
    skip_rows = "\n".join(skips) if skips else "| — | — | none |"
    body_99 = f"""
## Step 99: cross-engine suite (det + TSO litmus)

Host date in artifacts: `{stamp}`.
Native status `x86_engine` during FEX/qemu run: **{native_engine}**.
VZ status `x86_engine`: **{vz_engine}**.

Deterministic stdout must match `{expected_det_line()}` on every engine.
Message-passing has **no fences**; any `forbidden>0` is an x86 TSO break.
Store-buffering has **no mfence**; `both_zero` is allowed on real x86.

{table}

Lower `det_ns` / MP ns is faster. Relative column is vs FEX det on this host
(not SPECint). qemu-user is expected to be the slowest. `SB both_zero=0` is
not a failure: TCG often looks closer to sequential consistency than TSO.

### Skips / failures

| Item | Outcome | Why |
| --- | --- | --- |
{skip_rows}

Artifacts: `{artifacts}`.
"""
    base = step98.strip()
    marker = "## Step 99: cross-engine suite"
    if marker in base:
        base = base.split(marker)[0].rstrip()
    return base + "\n" + body_99

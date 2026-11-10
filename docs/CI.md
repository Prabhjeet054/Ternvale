# Continuous integration

Ternvale is `aarch64-apple-darwin` only. Guests need Hypervisor.framework.
GitHub-hosted macOS runners are nested VMs and do **not** expose HVF, so
`needs-hv` tests, the boot harness, and guest benches run on a dedicated
self-hosted Apple Silicon Mac.

## Workflows

| Workflow | File | When | Runner |
|---|---|---|---|
| PR | `.github/workflows/pr.yml` | every pull request and `main` push | GitHub-hosted `macos-15` (arm64) |
| Hypervisor | `.github/workflows/hypervisor.yml` | `main`, same-repo PRs, `workflow_dispatch` | self-hosted `[self-hosted, macOS, ARM64, ternvale]` |
| Nightly | `.github/workflows/nightly.yml` | 06:17 UTC and `workflow_dispatch` | same self-hosted runner |
| Release | `.github/workflows/release.yml` | tags `v*` | same self-hosted runner |

Docs-only PRs (no `*.rs` / Cargo / Makefile / `scripts/` / `.github/` changes)
skip the heavy PR jobs.

On failure, jobs upload `ci-artifacts`: `~/Library/Logs/Ternvale`, guest serial
logs, `target/{control,boot,regression,bench}-logs`, fuzz crashers, and crash
report zips (`scripts/ci/collect-artifacts.sh`).

## GitHub-hosted macOS and Hypervisor.framework

**Result (verified 2026-11-10 against current public runner-image reports): GitHub-hosted macOS arm64 cannot run Ternvale guests.**

Evidence:

- GitHub-hosted `macos-13`/`macos-14` arm64: `sysctl kern.hv_support` is `unknown oid`; QEMU `-accel hvf` returns `HV_UNSUPPORTED` ([actions/runner-images#9460](https://github.com/actions/runner-images/issues/9460)). Staff: architectural, nested virtualization not available on those images.
- Apple silicon nested virt (`hv_vm_config_set_el2_enabled`) exists on macOS 15+, but GitHub-hosted `macos-15` and `macos-26` arm64 still fail `kern.hv_support`. Staff confirmed **2026-01-08** that the limitation is the hosted VM’s hardware/hypervisor, not something the image can enable ([actions/runner-images#13505](https://github.com/actions/runner-images/issues/13505)).
- Nested virtualization is not a supported feature of GitHub-hosted macOS images ([docs](https://docs.github.com/en/actions/using-github-hosted-runners/about-github-hosted-runners); [runner-images#13277](https://github.com/actions/runner-images/issues/13277)).

The PR workflow still runs `scripts/ci/hv-probe.sh` and writes `kern.hv_support`
to the job summary so a future image change is visible. Do not move `needs-hv`
onto `macos-15` until that probe prints `1`.

PR jobs use `/usr/bin/env` as the cargo test runner so unit tests are not
ad-hoc codesigned (local AMFI SIGKILL with `scripts/sign-and-run.sh`).

## PR job (hosted)

1. `cargo fmt --check`
2. `cargo clippy --workspace -- -D warnings`
3. `scripts/audit-unsafe.sh`
4. `cargo deny check` (`deny.toml`)
5. `cargo audit` (`.cargo/audit.toml`)
6. `scripts/acpi-check.sh` (`iasl` from Homebrew `acpica` when brew succeeds; missing iasl is SKIP unless `TERNVALE_REQUIRE_IASL=1`)
7. `cargo test --workspace` (ignored tests not run)
8. `make fuzz-smoke` (nightly + `cargo-fuzz`, 30s per target)

Local stand-in: `make ci-pr` (no fuzz-smoke).

## Hypervisor job (self-hosted)

`make test-hv`, `scripts/regression.sh` (default scenarios `initrd rootfs`;
override `TERNVALE_REGRESSION_SCENARIOS`), then `make bench` when
`test-assets/bench` exists, else `make bench-compare-test`.

**Fork pull requests never run on this runner.** The job `if:` requires
`pull_request.head.repo.full_name == github.repository`. A fork PR only gets
the hosted “skip fork PRs” notice.

## Nightly

- `make fuzz-run` with `FUZZ_TIME=600` (10 minutes per target; dispatch with
  `3600` for `make fuzz-long`)
- `make bench` and `scripts/bench/compare.py --threshold 5` against the previous
  JSON in `bench-results/`
- `scripts/ci/compat-nightly.sh` → `make compat` (guest matrix → `docs/COMPAT.md`;
  missing images are skip, not fail)

## Release

Tag `v*` (or `workflow_dispatch`) runs `scripts/release.sh`. Secrets (never
printed):

| Secret | Use |
|---|---|
| `TERNVALE_CODESIGN_IDENTITY` | `Developer ID Application: …` (optional if Keychain already has it) |
| `TERNVALE_TEAM_ID` | Apple team id |
| `TERNVALE_NOTARY_KEY_ID` | App Store Connect API key id |
| `TERNVALE_NOTARY_ISSUER` | Issuer UUID |
| `TERNVALE_NOTARY_KEY` | `AuthKey_*.p8` **contents** (written to `$RUNNER_TEMP` with mode 600, deleted after) |

`TERNVALE_NOTARY_KEY_PATH` is set by the workflow; do not store the `.p8` in the
repo. See `docs/RELEASE.md`.

## Self-hosted runner setup

Use a **dedicated** Apple Silicon Mac (or a dedicated user account), not a
developer laptop. The runner process can execute whatever a workflow checks out.

1. Create a locked-down user, e.g. `ternvale-ci`. FileVault on. SSH off the
   public internet. SIP left enabled.
2. Install Xcode CLT, rustup (stable + nightly), `cargo-fuzz`, Docker Desktop
   if bench/rootfs images are built on the box, and the hypervisor entitlement
   codesign identity used by `scripts/sign-and-run.sh`.
3. Download the GitHub Actions runner for osx-arm64 into that user’s home,
   **not** `/`. Register it against **this repository only** (or a runner group
   limited to Ternvale). Labels: `self-hosted`, `macOS`, `ARM64`, `ternvale`.
4. In the repo: Settings → Actions → Runner groups: allow only this repo.
   Disable “Allow public forks to run workflows on self-hosted runners”
   (GitHub org/repo Actions settings).
5. Start the runner as that user (`./svc.sh install` / `start` under
   `ternvale-ci`, not as root).
6. Keep the machine otherwise idle: one VM at a time matches
   Hypervisor.framework’s process model (`make test-hv` uses `--test-threads=1`).

### Security rules

- **Trusted branches only.** The hypervisor and nightly workflows do not use
  `pull_request_target` (that would run fork code with secrets on the Mac).
  Fork PRs are skipped. Review and merge to `main` (or push a same-repo branch)
  before HV jobs run.
- **No secrets on hosted HV.** Release secrets live in GitHub encrypted
  secrets; the `.p8` is only materialized under `$RUNNER_TEMP` for the job.
- **Isolate.** Do not share the runner with other repos. Do not log in as
  `ternvale-ci` for interactive work. Wipe `~/Library/Logs/Ternvale` and
  `target/` on a schedule if disk fills.
- **Updates.** Pin runner version upgrades; `git` of untrusted workflow YAML
  from a fork must never execute here (already skipped).

If the runner is offline, hypervisor/nightly/release jobs queue until it
comes back. That is preferred to silently running guests on hosted VMs.

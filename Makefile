# Ternvale build targets. Run from the workspace root.
CARGO ?= cargo
ENTITLEMENTS := entitlements/ternvale.entitlements

.PHONY: build test test-hv lint ci-pr sign clean guest-tests viewer bench bench-image bench-compare-test fuzz-smoke fuzz-long fuzz-run audit-unsafe miri sanitize-stress loom compat compat-test

build:
	$(CARGO) build --workspace

# Dev viewer for `ternvale run --display window` (Swift/AppKit, needs swiftc).
viewer:
	./scripts/build-viewer.sh

test:
	$(CARGO) test --workspace

# Ignored tests are the ones marked needs-hv. They must run one at a time:
# Hypervisor.framework allows one VM per process, and the tests swap
# TERNVALE_LOG and the thread's log subscriber.
test-hv:
	$(CARGO) test --workspace -- --ignored --test-threads=1

# acpi-check.sh always checks table headers, checksums and pointers. Its iasl
# step is skipped (with a SKIP line, exit 0) when iasl is not installed, and
# fails lint when iasl is installed and reports a problem.
lint:
	$(CARGO) fmt --check
	$(CARGO) clippy --workspace -- -D warnings
	./scripts/acpi-check.sh
	./scripts/audit-unsafe.sh

audit-unsafe:
	./scripts/audit-unsafe.sh

# Hosted PR job without fuzz-smoke / cargo-fuzz (see docs/CI.md).
ci-pr:
	$(CARGO) fmt --check
	$(CARGO) clippy --workspace -- -D warnings
	./scripts/audit-unsafe.sh
	./scripts/acpi-check.sh
	@if $(CARGO) deny --version >/dev/null 2>&1; then $(CARGO) deny check; else echo "ci-pr: cargo-deny not installed; skip"; fi
	@if $(CARGO) audit --version >/dev/null 2>&1; then $(CARGO) audit; else echo "ci-pr: cargo-audit not installed; skip"; fi
	$(CARGO) test --workspace --config 'target.aarch64-apple-darwin.runner = "/usr/bin/env"' -- --test-threads=1

miri:
	./scripts/miri.sh

sanitize-stress:
	./scripts/sanitize-stress.sh

loom:
	./scripts/loom.sh

# Ad-hoc sign Mach-O binaries already built under target/debug.
sign:
	@set -euo pipefail; \
	found=0; \
	if [[ ! -d target/debug ]]; then \
		echo "sign: target/debug is missing; run make build first" >&2; \
		exit 1; \
	fi; \
	while IFS= read -r bin; do \
		if file "$$bin" | grep -q 'Mach-O'; then \
			echo "sign: $$bin" >&2; \
			codesign --sign - --force --entitlements $(ENTITLEMENTS) "$$bin"; \
			found=1; \
		fi; \
	done < <(find target/debug -type f -perm +111 ! -name '*.d'); \
	if [[ "$$found" -eq 0 ]]; then \
		echo "sign: no Mach-O binaries under target/debug" >&2; \
		exit 1; \
	fi

# AArch64 bare-metal payload: clang --target=aarch64-none-elf -c, then llvm-objcopy -O binary.
guest-tests:
	$(MAKE) -C guest-tests

# Step 62: Alpine sysbench/fio/iperf3/STREAM guest vs QEMU/HVF.
# Image: ./scripts/bench/make-image.sh (Docker linux/arm64). Results: bench-results/.
bench:
	python3 scripts/bench/run.py

bench-image:
	./scripts/bench/make-image.sh

bench-compare-test:
	cd scripts/bench && python3 test_compare.py

# Step 88: guest matrix → docs/COMPAT.md (skip missing images; --auto boots when HV is on).
compat:
	python3 scripts/compat/run.py --auto

compat-test:
	cd scripts/compat && python3 -m unittest discover -p 'test_*.py' -v

# Step 79: libFuzzer smoke (30s per target). Needs nightly + `cargo install cargo-fuzz`.
FUZZ_CRATE := ternvale-fuzz
NIGHTLY ?= nightly
FUZZ_TIME ?= 30
FUZZ_TARGETS := virtqueue virtio_blk virtio_net virtio_rng virtio_vsock virtio_gpu \
	virtio_input virtio_balloon virtio_fs virtio_pci xhci_trb tpm_crb vsock_packet \
	agent_proto esr psci user_net fuse

fuzz-smoke:
	$(MAKE) fuzz-run FUZZ_TIME=30

# Long campaign (default 1 hour per target). Override with `make fuzz-long FUZZ_TIME=600`.
fuzz-long:
	$(MAKE) fuzz-run FUZZ_TIME=3600

fuzz-run:
	@if ! $(CARGO) +$(NIGHTLY) fuzz --help >/dev/null 2>&1; then \
		echo "fuzz-run: need rustup nightly and cargo-fuzz (\`cargo install cargo-fuzz\`)" >&2; \
		exit 1; \
	fi
	@cd $(FUZZ_CRATE) && for t in $(FUZZ_TARGETS); do \
		echo "==> fuzz $$t ($(FUZZ_TIME)s)"; \
		$(CARGO) +$(NIGHTLY) fuzz run $$t -- -max_total_time=$(FUZZ_TIME) -timeout=5 || exit 1; \
	done

clean:
	$(CARGO) clean
	$(MAKE) -C guest-tests clean

# Ternvale build targets. Run from the workspace root.
CARGO ?= cargo
ENTITLEMENTS := entitlements/ternvale.entitlements

.PHONY: build test test-hv lint sign clean guest-tests

build:
	$(CARGO) build --workspace

test:
	$(CARGO) test --workspace

# Ignored tests are the ones marked needs-hv.
test-hv:
	$(CARGO) test --workspace -- --ignored

lint:
	$(CARGO) fmt --check
	$(CARGO) clippy --workspace -- -D warnings

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

clean:
	$(CARGO) clean
	$(MAKE) -C guest-tests clean

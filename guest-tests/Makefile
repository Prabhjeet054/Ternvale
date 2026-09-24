# Bare-metal AArch64 guest payloads. Run from guest-tests/ or `make -C guest-tests`.
# Apple ld rejects aarch64-none-elf, so clang only assembles. The string lives in
# .text, so `adr` needs no relocation and llvm-objcopy emits a raw image.
LLVM_BIN := /opt/homebrew/opt/llvm/bin
CLANG ?= clang
OBJCOPY ?= $(LLVM_BIN)/llvm-objcopy

.PHONY: all clean

all: hello.bin

hello.o: hello.S
	$(CLANG) --target=aarch64-none-elf -c -o $@ hello.S

hello.bin: hello.o
	$(OBJCOPY) -O binary hello.o $@

clean:
	rm -f hello.o hello.bin

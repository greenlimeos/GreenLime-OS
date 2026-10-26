# GreenLime OS — WSL Makefile (tabs required)
# Usage (inside Ubuntu WSL): make run

# Toolchain selection is governed by rust-toolchain.toml (an exact, dated
# nightly pin installed by scripts/setup-wsl.sh). Do NOT write '+nightly' here:
# a channel override would bypass the pin and grab today's nightly instead.
# Build cache lives outside the project tree so the source folder stays
# lean (~550 MB otherwise). Override per-invocation if needed:
#   make CARGO_TARGET_DIR=/path/to/dir run-release
CARGO_TARGET_DIR ?= $(HOME)/.cache/greenlime-target
export CARGO_TARGET_DIR
KERNEL_ELF := $(CARGO_TARGET_DIR)/x86_64-unknown-none/debug/kernel.elf
KERNEL_ELF_RELEASE := $(CARGO_TARGET_DIR)/x86_64-unknown-none/release/kernel.elf
INIT_ELF := $(CARGO_TARGET_DIR)/x86_64-unknown-none/debug/greenlime-init
ISO := dist/GreenLime.iso
ISO_RELEASE := dist/GreenLime-release.iso
LIMINE_DIR ?= $(HOME)/limine

# Userland gets real SSE/SSE2 (M13) — and since M15.1 via its own target
# spec, targets/x86_64-greenlime-sse.json: SSE/SSE2 are baked INTO the
# target (hard-float SysV, no `rustc-abi: softfloat`), so there is nothing
# left to fight from the CLI. The old env flags (`-Ctarget-feature=
# +sse,+sse2,-soft-float` on the stock soft-float target) tripped the
# rust#162235 ABI-mismatch warnings on EVERY userland crate and put core/
# zune-png/simd-adler32 on the future-incompat list (rust#117938: SSE is
# illegal on a soft-float target). The kernel keeps the stock soft-float
# target (x86_64-unknown-none) BY DESIGN: IRQ handlers can never touch
# XMM — the M13 trust shape. RUSTFLAGS env overrides .cargo/config
# rustflags entirely — hence relocation-model here.
UE_TARGET = targets/x86_64-greenlime-sse.json
# Cargo names a JSON-spec target's build dir after the FILE STEM, so the
# userland artifacts sit in <CARGO_TARGET_DIR>/$(UE_DIR)/<profile>/ — the
# kernel build.rs reads GREENLIME_UE_DIR to embed the right userland ELFs.
UE_DIR := $(basename $(notdir $(UE_TARGET)))
UE_RUSTFLAGS = -C relocation-model=static

.PHONY: build iso run run-uefi run-nographic debug diag clean check build-release iso-release run-release disk setup

setup:
	bash scripts/setup-wsl.sh

build:
	RUSTFLAGS="$(UE_RUSTFLAGS)" cargo build -p greenlime-init -p greenlime-shell -p greenlime-demo -p greenlime-fuzz -p greenlime-wallpaper --target $(UE_TARGET) -Zunstable-options -Zjson-target-spec
	GREENLIME_UE_DIR=$(UE_DIR) cargo build -p greenlime-kernel --target x86_64-unknown-none
	cp $(CARGO_TARGET_DIR)/x86_64-unknown-none/debug/greenlime-kernel $(KERNEL_ELF)

build-release:
	RUSTFLAGS="$(UE_RUSTFLAGS)" cargo build --release -p greenlime-init -p greenlime-shell -p greenlime-demo -p greenlime-fuzz -p greenlime-wallpaper --target $(UE_TARGET) -Zunstable-options -Zjson-target-spec
	GREENLIME_UE_DIR=$(UE_DIR) cargo build --release -p greenlime-kernel --target x86_64-unknown-none
	cp $(CARGO_TARGET_DIR)/x86_64-unknown-none/release/greenlime-kernel $(KERNEL_ELF_RELEASE)

iso-release: build-release
	mkdir -p iso_root/boot dist
	cp $(KERNEL_ELF_RELEASE) iso_root/boot/kernel.elf
	cp bootloader/limine.conf iso_root/boot/limine.conf
	cp bootloader/limine.conf iso_root/limine.conf
	cp $(LIMINE_DIR)/limine-bios.sys $(LIMINE_DIR)/limine-bios-cd.bin $(LIMINE_DIR)/limine-uefi-cd.bin iso_root/boot/ 2>/dev/null || (echo "Run scripts/setup-wsl.sh first (limine missing)"; exit 1)
	mkdir -p iso_root/EFI/BOOT
	cp $(LIMINE_DIR)/BOOTX64.EFI iso_root/EFI/BOOT/ 2>/dev/null || true
	xorriso -as mkisofs -b boot/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
	  --efi-boot boot/limine-uefi-cd.bin -efi-boot-part --efi-boot-image --protective-msdos-label \
	  iso_root -o $(ISO_RELEASE)
	$(LIMINE_DIR)/limine bios-install $(ISO_RELEASE)

run-release: iso-release
	bash scripts/run-qemu.sh bios $(ISO_RELEASE)

check:
	RUSTFLAGS="$(UE_RUSTFLAGS)" cargo build -p greenlime-init -p greenlime-shell -p greenlime-demo -p greenlime-fuzz -p greenlime-wallpaper --target $(UE_TARGET) -Zunstable-options -Zjson-target-spec
	GREENLIME_UE_DIR=$(UE_DIR) cargo check -p greenlime-kernel --target x86_64-unknown-none
	GREENLIME_UE_DIR=$(UE_DIR) cargo check -p greenlime-abi -p greenlime-init -p greenlime-shell -p greenlime-demo -p greenlime-fuzz -p greenlime-wallpaper -p greenlime-font

iso: build
	mkdir -p iso_root/boot dist
	cp $(KERNEL_ELF) iso_root/boot/kernel.elf
	cp bootloader/limine.conf iso_root/boot/limine.conf
	cp bootloader/limine.conf iso_root/limine.conf
	cp $(LIMINE_DIR)/limine-bios.sys $(LIMINE_DIR)/limine-bios-cd.bin $(LIMINE_DIR)/limine-uefi-cd.bin iso_root/boot/ 2>/dev/null || (echo "Run scripts/setup-wsl.sh first (limine missing)"; exit 1)
	mkdir -p iso_root/EFI/BOOT
	cp $(LIMINE_DIR)/BOOTX64.EFI iso_root/EFI/BOOT/ 2>/dev/null || true
	xorriso -as mkisofs -b boot/limine-bios-cd.bin -no-emul-boot -boot-load-size 4 -boot-info-table \
	  --efi-boot boot/limine-uefi-cd.bin -efi-boot-part --efi-boot-image --protective-msdos-label \
	  iso_root -o $(ISO)
	$(LIMINE_DIR)/limine bios-install $(ISO)

run: iso
	bash scripts/run-qemu.sh bios

run-uefi: iso
	bash scripts/run-qemu.sh uefi

run-nographic: iso
	bash scripts/run-qemu.sh nographic

debug: iso
	bash scripts/run-qemu.sh debug

diag:
	bash scripts/collect-logs.sh

disk:
	bash scripts/make-disk.sh

clean:
	rm -rf $(CARGO_TARGET_DIR) iso_root dist
	cargo clean || true

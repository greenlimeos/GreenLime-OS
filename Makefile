# GreenLime OS — WSL Makefile (tabs required)
# Usage (inside Ubuntu WSL): make run

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

.PHONY: build iso run run-uefi run-nographic debug diag clean check build-release iso-release run-release disk

build:
	cargo +nightly build -p greenlime-init -p greenlime-shell -p greenlime-demo -p greenlime-fuzz -p greenlime-wallpaper --target x86_64-unknown-none
	cargo +nightly build -p greenlime-kernel --target x86_64-unknown-none
	cp $(CARGO_TARGET_DIR)/x86_64-unknown-none/debug/greenlime-kernel $(KERNEL_ELF)

build-release:
	cargo +nightly build --release -p greenlime-init -p greenlime-shell -p greenlime-demo -p greenlime-fuzz -p greenlime-wallpaper --target x86_64-unknown-none
	cargo +nightly build --release -p greenlime-kernel --target x86_64-unknown-none
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
	cargo +nightly build -p greenlime-init -p greenlime-shell -p greenlime-demo -p greenlime-fuzz -p greenlime-wallpaper --target x86_64-unknown-none
	cargo +nightly check -p greenlime-kernel --target x86_64-unknown-none
	cargo check -p greenlime-abi -p greenlime-init -p greenlime-shell -p greenlime-demo -p greenlime-fuzz -p greenlime-wallpaper -p greenlime-font

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

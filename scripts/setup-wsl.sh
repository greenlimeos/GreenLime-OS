#!/usr/bin/env bash
# GreenLime OS — one-time WSL setup. Run inside Ubuntu WSL.
set -euo pipefail

echo "[setup] Updating apt..."
sudo apt update
sudo apt install -y \
  build-essential git curl xorriso mtools \
  qemu-system-x86 ovmf gdb-multiarch \
  parted gdisk python3

if ! command -v rustup >/dev/null 2>&1; then
  echo "[setup] Installing rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  source "$HOME/.cargo/env"
fi

echo "[setup] Installing nightly + x86_64-unknown-none..."
rustup toolchain install nightly --profile minimal -c rust-src -c llvm-tools-preview
rustup target add x86_64-unknown-none --toolchain nightly || true
rustup component add rust-src llvm-tools-preview --toolchain nightly || true

echo "[setup] Fetching Limine bootloader (v12.9.3, latest)..."
LIMINE_DIR="$HOME/limine"
LIMINE_VER="v12.9.3"
rm -rf "$LIMINE_DIR"
mkdir -p "$LIMINE_DIR"
curl -L "https://github.com/Limine-Bootloader/Limine/releases/download/${LIMINE_VER}/limine-binary.tar.gz" \
  | tar xz -C "$LIMINE_DIR"
# binary tar extracts to limine-binary/ subdir — flatten to $LIMINE_DIR
if [ -d "$LIMINE_DIR/limine-binary" ]; then
  cp -rf "$LIMINE_DIR/limine-binary"/. "$LIMINE_DIR"/
fi
# Build the `limine` host tool (bios-install) from bundled limine.c
(cd "$LIMINE_DIR" && make 2>&1 | tail -5 || true)
chmod +x "$LIMINE_DIR/limine" 2>/dev/null || true
ls -l "$LIMINE_DIR/limine" "$LIMINE_DIR/limine-bios.sys" 2>&1 | head -5
echo "[setup] Limine at $LIMINE_DIR/limine ($LIMINE_VER)"

echo "[setup] Done. Run: make run"

#!/usr/bin/env bash
# GreenLime OS — one-time WSL setup. Run inside Ubuntu WSL:
#   bash scripts/setup-wsl.sh
# (works from the repo root or any subdir — it cds itself) — then:
#   make run-release
#
# Installs EVERYTHING in one shot:
#   * apt deps (build-essential, QEMU, xorriso, mtools, python3, gdb, ovmf)
#   * rustup + the EXACT pinned Rust toolchain, read from rust-toolchain.toml
#     (nightly-2026-10-25 == 1.101.0-nightly) — NEVER "whatever nightly is
#     newest today", so every machine builds with the identical compiler
#   * Limine bootloader v12.9.4
# Re-runs are cheap: anything already present is skipped after a check,
# and the installed compiler is verified bit-for-bit against the pin.
set -euo pipefail

cd "$(dirname "$0")/.."   # repo root, so relative paths work from anywhere

# --- the toolchain pin (single source of truth: rust-toolchain.toml) ---
RUST_CHANNEL="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml | head -n1)"
[ -n "$RUST_CHANNEL" ] || {
  echo "[setup] FATAL: could not read 'channel' from rust-toolchain.toml"; exit 1;
}
# Sanity stamp the pin must produce — commit-exact, not just the version string
# (adjacent nightly dates can share a version number but differ by commits).
# Update this line TOGETHER with 'channel' above when bumping the pin.
RUST_EXPECT='1.101.0-nightly (12433c307 2026-10-24)'

echo "[setup] Pinned toolchain: ${RUST_CHANNEL}  (== rustc ${RUST_EXPECT})"

echo "[setup] Updating apt..."
sudo apt update
sudo apt install -y \
  build-essential git curl xorriso mtools \
  qemu-system-x86 ovmf gdb-multiarch \
  parted gdisk python3

if ! command -v rustup >/dev/null 2>&1; then
  echo "[setup] Installing rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
source "$HOME/.cargo/env" 2>/dev/null || true

if ! rustup toolchain list 2>/dev/null | grep -q "^${RUST_CHANNEL}-"; then
  echo "[setup] Installing EXACT toolchain ${RUST_CHANNEL} (one-time download)..."
  rustup toolchain install "$RUST_CHANNEL" --profile minimal \
    -c rust-src -c llvm-tools-preview
else
  echo "[setup] ${RUST_CHANNEL} already installed — skipping."
fi
rustup target add x86_64-unknown-none --toolchain "$RUST_CHANNEL" || true
rustup component add rust-src llvm-tools-preview --toolchain "$RUST_CHANNEL" || true
# Editor extras (also declared in rust-toolchain.toml) — best effort, not
# build-critical: a network hiccup here must not fail the whole setup.
rustup component add rust-analyzer rustfmt clippy --toolchain "$RUST_CHANNEL" 2>/dev/null || true

# The pin is only trustworthy if the compiler is EXACTLY the expected build.
GOT="$(rustc +"$RUST_CHANNEL" --version)"
if [ "${GOT#*"$RUST_EXPECT"}" = "$GOT" ]; then
  echo "[setup] FATAL: toolchain mismatch for ${RUST_CHANNEL}."
  echo "[setup]   wanted: rustc ${RUST_EXPECT}"
  echo "[setup]   got:    ${GOT}"
  echo "[setup] If you bumped the pin in rust-toolchain.toml, update RUST_EXPECT here too."
  exit 1
fi
echo "[setup] ✅ ${GOT}"

echo "[setup] Installing Limine bootloader (v12.9.4, pinned)..."
LIMINE_DIR="$HOME/limine"
LIMINE_VER="v12.9.4"
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

echo ""
echo "============================================================"
echo "[setup] ✅ Everything installed and pinned:"
echo "[setup]    Rust        ${GOT}  (${RUST_CHANNEL}, exact)"
echo "[setup]    Limine      ${LIMINE_VER}"
echo "[setup]    QEMU/apt    installed"
echo "[setup] Next step (one command):"
echo "[setup]     make run-release"
echo "============================================================"

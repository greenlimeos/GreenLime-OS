#!/usr/bin/env bash
# GreenLime OS — one-shot diagnostics for pasting to AI / issues.
# Usage: bash scripts/collect-logs.sh
# Output: everything needed (build log, ISO layout, QEMU + Limine versions, serial log).
set -uo pipefail

echo "================ GreenLime diag $(date -u +%FT%TZ) ================"
echo "--- versions ---"
rustc +nightly --version 2>&1 || rustc --version
qemu-system-x86_64 --version 2>&1 | head -2
~/limine/limine --version 2>&1 | head -2 || echo "(limine tool: no --version)"
cat Cargo.toml | grep -A6 workspace.dependencies
echo ""
echo "--- build ---"
cargo +nightly build -p greenlime-kernel --target x86_64-unknown-none 2>&1 | tail -30
echo ""
echo "--- iso layout ---"
ls -lh dist/ 2>&1
xorriso -indev dist/GreenLime.iso -find 2>&1 | head -20 || true
echo ""
echo "--- limine.conf ---"
cat bootloader/limine.conf
echo ""
echo "--- serial logs (per-boot; run-qemu.sh names them per ISO) ---"
cat dist/serial*.log 2>/dev/null || echo "(no dist/serial*.log yet — run: bash scripts/run-qemu.sh bios)"
echo ""
echo "================ END DIAG — copy everything above ================"

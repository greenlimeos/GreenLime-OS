#!/usr/bin/env bash
# GreenLime OS — QEMU runner. `make run` prints the serial log on exit.
# Usage:
#   bash scripts/run-qemu.sh bios        # GUI window, log dumped on exit
#   bash scripts/run-qemu.sh nographic   # no window, log dumped on exit (Ctrl+C to exit)
#   bash scripts/run-qemu.sh debug       # GDB stub on :1234, frozen, attach from 2nd terminal
#   bash scripts/run-qemu.sh uefi        # UEFI boot with OVMF
set -euo pipefail
MODE="${1:-bios}"
ISO="${2:-}"
mkdir -p dist

# If the caller didn't name an ISO, use the release one when it's the only
# thing built, else the debug ISO. Resolved AFTER the arg so an empty or
# bad argument can never reach QEMU as an empty -chardev logfile=.
if [ -z "$ISO" ]; then
  if [ -f dist/GreenLime-release.iso ] && [ ! -f dist/GreenLime.iso ]; then
    ISO=dist/GreenLime-release.iso
  else
    ISO=dist/GreenLime.iso
  fi
fi

# Never let a missing/odd ISO path reach QEMU: it exits with no window.
if [ ! -f "$ISO" ]; then
  echo "[qemu] $ISO missing, building first..."
  make iso
  ISO=dist/GreenLime.iso
fi

# Serial log is named after the ISO, so `make run` and `make run-release`
# don't clobber each other. Guarded: an empty name would make QEMU exit
# before opening a window ("Could not create ''").
BASE="${ISO##*/}"; BASE="${BASE%.iso}"
[ -n "$BASE" ] || BASE="GreenLime"
LOG="dist/serial-$BASE.log"

QEMU_COMMON=(-cpu qemu64,+smep,+smap -cdrom "$ISO" -m 512M -vga std -machine q35 -no-reboot)
# CPU count (Phase A: SMP bring-up, APs print + park; BSP is fail-soft).
# Headless SMP tests override with QEMU_SMP=4.
QEMU_COMMON+=(-smp "${QEMU_SMP:-2}")
# Boot the CD first: without this SeaBIOS tries the virtio disk and hangs
# before Limine ever runs.
QEMU_COMMON+=(-boot order=d)
# Virtio test disk (created by scripts/make-disk.sh via `make disk`).
DISK="dist/disk.img"
if [ ! -f "$DISK" ]; then
  echo "[qemu] no test disk, creating..."
  bash scripts/make-disk.sh
fi
QEMU_COMMON+=(-drive "file=$DISK,if=virtio,format=raw")
# User-mode NAT NIC (Phase B): guest gets 10.0.2.15, gateway/DHCP/DNS at
# 10.0.2.2. Only live runs get it — the headless suites keep their exact
# old argv (no NIC) so nothing regresses; the net test passes its own.
QEMU_COMMON+=(-device virtio-net-pci,netdev=n0 -netdev user,id=n0)
# Absolute pointer: the guest cursor tracks the host cursor exactly.
# QEMU's virtio-input is modern-only; our driver walks the PCI caps.
QEMU_COMMON+=(-device virtio-tablet-pci)
SERIAL=( -chardev stdio,id=ser0,mux=on,signal=on,logfile="$LOG" -serial chardev:ser0 )

dump_log() {
  echo ""
  echo "================ SERIAL LOG ($LOG) — COPY EVERYTHING BELOW ================"
  cat "$LOG" 2>/dev/null || echo "(empty — kernel printed nothing to COM1)"
  echo "================ END LOG ================"
}

# Promote the virtio-tablet to QEMU's console pointer (seamless host->guest
# cursor). QEMU registers its PS/2 mouse before the tablet, so without help
# PS/2 stays the console pointer: GTK grabs on click (host cursor vanishes)
# and the guest cursor never moves. `mouse_set` fixes that. See docs/GUI.md.
#
# GUI modes only: promote the virtio-tablet to QEMU's console pointer
# (seamless host->guest cursor). QEMU registers its PS/2 mouse before the
# tablet, so without help PS/2 stays the console pointer: GTK grabs on click
# (host cursor vanishes) and the guest cursor never moves. `mouse_set` fixes
# that. See docs/GUI.md.
#
# NOTE: no $( ) around this. A command substitution forks a subshell, and
# bash kills its backgrounded jobs on exit -- which would kill the helper
# (or, with a trailing `rm -f "$MON"`, delete the socket after QEMU bound
# it, so QEMU exits 1 and no window appears). The helper double-forks to
# detach; failure is non-fatal and only costs the seamless cursor.
MON="/tmp/greenlime-mon.sock"
start_tablet_helper() {
  python3 scripts/activate_tablet.py "$MON" >/dev/null 2>&1 &
}

trap dump_log EXIT

case "$MODE" in
  bios)
    echo "[qemu] BIOS + GTK. Log -> $LOG, dumped below on exit. Exit: Ctrl+C in terminal, Ctrl+A,X, or close window"
    rm -f "$LOG"
    start_tablet_helper
    qemu-system-x86_64 "${QEMU_COMMON[@]}" "${SERIAL[@]}" \
      -monitor "unix:$MON,server,nowait" -display gtk,show-cursor=on || true
    ;;
  uefi)
    OVMF="/usr/share/OVMF/OVMF_CODE_4M.fd"
    [ -f "$OVMF" ] || OVMF="/usr/share/ovmf/OVMF.fd"
    echo "[qemu] UEFI + GTK. Log -> $LOG, dumped below on exit."
    rm -f "$LOG"
    start_tablet_helper
    qemu-system-x86_64 -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
      "${QEMU_COMMON[@]}" "${SERIAL[@]}" \
      -monitor "unix:$MON,server,nowait" -display gtk,show-cursor=on || true
    ;;
  nographic)
    echo "[qemu] nographic. Log -> $LOG, dumped below on exit. Exit: Ctrl+A, X or Ctrl+C"
    rm -f "$LOG"
    qemu-system-x86_64 "${QEMU_COMMON[@]}" "${SERIAL[@]}" -display none || true
    ;;
  debug)
    echo "[qemu] GDB stub on :1234, CPU frozen. From a 2nd WSL terminal run:"
    echo "  gdb-multiarch ${CARGO_TARGET_DIR:-$HOME/.cache/greenlime-target}/x86_64-unknown-none/debug/kernel.elf -ex 'target remote :1234' -ex 'break kmain' -ex 'continue'"
    echo "[qemu] Serial mirrors to $LOG."
    qemu-system-x86_64 "${QEMU_COMMON[@]}" "${SERIAL[@]}" -display none -s -S || true
    ;;
  *)
    echo "unknown mode: $MODE (bios|uefi|nographic|debug)"; exit 1
    ;;
esac

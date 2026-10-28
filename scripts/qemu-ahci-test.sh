#!/usr/bin/env bash
# GreenLime OS — AHCI driver proof (M19). Runs real QEMU boots and greps
# the serial log, so every claim below is an observed kernel line.
#
# Cases:
#  1. sata  — FAT32 test volume attached to the ICH9 AHCI (ide-hd; QEMU 11
#             dropped `if=sata`), NO virtio disk. Claim: ahci claims it,
#             FAT32 mounts.
#  2. dual  — the same SATA volume PLUS the virtio test disk (suite-truth
#             setup). Claim: AHCI is preferred; virtio probe bails out
#             ("no disk" style), storage reports "ahci ready".
#  3. suite — the exact m18-suite argv (virtio only, empty ICH9). Claim:
#             the AHCI probe is neutral (no claim, no engine state
#             touched) and virtio stays the disk.
set -euo pipefail
cd "$(dirname "$0")/.."

ISO=dist/GreenLime-release.iso
echo "[ahci-test] building release ISO..."
make iso-release >/dev/null 2>&1

IMG=dist/ahci-sata-test.img       # scratch copy — NEVER the real test disk
cp -f dist/m18-disk.img "$IMG"    # same 32 MiB FAT32 volume the suite uses

# Options-only array; the binary is invoked below with `timeout`.
QEMU=(-machine q35 -m 512M -cpu qemu64,+smep,+smap -smp 2
      -cdrom "$ISO" -boot order=d -no-reboot -display none)

run_case() { # name, then QEMU drive args
  local name="$1"; shift
  local log="dist/serial-ahci-$name.log"
  rm -f "$log"
  echo "[ahci-test] case '$name' (25 s boot window) -> $log"
  timeout 25 qemu-system-x86_64 "${QEMU[@]}" -serial "file:$log" "$@" >/dev/null 2>&1 || true
}

grep_line() { # file, regex, verdict name  -> PASS/FAIL line  (-E: + and
  # [0-9]+ quantifiers are real regexes, not BRE literals)
  if grep -qE "$2" "$1"; then
    echo "[ahci-test] PASS  $3"
  else
    echo "[ahci-test] FAIL  $3"
  fi
}
absent_line() {
  if grep -qE "$2" "$1"; then
    echo "[ahci-test] FAIL  $3 (unexpectedly present)"
  else
    echo "[ahci-test] PASS  $3"
  fi
}

# ---- case 1: SATA only ----
# (QEMU 11 dropped `if=sata`; the modern attach is if=none + ide-hd, which
# lands the device on the ich9-ahci bus already inside q35.)
run_case sata -drive "file=$IMG,format=raw,if=none,id=sd0" -device ide-hd,drive=sd0
L1=dist/serial-ahci-sata.log
grep_line "$L1" '\[drivers\] ahci: disk [0-9]+ sectors' 'AHCI: identified the SATA disk'
grep_line "$L1" '\[drivers\] storage: ahci ready'      'claim: storage = ahci'
grep_line "$L1" '\[servers\] vfs: ramfs \+ FAT32 disk' 'mount: real hardware path reads BPB'

# ---- case 2: SATA + virtio (preference) ----
run_case dual -drive "file=$IMG,format=raw,if=none,id=sd0" -device ide-hd,drive=sd0 \
  -drive "file=dist/disk.img,if=virtio,format=raw"
L2=dist/serial-ahci-dual.log
grep_line   "$L2" '\[drivers\] storage: ahci ready'        'claim: ahci wins with both attached'
absent_line "$L2" '\[drivers\] storage: virtio-blk ready' 'claim: virtio probe skipped'
grep_line   "$L2" '\[servers\] vfs: ramfs \+ FAT32 disk'  'mount: still FAT32 (from AHCI)'

# ---- case 3: exact suite argv (neutrality) ----
run_case suite -drive "file=dist/m18-disk.img,if=virtio,format=raw"
L3=dist/serial-ahci-suite.log
absent_line "$L3" '\[drivers\] ahci: disk'          'neutral: empty controller never claims'
absent_line "$L3" '\[drivers\] storage: ahci ready' 'neutral: storage stays virtio'
grep_line   "$L3" '\[drivers\] storage: virtio-blk ready' 'virtio disk still ready'
grep_line   "$L3" '\[servers\] vfs: ramfs \+ FAT32 disk' 'virtio mount unchanged'

echo "[ahci-test] done"

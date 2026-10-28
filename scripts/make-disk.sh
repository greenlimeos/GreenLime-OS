#!/usr/bin/env bash
# GreenLime OS — build the FAT32 test disk image (dist/disk.img).
# Needs: mtools, dd. Run inside WSL. Idempotent.
set -euo pipefail

IMG="dist/disk.img"
mkdir -p dist testdata/disk/docs

if [ -f "$IMG" ]; then
  echo "[disk] $IMG exists, reuse (delete it to rebuild)"
  exit 0
fi

echo "[disk] creating 64MB FAT32 image (M19: room for v2 .gle containers)..."
dd if=/dev/zero of="$IMG" bs=1M count=64 status=none
mformat -i "$IMG" -F -v GREENLIME ::
mcopy -s -i "$IMG" testdata/disk/* ::
echo "[disk] contents:"
mdir -i "$IMG" ::
echo "[disk] $IMG ready"

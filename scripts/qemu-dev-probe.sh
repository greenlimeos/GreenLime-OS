#!/usr/bin/env bash
# Dev helper: which PCI IDs do virtio-tablet-pci variants present?
for dev in \
  'virtio-tablet-pci' \
  'virtio-tablet-pci,disable-modern=on' \
  'virtio-tablet-pci,disable-legacy=off,disable-modern=on' \
  'virtio-tablet-pci,disable-legacy=off'
do
  echo "== $dev"
  printf 'info pci\nquit\n' | qemu-system-x86_64 -machine q35 -device "$dev" \
    -display none -monitor stdio 2>/dev/null \
    | grep -E -A5 'device +3,'
done

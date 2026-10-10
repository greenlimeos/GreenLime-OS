#!/bin/bash
# Convert the current test screenshots to PNG for visual review.
cd "$(dirname "$0")/.." || exit 1
for f in fb-menu fb-about fb-dismissed fb-multi-open fb-multi-moved fb-tablet-menu fb-splash; do
  [ -f "dist/$f.ppm" ] || continue
  python3 scripts/ppm2png.py "dist/$f.ppm" "dist/$f.png" || exit 1
  echo "dist/$f.png"
done

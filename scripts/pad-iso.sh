#!/usr/bin/env bash
# Pad the release ISO to a 1 MiB boundary, then limine BIOS-install it.
# Why: the isohybrid GPT conversion ("Partition 2 ends past the device")
# breaks when the image is not 4-sector aligned — kernel size changes
# (M18 baked in the notepad ELF) flipped the old lucky alignment.
set -e
cd "$(dirname "$0")/.."
p="dist/GreenLime-release.iso"
before=$(stat -c %s "$p")
python3 - <<EOF
import os
p = "$p"
s = os.path.getsize(p)
ns = (s + 1048575) // 1048576 * 1048576
with open(p, "ab") as f:
    f.write(b"\0" * (ns - s))
print("padded", s, "->", ns)
EOF
"$LIMINE_DIR/limine" bios-install "$p" 2>/dev/null || /home/denis/limine/limine bios-install "$p"
after=$(stat -c %s "$p")
echo "ISO $before -> $after bytes, BIOS stages installed"

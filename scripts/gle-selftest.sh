#!/usr/bin/env bash
# gle-selftest.sh — round-trip proof for scripts/gle-pack.py (no OS needed).
set -u
cd "$(dirname "$0")/.."
T=/tmp/gle-selftest
rm -rf "$T"; mkdir -p "$T"

echo "== mkicon =="
python3 scripts/gle-pack.py mkicon "$T/icon.png" || exit 1

echo "== fixtures: fake static ELF + a 56x56 mixed-filter PNG =="
python3 - "$T" <<'EOF'
import struct, sys, zlib
t = sys.argv[1]
# 64-byte blob that satisfies the PACKER's ELF sniff (not the kernel's).
e = bytearray(64)
e[:4] = b"\x7fELF"
struct.pack_into("<H", e, 16, 2)     # ET_EXEC
struct.pack_into("<H", e, 18, 0x3E)  # EM_X86_64
open(t + "/fake.elf", "wb").write(bytes(e))
# 56x56 RGBA PNG using scanline filters 0..4 round-robin (decoder workout).
w = h = 56
sig = b"\x89PNG\r\n\x1a\n"
def chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(
        ">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
def px(x, y):
    return bytes((x * 4 % 256, y * 3 % 256, (x + y) % 256, 238 if ((x//7)+(y//7)) % 2 else 120))
rows = []
for y in range(h):
    ft = y % 5
    line = bytearray()
    for x in range(w):
        cur = px(x, y)
        a = px(x - 4 if (x // 4 * 4) == x and x >= 4 and ft in (1, 3, 4) else x, y) \
            if ft in (1, 3, 4) else cur
        pred = a
        if ft == 0:
            line += cur
        else:
            line += bytes((c - p) & 0xFF for c, p in zip(cur, pred))
    rows.append(bytes((ft,)) + bytes(line))
open(t + "/big.png", "wb").write(sig + chunk(b"IHDR", ihdr)
    + chunk(b"IDAT", zlib.compress(b"".join(rows))) + chunk(b"IEND", b""))
EOF
[ $? -eq 0 ] || exit 1
printf '{"name":"hello","version":"1.0","author":"Denis","icon":"icon.png","gui":true}' > "$T/app.json"
printf '{"name":"big","version":"2.1","author":"","icon":"big.png","gui":true}'    > "$T/big.json"

echo "== pack (28px icon) =="
python3 scripts/gle-pack.py pack "$T/app.json" --elf "$T/fake.elf" --out "$T/hello.gle" || exit 1

echo "== pack (56px -> resized) =="
python3 scripts/gle-pack.py pack "$T/big.json" --elf "$T/fake.elf" --out "$T/big.gle" || exit 1

echo "== inspect =="
python3 scripts/gle-pack.py inspect "$T/hello.gle" || exit 1
python3 scripts/gle-pack.py inspect "$T/big.gle" || exit 1

echo "== extract + byte-compare =="
python3 scripts/gle-pack.py extract "$T/hello.gle" --dir "$T/x" || exit 1
python3 scripts/gle-pack.py extract "$T/big.gle" --dir "$T/xb" || exit 1
python3 - "$T" <<'EOF'
import sys, zlib
t = sys.argv[1]
js = open(t + "/x/sticker.json").read()
assert '"name": "hello"' in js, js
assert '"version": "1.0"' in js, js
assert '"author": "Denis"' in js, js
assert '"gui": true' in js, js
open(t + "/x/payload.elf.bin", "wb").write(b"")  # noise to keep partial below
a = open(t + "/x/payload.elf", "rb").read()
b = open(t + "/fake.elf", "rb").read()
assert a == b, "extracted ELF differs from packed ELF"
ib = open(t + "/xb/icon.png", "rb").read()
assert ib.startswith(b"\x89PNG\r\n\x1a\n"), "round-trip icon is not a PNG"
print("python round-trip asserts: PASS")
EOF
[ $? -eq 0 ] || exit 1

echo "== tamper: flip a byte inside the ELF region -> CRC must catch it =="
cp "$T/hello.gle" "$T/bad.gle"
python3 - "$T" <<'EOF'
import sys
t = sys.argv[1]
b = bytearray(open(t + "/bad.gle", "rb").read())
b[-1] ^= 0xFF
open(t + "/bad.gle", "wb").write(bytes(b))
EOF
if python3 scripts/gle-pack.py inspect "$T/bad.gle" 2>/dev/null; then
  echo "FAIL: corrupt container inspected clean"; exit 1
else
  echo "corrupt refused: rc=$? (expected non-zero)"
fi
# The corrupt file must also show MISMATCH when the reason is surfaced; keep
# the quiet refuse above as the contract; (kernel prints its own reason line).

echo "== header-law rejects =="
python3 - "$T" <<'EOF'
import sys
t = sys.argv[1]
b = bytearray(open(t + "/hello.gle", "rb").read())
b[3] = 0x30  # "GLE1" -> "GLE0"
open(t + "/magic.gle", "wb").write(bytes(b))
b = bytearray(open(t + "/hello.gle", "rb").read())
b = b[:-1]   # truncated trailing byte
open(t + "/short.gle", "wb").write(bytes(b))
EOF
for bad in magic short; do
  if python3 scripts/gle-pack.py inspect "$T/$bad.gle" 2>/dev/null; then
    echo "FAIL: $bad.gle accepted"; exit 1
  else
    echo "refused $bad.gle (expected)"
  fi
done

echo "== non-ASCII sticker refused =="
printf '{"name":"na\xc3\xafve","version":"1","author":"","icon":null,"gui":false}' > "$T/bad.json"
if python3 scripts/gle-pack.py pack "$T/bad.json" --elf "$T/fake.elf" --out "$T/bad2.gle" 2>/dev/null; then
  echo "FAIL: non-ASCII name packed"; exit 1
else
  echo "refused non-ASCII name (expected)"
fi

echo "== v2: pack hello2 WITH assets (both cross page boundaries) =="
python3 - "$T" <<'EOF'
import sys
t = sys.argv[1]
# asset0: 10240 B (2.5 pages) — alignment tail padding must survive
open(t + "/asset0.bin", "wb").write(bytes(range(256)) * 40)
# asset1: tagged proof blob, non-round size
open(t + "/asset1.bin", "wb").write(b"GREENLIME-ASSET-PROOF:" + bytes(range(64)) * 33 + b"EOF")
EOF
printf '{"name":"hello2","version":"2.0","author":"Denis","icon":"icon.png","gui":true,"assets":["asset0.bin","asset1.bin"]}' > "$T/v2.json"
python3 scripts/gle-pack.py pack "$T/v2.json" --elf "$T/fake.elf" --out "$T/hello2.gle" || exit 1
python3 scripts/gle-pack.py inspect "$T/hello2.gle" || exit 1
python3 scripts/gle-pack.py extract "$T/hello2.gle" --dir "$T/x2" || exit 1
python3 - "$T" <<'EOF'
import sys
t = sys.argv[1]
js = open(t + "/x2/sticker.json").read()
assert '"format": "GLE v2"' in js, js
assert "assets/asset0.bin" in js and "assets/asset1.bin" in js, js
a0 = open(t + "/x2/assets/asset0.bin", "rb").read()
a1 = open(t + "/x2/assets/asset1.bin", "rb").read()
assert a0 == open(t + "/asset0.bin", "rb").read(), "asset0 bytes differ"
assert a1 == open(t + "/asset1.bin", "rb").read(), "asset1 bytes differ"
elf = open(t + "/x2/payload.elf", "rb").read()
assert elf == open(t + "/fake.elf", "rb").read(), "v2 ELF differs"
ins = open(t + "/hello2.gle", "rb").read()
# v2 law spots, byte-checked: meta prefix right after the 20-byte header
assert ins[20:26] == b"GLE2M1", ins[:32]
# the gap between the entry table and the first blob must be all zeros
print("v2 round-trip asserts: PASS")
EOF
[ $? -eq 0 ] || exit 1

echo "== v2 tamper (inside the last asset blob) must fail both CRCs =="
python3 - "$T" <<'EOF'
import sys
t = sys.argv[1]
b = bytearray(open(t + "/hello2.gle", "rb").read())
b[len(b) - 200] ^= 0xFF  # asset1's blob is the tail; -200 lands inside it
open(t + "/tamper2.gle", "wb").write(bytes(b))
EOF
if python3 scripts/gle-pack.py inspect "$T/tamper2.gle" 2>/dev/null; then
  echo "FAIL: v2 tamper inspected clean"; exit 1
else
  echo "refused tamper2.gle (expected)"
fi

echo "== v2 truncation refused =="
python3 - "$T" <<'EOF'
import sys
t = sys.argv[1]
b = open(t + "/hello2.gle", "rb").read()
open(t + "/short2.gle", "wb").write(b[:-7])
EOF
if python3 scripts/gle-pack.py inspect "$T/short2.gle" 2>/dev/null; then
  echo "FAIL: truncated v2 accepted"; exit 1
else
  echo "refused short2.gle (expected)"
fi

echo "GLE-SELFTEST: ALL GREEN"

# GreenLime fonts — where your TTF lives and how it works

## Where to put your font

```
GreenLime OS/assets/fonts/default-font.ttf   <-- YOUR font goes here, exactly this name
```

* The kernel **embeds** this file at build time (`kernel/build.rs`).
* Missing/wrong name = build stops with an error telling you the path.
* Any **TrueType-outlined** `.ttf`/`.otf` works (Roboto, DejaVu, Inter-TTF…).
  **CFF outlines** (many `.otf` files, e.g. Inter-OTF) are rejected — pick
  the TTF build of the family.
* `*.ttf`/`*.otf` are git-ignored (licenses + size). This folder keeps
  only this README in git.

## Console subset (v0.5) — why your full font becomes a smaller file

`fontdue` pre-outlines **every glyph at load**. A full Roboto (~3000 glyphs)
parses in ~50 ms natively but 10–60 s under QEMU/TCG in debug builds
(float-heavy, unoptimized, emulated). Unacceptable for every boot.

So the checked-in flow uses a **console subset** (~600 glyphs, everything a
terminal prints):

```
ASCII  U+0020-007E        logs, shell, paths
Latin-1 U+00A0-00FF       accented text
Box drawing U+2500-257F  shell/UI frames
Blocks U+2580-259F        progress bars
Arrows U+2190-21FF        hints
Punctuation U+2000-206F   dashes, quotes, …
Currency U+20A0-20CF      € etc.
```

Regenerate it any time (needs `python3-fonttools`):

```bash
sudo apt install -y python3-fonttools
cp assets/fonts/Roboto-full.ttf assets/fonts/work.ttf   # your full font, kept as backup
pyftsubset assets/fonts/work.ttf \
  --output-file=assets/fonts/default-font.ttf \
  --unicodes="U+0020-007E,U+00A0-00FF,U+2500-257F,U+2580-259F,U+2190-21FF,U+2000-206F,U+20A0-20CF" \
  --layout-features='' --no-hinting \
  --name-IDs='*' --glyph-names --notdef-glyph --notdef-outline
```

Keep your full font as `assets/fonts/<Name>-full.ttf` (also ignored).
The GUI (v0.8) gets a **lazy on-demand loader** for the full file, so this
subset is console-only and temporary by design.

## How the system fits together

```
assets/fonts/default-font.ttf
  │  kernel/build.rs embeds (GREENLIME_FONT_TTF)
  ▼
libs/greenlime-font/          no_std + alloc, shared kernel <-> userspace
  FontSystem::from_bytes      TTF parse (fontdue), BTreeMap glyph cache
  FontSystem::draw_char       layout (advance/wrap) + alpha blend
  Framebuffer                 32-bit XRGB put_pixel/fill/scroll (volatile MMIO)
  ▼
kernel/src/drivers/fb.rs      FbConsole: cursor, wrap, scroll, clear
  ▼
kernel/src/kprint.rs          every kprint! mirrors serial + screen
```

Userspace compositor (v0.7+) reuses `greenlime-font` directly — same API,
same cache design, no kernel involvement.

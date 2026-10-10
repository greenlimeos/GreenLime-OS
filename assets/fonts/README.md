# GreenLime fonts — drop your TTF here

The kernel embeds **`default-font.ttf`** from this folder at build time
(required — see `docs/FONTS.md`). Any TrueType-outlined `.ttf`/`.otf`
works (no CFF outlines).

Good free options (OFL licensed, find them yourself):
* DejaVu Sans / DejaVu Sans Mono
* Inter / Inter Tight
* Fira Code / Fira Sans
* IBM Plex Sans / IBM Plex Mono

Steps:
1. Download a `.ttf`.
2. Copy it here as exactly `default-font.ttf`:
   `assets/fonts/default-font.ttf`
3. `make run` — the framebuffer console renders with it.

NOTE: `*.ttf` files are git-ignored (licenses + size). This README stays.

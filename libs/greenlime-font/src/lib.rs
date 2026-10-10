//! GreenLime font system (v0.5): TrueType load, glyph cache, framebuffer text.
//! no_std + alloc. Glyph outlines via fontdue; layout, cache, blending, and
//! the framebuffer console contract are ours. Shared by the kernel console
//! now and the userspace compositor later (v0.7+).

#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use fontdue::{Font, FontSettings};

/// 8-bit per channel color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn pack(self) -> u32 {
        ((self.0 as u32) << 16) | ((self.1 as u32) << 8) | (self.2 as u32)
    }
}

/// A 32-bit XRGB framebuffer. Pixels are bounds-checked; writes are volatile
/// (usually MMIO). Created from bootloader info by whoever owns the mapping.
pub struct Framebuffer {
    base: *mut u8,
    pub width: u32,
    pub height: u32,
    pub pitch: u32, // bytes per row
}

// SAFETY: callers synchronize (kernel console holds a lock).
unsafe impl Send for Framebuffer {}

impl Framebuffer {
    /// SAFETY: `addr` must be a writable 32-bit XRGB mapping of at least
    /// `height` rows of `pitch` bytes for the framebuffer's lifetime.
    pub unsafe fn from_raw(addr: u64, width: u64, height: u64, pitch: u64) -> Self {
        Self {
            base: addr as *mut u8,
            width: width as u32,
            height: height as u32,
            pitch: pitch as u32,
        }
    }

    /// Raw base pointer (for cursor save/restore and friends).
    ///
    /// SAFETY: same contract as `from_raw`; caller bounds-checks.
    pub fn base_mut(&mut self) -> *mut u8 {
        self.base
    }

    /// Row stride in bytes.
    pub fn pitch(&self) -> u32 {
        self.pitch
    }

    #[inline]
    fn word_at(&mut self, x: u32, y: u32) -> Option<*mut u32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        // SAFETY: bounds checked above; pitch rows are mapped.
        unsafe {
            Some(self.base.add((y * self.pitch + x * 4) as usize) as *mut u32)
        }
    }

    pub fn put_pixel(&mut self, x: u32, y: u32, c: Rgb) {
        if let Some(w) = self.word_at(x, y) {
            unsafe { core::ptr::write_volatile(w, c.pack()) };
        }
    }

    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, c: Rgb) {
        let v = c.pack();
        for row in y..(y + h).min(self.height) {
            for col in x..(x + w).min(self.width) {
                if let Some(word) = self.word_at(col, row) {
                    unsafe { core::ptr::write_volatile(word, v) };
                }
            }
        }
    }

    pub fn clear(&mut self, c: Rgb) {
        let (w, h) = (self.width, self.height);
        self.fill_rect(0, 0, w, h, c);
    }

    /// Move all rows up by `px` pixels, fill the freed bottom band.
    /// Copy runs top-down (dst row < src row), so overlapping rows are safe.
    pub fn scroll_up(&mut self, px: u32, fill: Rgb) {
        self.scroll_region(0, 0, self.width, self.height, px, fill);
    }

    /// Scroll a sub-rectangle up by `px` pixels (for terminal regions).
    pub fn scroll_region(&mut self, x: u32, y: u32, w: u32, h: u32, px: u32, fill: Rgb) {
        let x1 = (x + w).min(self.width);
        let y1 = (y + h).min(self.height);
        if x >= x1 || y >= y1 || px == 0 {
            return;
        }
        if px >= y1 - y {
            self.fill_rect(x, y, x1 - x, y1 - y, fill);
            return;
        }
        let stride = (self.pitch / 4) as usize;
        let rows = (y1 - y - px) as usize;
        let cols = (x1 - x) as usize;
        let x0 = x as usize;
        let y0 = y as usize;
        let p = px as usize;
        for row in 0..rows {
            for col in 0..cols {
                // SAFETY: both rows fully inside the mapped region.
                unsafe {
                    let s =
                        (self.base as *const u32).add((y0 + row + p) * stride + x0 + col);
                    let d = (self.base as *mut u32).add((y0 + row) * stride + x0 + col);
                    core::ptr::write_volatile(d, core::ptr::read_volatile(s));
                }
            }
        }
        self.fill_rect(x, y1 - px, x1 - x, px, fill);
    }
}

/// f32::ceil lives in std/libm — reimplement with core-only ops.
fn ceil_u32(x: f32) -> u32 {
    let t = x as u32;
    if (t as f32) < x {
        t.saturating_add(1)
    } else {
        t
    }
}

fn ceil_i32(x: f32) -> i32 {
    let t = x as i32;
    if (t as f32) < x {
        t.saturating_add(1)
    } else {
        t
    }
}

/// Rasterized + cached glyph: coverage bitmap with bearings.
struct CachedGlyph {
    w: u32,
    h: u32,
    xmin: i32,
    ymin: i32,
    advance: u32,
    cover: Vec<u8>, // row-major, top row first, 0..255 alpha
}

/// TrueType font with a glyph cache. Sizes in pixels (f32 px, like CSS).
pub struct FontSystem {
    font: Font,
    cache: BTreeMap<(char, u32), CachedGlyph>,
}

impl FontSystem {
    /// Load from TTF/OTF bytes (TrueType outlines; CFF is rejected).
    pub fn from_bytes(data: &[u8]) -> Result<Self, &'static str> {
        let font = Font::from_bytes(data, FontSettings::default())?;
        Ok(Self {
            font,
            cache: BTreeMap::new(),
        })
    }

    /// Pixels per line at this size (ascent - descent + gap).
    pub fn line_height(&self, px: f32) -> u32 {
        match self.font.horizontal_line_metrics(px) {
            Some(m) => ceil_u32(m.ascent - m.descent + m.line_gap).max(1),
            None => ceil_u32(px * 1.25).max(1),
        }
    }

    fn glyph(&mut self, ch: char, px: f32) -> &CachedGlyph {
        let key = (ch, px.to_bits());
        if !self.cache.contains_key(&key) {
            let (m, cover) = self.font.rasterize(ch, px);
            let g = CachedGlyph {
                w: m.width as u32,
                h: m.height as u32,
                xmin: m.xmin as i32,
                ymin: m.ymin as i32,
                advance: ceil_u32(m.advance_width).max(1),
                cover,
            };
            self.cache.insert(key, g);
        }
        &self.cache[&key]
    }

    /// Horizontal advance of a char (for wrapping) without drawing.
    pub fn char_advance(&mut self, ch: char, px: f32) -> u32 {
        self.glyph(ch, px).advance
    }

    /// Draw one char with top-left cell origin (x, top), opaque bg cell of
    /// (advance × line_h). Returns the advance. Baseline = top + ascent.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_char(
        &mut self,
        fb: &mut Framebuffer,
        x: u32,
        top: u32,
        ch: char,
        px: f32,
        line_h: u32,
        fg: Rgb,
        bg: Rgb,
    ) -> u32 {
        let ascent = match self.font.horizontal_line_metrics(px) {
            Some(m) => ceil_i32(m.ascent),
            None => ceil_i32(px),
        };
        let g = self.glyph(ch, px);
        let (w, h, xmin, ymin, adv) = (g.w, g.h, g.xmin, g.ymin, g.advance);

        // Opaque cell first (keeps scroll/clear trivially correct).
        fb.fill_rect(x, top, adv, line_h, bg);

        // Blit coverage over the cell. Bitmap row 0 is the glyph top;
        // its screen row is baseline - ymin - h + row... fontdue's ymin is
        // the bitmap bottom relative to the baseline, so:
        //   screen_y(row) = baseline - ymin - h + row
        let baseline = top as i32 + ascent;
        for row in 0..h {
            for col in 0..w {
                let a = g.cover[(row * w + col) as usize];
                if a == 0 {
                    continue;
                }
                let sx = x as i32 + xmin + col as i32;
                let sy = baseline - ymin - h as i32 + row as i32;
                if sx < 0 || sy < 0 {
                    continue;
                }
                let (sx, sy) = (sx as u32, sy as u32);
                if sx >= fb.width || sy >= fb.height {
                    continue;
                }
                // Blend fg over bg by coverage alpha.
                let af = a as u32;
                let ab = 255 - af;
                let (fr, fgg, fb_) = (fg.0 as u32, fg.1 as u32, fg.2 as u32);
                let (br, bg_, bb) = (bg.0 as u32, bg.1 as u32, bg.2 as u32);
                let out = Rgb(
                    ((fr * af + br * ab) / 255) as u8,
                    ((fgg * af + bg_ * ab) / 255) as u8,
                    ((fb_ * af + bb * ab) / 255) as u8,
                );
                fb.put_pixel(sx, sy, out);
            }
        }
        adv
    }

    /// Draw one char blended over whatever is already on the framebuffer
    /// (no opaque cell) — for text on gradients/wallpapers where a solid
    /// cell box would show. Returns the advance.
    pub fn draw_char_blended(
        &mut self,
        fb: &mut Framebuffer,
        x: u32,
        top: u32,
        ch: char,
        px: f32,
        fg: Rgb,
    ) -> u32 {
        let ascent = match self.font.horizontal_line_metrics(px) {
            Some(m) => ceil_i32(m.ascent),
            None => ceil_i32(px),
        };
        let g = self.glyph(ch, px);
        let (w, h, xmin, ymin, adv) = (g.w, g.h, g.xmin, g.ymin, g.advance);
        let baseline = top as i32 + ascent;
        let stride = (fb.pitch() / 4) as usize;
        let base = fb.base_mut() as *const u32;
        for row in 0..h {
            for col in 0..w {
                let a = g.cover[(row * w + col) as usize];
                if a == 0 {
                    continue;
                }
                let sx = x as i32 + xmin + col as i32;
                let sy = baseline - ymin - h as i32 + row as i32;
                if sx < 0 || sy < 0 {
                    continue;
                }
                let (sx, sy) = (sx as u32, sy as u32);
                if sx >= fb.width || sy >= fb.height {
                    continue;
                }
                // SAFETY: bounds checked against the framebuffer above.
                let v = unsafe {
                    core::ptr::read_volatile(base.add(sy as usize * stride + sx as usize))
                };
                let bg = Rgb(((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8);
                let af = a as u32;
                let ab = 255 - af;
                let (fr, fgg, fb_) = (fg.0 as u32, fg.1 as u32, fg.2 as u32);
                let (br, bg_, bb) = (bg.0 as u32, bg.1 as u32, bg.2 as u32);
                let out = Rgb(
                    ((fr * af + br * ab) / 255) as u8,
                    ((fgg * af + bg_ * ab) / 255) as u8,
                    ((fb_ * af + bb * ab) / 255) as u8,
                );
                fb.put_pixel(sx, sy, out);
            }
        }
        adv
    }
}

//! Framebuffer console: TrueType text on screen, mirroring serial.
//! Owns the Limine framebuffer + embedded default font. Never kprintln!s
//! (that would recurse through kprint) — init reports success itself.

use greenlime_font::{FontSystem, Framebuffer, Rgb};
use spin::Mutex;

use crate::boot;

const FONT_PX: f32 = 16.0;
// Dark navy console + neutral text (matches the desktop theme).
const FG: Rgb = Rgb(214, 222, 235);
const BG: Rgb = Rgb(10, 14, 28);

static FONT_TTF: &[u8] = include_bytes!(env!("GREENLIME_FONT_TTF"));

pub struct FbConsole {
    fb: Framebuffer,
    font: FontSystem,
    x: u32,
    y: u32,
    line_h: u32,
    /// Splash mode: screen drawing suppressed (serial keeps every line).
    quiet: bool,
}

static FB: Mutex<Option<FbConsole>> = Mutex::new(None);

impl FbConsole {
    fn write_str(&mut self, s: &str) {
        if self.quiet {
            return; // splash owns the screen; serial already has the line
        }
        for ch in s.chars() {
            match ch {
                '\n' => self.newline(),
                '\r' => self.x = 0,
                c => {
                    let adv = self.font.char_advance(c, FONT_PX);
                    if self.x + adv >= self.fb.width {
                        self.newline();
                    }
                    let n = self.font.draw_char(
                        &mut self.fb,
                        self.x,
                        self.y,
                        c,
                        FONT_PX,
                        self.line_h,
                        FG,
                        BG,
                    );
                    self.x += n;
                }
            }
        }
    }

    fn newline(&mut self) {
        self.x = 0;
        self.y += self.line_h;
        while self.y + self.line_h > self.fb.height {
            self.fb.scroll_up(self.line_h, BG);
            self.y -= self.line_h;
        }
    }
}

/// Early init (from drivers): clear screen, take over. Returns screen size.
pub fn init() -> Option<(u32, u32)> {
    let info = boot::framebuffer()?;
    if info.bpp != 32 {
        return None; // serial-only on exotic modes
    }
    // SAFETY: Limine maps the framebuffer; address is directly writable.
    let mut fb =
        unsafe { Framebuffer::from_raw(info.address, info.width, info.height, info.pitch) };
    let font = FontSystem::from_bytes(FONT_TTF).ok()?;
    fb.clear(BG);
    let line_h = font.line_height(FONT_PX);
    let (w, h) = (fb.width, fb.height);
    *FB.lock() = Some(FbConsole {
        fb,
        font,
        x: 0,
        y: 0,
        line_h,
        quiet: false,
    });
    Some((w, h))
}

/// Boot splash: big centered title + subtitle on a cleared screen, then
/// silence the on-screen console (serial keeps every diagnostic line).
/// The window server repaints everything at handoff.
pub fn splash(title: &str, sub: &str) {
    let mut guard = FB.lock();
    let Some(con) = guard.as_mut() else {
        return;
    };
    con.fb.clear(BG);
    let (w, h) = (con.fb.width, con.fb.height);
    let FbConsole { fb, font, .. } = con;

    // Title, vertically centered a touch above the middle.
    let title_px = 64.0f32;
    let th = font.line_height(title_px);
    let tw: u32 = title.chars().map(|c| font.char_advance(c, title_px)).sum();
    let mut x = w.saturating_sub(tw) / 2;
    let y = h / 2 - th - 8;
    for c in title.chars() {
        x += font.draw_char(fb, x, y, c, title_px, th, Rgb(243, 245, 248), BG);
    }

    // Subtitle below the title.
    let sub_px = 18.0f32;
    let sh = font.line_height(sub_px);
    let sw: u32 = sub.chars().map(|c| font.char_advance(c, sub_px)).sum();
    let mut sx = w.saturating_sub(sw) / 2;
    let sy = h / 2 + 24;
    for c in sub.chars() {
        sx += font.draw_char(fb, sx, sy, c, sub_px, sh, Rgb(150, 165, 190), BG);
    }

    con.quiet = true;
}

/// Panic path: clear, home, and unsilence the screen console so the panic
/// message lands on the display instead of under the splash.
pub fn wake_console() {
    if let Some(con) = FB.lock().as_mut() {
        con.fb.clear(BG);
        con.x = 0;
        con.y = 0;
        con.quiet = false;
    }
}

/// Mirror a serial fragment to screen. No-op after GUI takeover.
pub fn print_str(s: &str) {
    if let Some(con) = FB.lock().as_mut() {
        con.write_str(s);
    }
}

/// GUI handoff: move the framebuffer + warm font cache out of the console
/// and stop mirroring (serial keeps working). The window server owns the
/// screen from here on. None if there is no framebuffer.
pub fn takeover() -> Option<(Framebuffer, FontSystem)> {
    let mut guard = FB.lock();
    let con = guard.take()?;
    Some((con.fb, con.font))
}

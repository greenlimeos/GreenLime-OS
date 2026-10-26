//! Window server (kernel task): owns the screen after fb takeover.
//! v1.1 — Windows 11 "Fluent dark" desktop, rebuilt from scratch.
//! Damage-rect compositor over a back buffer: state changes invalidate
//! rects; each lap composes ONLY the damage into the back buffer (layers
//! in z-order: wallpaper desktop, windows, taskbar, start card) and
//! blits it. The on-screen mouse cursor never lives in the back buffer
//! and stays DRAWN between laps: a move undoes its save-under at the
//! old spot (pointer), and a blit that would run over the sprite zone
//! undoes it just before present (steady_cursor) — under message floods
//! (canvas streaming) the compositor no longer blanks it every lap.
//!
//! Design (Windows 11 dark, minimal — no bloat):
//!   * Full-width translucent taskbar: one CENTERED cluster of 40px
//!     rounded buttons — start (brand-lime G), Files / Terminal /
//!     Settings pins, then one button per running client — and a right
//!     tray (just the two-line wall clock — icon glyphs removed).
//!     Open apps wear a rounded underline under their button (brand lime
//!     when focused, soft gray when merely running); hover lifts any
//!     button with a subtle rounded tint.
//!   * Start menu: a centered acrylic card floating above the bar —
//!     search field on top (typing live-filters the pinned grid),
//!     a 4-wide pinned-tile grid, and a footer with a user chip (lime
//!     avatar) and a power button. Nothing else — clean and minimal.
//!   * Windows: dark mica panels, 8px rounded corners (square while
//!     maximized, Windows 11 behavior), 1px border, 34px titlebar with
//!     the left-aligned title and flat maximize/close buttons (close
//!     hovers red, Windows 11 style), soft 4-ring drop shadow, and the
//!     120ms fade+rise open animation. Client and system windows share
//!     the exact same chrome — no browser-style tab strips.
//!   * Every floating surface is glass: alpha-blended with the wallpaper
//!     pixels beneath it (the wallpaper vec is the shared backdrop; the
//!     ring-3 wallpaper task streams the real image,
//!     assets/wallpapers/wallpaper.{png,jpeg,jpg}, via WIN_WALL_* IPC and
//!     the kernel only converts + blits).
//!
//! Window content stays text-only: terminal (id 0, line discipline,
//! opens on demand from the start menu — never at boot), Files (id 1,
//! vfs explorer), Settings (id 2, info card) + client windows
//! (WIN_CREATE, raw keys while focused). Focus follows clicks, titlebar
//! drag moves, corner grip resizes, taskbar buttons focus/raise,
//! double-click titlebar (or the maximize button) toggles maximize,
//! the left Windows key toggles the start card. Keys arrive via PS/2 or
//! virtio-tablet pointer; while the start card is open keys filter its
//! search instead. Hybrid note: in-kernel like vfs/dev; migrates out
//! with shared-memory windows later.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use greenlime_abi::ipc::{Message, PORT_WINSRV, MAX_PAYLOAD, REPLY_ERR, REPLY_OK};
use greenlime_abi::win::*;
use greenlime_font::{FontSystem, Framebuffer, Rgb};
use spin::Mutex;

use crate::arch::x86_64::idt;
use crate::arch::x86_64::simd;
use crate::drivers::{fb, input, virtio_input};
use crate::servers::vfs;
use crate::{ipc, kprintln, sched};

const FONT_PX: f32 = 16.0;
// ---- Palette: Windows 11 dark theme (charcoal mica, brand lime accent) --
// RULE: every painted pixel keeps ALL channels strictly < 239. The mouse
// cursor (240,240,240) must remain the only >=239 blob on screen —
// qmp_input_test / qmp_tablet_test locate the cursor by that predicate,
// and the wallpaper task clamps its own pixels the same way. Bright
// constants are capped just under the cursor by design.
const BG: Rgb = Rgb(32, 32, 32); // window body (#202020, Win11 mica dark)
const TITLE: Rgb = Rgb(43, 43, 43); // focused titlebar band (#2B2B2B)
const TITLE_UNFOC: Rgb = Rgb(37, 37, 38); // unfocused titlebar band
const FG: Rgb = Rgb(222, 226, 230); // window content text
const TITLE_FG: Rgb = Rgb(222, 226, 230); // focused title text
const FRAME: Rgb = Rgb(74, 78, 88); // focused window border
const FRAME_UNFOC: Rgb = Rgb(47, 49, 55); // unfocused window border
const BTN_HOVER: Rgb = Rgb(58, 60, 66); // titlebar button hover
const CLOSE_HOVER: Rgb = Rgb(196, 44, 30); // close button hover (#C42B1C)
const ICON_FG: Rgb = Rgb(200, 204, 210); // titlebar button glyph strokes
const SUB_FG: Rgb = Rgb(150, 154, 162); // hints / dim labels / clock date
const WHITE: Rgb = Rgb(232, 236, 242); // bright glyphs (tray, terminal)
const LABEL_FG: Rgb = Rgb(216, 220, 228); // tiles / tray text
const CURSOR: Rgb = Rgb(240, 240, 240); // tests detect >= 239 in ALL channels
// Glass tints: surfaces blend these over the wallpaper at the alphas below.
const BAR_TINT: Rgb = Rgb(20, 22, 27); // taskbar
const PANEL_TINT: Rgb = Rgb(18, 20, 26); // start card
const PANEL_BORDER: Rgb = Rgb(56, 60, 70); // card hairline
const SEARCH_BG: Rgb = Rgb(44, 46, 52); // search field fill
const HOVER: Rgb = Rgb(58, 61, 68); // hover lift on dark surfaces
const ACTIVE: Rgb = Rgb(48, 50, 58); // pressed / open state fill
const HAIRLINE: Rgb = Rgb(56, 58, 66); // taskbar top line + separators
// Brand + app icon colors.
const LIME: Rgb = Rgb(170, 232, 64); // brand accent: start logo + focus bar
const UND_SOFT: Rgb = Rgb(146, 150, 158); // running (unfocused) underline
const BLUE: Rgb = Rgb(76, 148, 244); // Files folder back panel
const BLUE_HI: Rgb = Rgb(122, 180, 250); // folder front sheet
const BALL_RED: Rgb = Rgb(225, 74, 58); // browser ball wedges
const BALL_GRN: Rgb = Rgb(52, 168, 83);
const BALL_YEL: Rgb = Rgb(244, 180, 6);
const BALL_BLU: Rgb = Rgb(66, 133, 244);
const BALL_WHITE: Rgb = Rgb(238, 240, 246);
const GEAR: Rgb = Rgb(158, 163, 172); // settings gear
const TERM_SQ: Rgb = Rgb(56, 58, 64); // terminal tile square
const FOOT_FG: Rgb = Rgb(176, 180, 188); // search / power strokes
const AVATAR_INK: Rgb = Rgb(30, 36, 18); // avatar initial on lime

/// Glass strengths (Q8 alpha).
const BAR_A: u32 = 205; // taskbar (~0.80)
const PANEL_A: u32 = 216; // start card (~0.84)

/// Corner radii (px).
const RAD_WIN: u32 = 8; // Win11 window corners (square while maximized)
const RAD_PANEL: u32 = 12; // start card
const RAD_CELL: u32 = 6; // taskbar buttons + tray
const RAD_TILE: u32 = 8; // start pinned tiles

/// Per-band drop-shadow strength for the 4 shadow rings (inner -> outer).
const SHADOW_A: [u32; 4] = [110, 70, 40, 22];

/// Open animation: length in scheduler ticks (10 ms each) + how far a
/// window/card rises from (in px). 12 ticks = 120 ms.
const ANIM_TICKS: u64 = 12;
const ANIM_RISE: i32 = 14;
/// One compose lap can take LONGER than the whole animation (a loaded
/// debug VM laps at ~1 s), so the fade invalidation keeps animating
/// surfaces alive for this much longer: the first post-expiry lap paints
/// the final crisp (p=256) frame.
const ANIM_TAIL: u64 = 180;

const TITLE_H: u32 = 34; // Win11-ish titlebar height
const TASKBAR_H: u32 = 48; // full-width bottom bar
const CELL: u32 = 40; // taskbar button
const PITCH: u32 = 44; // button pitch (4px gap)
/// Buttons after the pins (running clients), capped so the centered
/// cluster can never collide with the tray.
const MAX_CLIENTS: usize = 4;
const TRAY_W: u32 = 84; // clock zone (two-line clock; icon glyphs removed)

// Start card geometry.
const MENU_W: u32 = 560;
const MENU_H: u32 = 300;
const MENU_GAP: u32 = 12; // air between the card and the bar
const TILE_W: u32 = 126;
const TILE_H: u32 = 100;

const SCROLLBACK: usize = 200;
const CUR_W: u32 = 12;
const CUR_H: u32 = 14;
// Desktop layout (1280x800): terminal is a window, not fullscreen.
const TERM_X0: u32 = 150;
const TERM_Y0: u32 = 100;
const TERM_X1: u32 = 1130;
const TERM_Y1: u32 = 640;

const TERM_ID: u32 = 0;
/// Files window (system window, like the terminal): browsing the vfs.
const FILES_ID: u32 = 1;
/// Settings window (system window): static info card. Uses a HIGH
/// sentinel id on purpose: the serial contract is `[winsrv] win 2 open
/// at 700,120 540x330` — the first WIN_CREATE client (the demo) must be
/// id 2, so Settings must never occupy the low id space (it was a
/// non-window popup back when that contract was frozen).
const SETTINGS_ID: u32 = u32::MAX - 1;
/// No window focused (clean desktop: terminal opens on demand, never at boot).
const NO_FOCUS: u32 = u32::MAX;
/// Longest single terminal line (heap guard: the open line is uncapped by
/// SCROLLBACK, which only limits the line *count*).
const MAX_LINE: usize = 4096;
const MAX_WINS: usize = 8;
/// M15 canvas budget: max TOTAL canvas pixels across all windows (1 Mi px
/// = 4 MiB of u32 — the kernel heap also carries the 4 MiB back buffer +
/// the wallpaper cache; this keeps canvases a bounded share of it). A
/// SETUP beyond the budget answers REPLY_ERR instead of OOM-ing.
const CANVAS_BUDGET_PX: u64 = 1 << 20;
const GRIP: u32 = 16;
const MIN_W: u32 = 200;
const MIN_H: u32 = 120;
/// Drop-shadow depth (px) — the pad added to every shape invalidate.
const SHADOW_PAD: u32 = 12;

/// 12x14 arrow cursor, row bitmasks (bit 11 = leftmost pixel).
const CURSOR_BITS: [u16; 14] = [
    0b100000000000,
    0b110000000000,
    0b111000000000,
    0b111100000000,
    0b111110000000,
    0b111111000000,
    0b111111100000,
    0b111111110000,
    0b111111111000,
    0b111100000000,
    0b110110000000,
    0b100011000000,
    0b000011000000,
    0b000001100000,
];

/// x, y, w, h
type Rect = (u32, u32, u32, u32);

fn r_inter(a: Rect, b: Rect) -> bool {
    a.0 < b.0.saturating_add(b.2)
        && b.0 < a.0.saturating_add(a.2)
        && a.1 < b.1.saturating_add(b.3)
        && b.1 < a.1.saturating_add(a.3)
}

/// Intersection (zero-sized when disjoint).
fn r_clip(a: Rect, b: Rect) -> Rect {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0.saturating_add(a.2)).min(b.0.saturating_add(b.2));
    let y1 = (a.1.saturating_add(a.3)).min(b.1.saturating_add(b.3));
    if x1 <= x0 || y1 <= y0 {
        (0, 0, 0, 0)
    } else {
        (x0, y0, x1 - x0, y1 - y0)
    }
}

fn r_union(a: Rect, b: Rect) -> Rect {
    let x0 = a.0.min(b.0);
    let y0 = a.1.min(b.1);
    let x1 = a.0.saturating_add(a.2).max(b.0.saturating_add(b.2));
    let y1 = a.1.saturating_add(a.3).max(b.1.saturating_add(b.3));
    (x0, y0, x1 - x0, y1 - y0)
}

/// Grow a rect by the drop-shadow margin on every side: anything that
/// changes window/panel pixels must also repaint the shadow rings, so
/// every shape-significant invalidate goes through this.
fn pad(r: Rect) -> Rect {
    (
        r.0.saturating_sub(SHADOW_PAD),
        r.1.saturating_sub(SHADOW_PAD),
        r.2 + 2 * SHADOW_PAD,
        r.3 + 2 * SHADOW_PAD,
    )
}

/// Fill `area` but only inside the damage rect `r` (compose clip).
fn fill_clip(fb: &mut Framebuffer, r: Rect, area: Rect, c: Rgb) {
    let q = r_clip(r, area);
    if q.2 > 0 && q.3 > 0 {
        fb.fill_rect(q.0, q.1, q.2, q.3, c);
    }
}

fn point_in(x: u32, y: u32, r: Rect) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// Start menu pinned apps (tile grid + search filter over the same list).
#[derive(Clone, Copy, PartialEq, Eq)]
enum App {
    Files,
    Terminal,
    Settings,
    Demo,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Files => "Files",
            App::Terminal => "Terminal",
            App::Settings => "Settings",
            App::Demo => "Demo",
        }
    }
}

const APPS: [(&str, App); 4] = [
    ("Files", App::Files),
    ("Terminal", App::Terminal),
    ("Settings", App::Settings),
    ("Demo", App::Demo),
];

/// Case-insensitive prefix/substring filter over an app name.
fn name_matches(name: &str, q: &[u8]) -> bool {
    if q.is_empty() {
        return true;
    }
    let b = name.as_bytes();
    if q.len() > b.len() {
        return false;
    }
    b.windows(q.len())
        .any(|w| w.iter().map(|c| c.to_ascii_lowercase()).eq(q.iter().copied()))
}

/// One window's geometry + state. Z-order = position in WinSrv::windows.
/// Every window (system + IPC client) wears the same Windows 11 chrome.
struct Window {
    id: u32,
    /// Creating task (kernel-stamped sender at WIN_CREATE; u32::MAX for
    /// system windows). Client ops require sender == owner (GLS-006).
    owner: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    /// Scheduler tick of creation: drives the open animation (fade+rise).
    born: u64,
    /// Saved geometry while maximized (double-click titlebar toggles).
    maxim: Option<(u32, u32, u32, u32)>,
    title: String,
    /// Text content (console rules; wrapped at render time).
    lines: VecDeque<String>,
    /// M15 graphics mode: Some((pixels, w, h)) — a per-client canvas
    /// anchored at the content viewport's top-left, 1:1 pixels (never
    /// scaled: integer-only compositor), painted OVER the window body and
    /// REPLACING the text lines while present. Owned + fed only by the
    /// window's owner over IPC (WIN_BLIT_*); freed by WIN_BLIT_END or
    /// when the window dies.
    canvas: Option<(Vec<u32>, u32, u32)>,
    /// Client-only raw input ring + at most one waiting reader.
    ring: [u8; 128],
    head: usize,
    len: usize,
    pending: Option<u32>,
}

impl Window {
    fn rect(&self) -> Rect {
        (self.x, self.y, self.w, self.h)
    }
    fn title_rect(&self) -> Rect {
        (self.x + 2, self.y + 2, self.w.saturating_sub(4), TITLE_H)
    }
    /// Content viewport: below the titlebar.
    fn content_rect(&self) -> Rect {
        (
            self.x + 8,
            self.y + TITLE_H + 8,
            self.w.saturating_sub(16),
            self.h.saturating_sub(TITLE_H + 16),
        )
    }
    /// Flat close button (top-right in the titlebar).
    fn close_rect(&self) -> Rect {
        (self.x + self.w - 26, self.y + 4, 20, 22)
    }
    /// Flat maximize toggle (left of the close button).
    fn max_rect(&self) -> Rect {
        (self.x + self.w - 46, self.y + 4, 20, 22)
    }
    /// Resize grip (bottom-right corner).
    fn grip_rect(&self) -> Rect {
        (
            self.x + self.w - GRIP,
            self.y + self.h - GRIP,
            GRIP,
            GRIP,
        )
    }
}

/// Active pointer gesture: titlebar drag or corner resize.
#[derive(Clone, Copy)]
struct Drag {
    id: u32,
    dx: u32,
    dy: u32,
    resize: bool,
}

struct WinSrv {
    fb: Framebuffer,   // real screen (only blits + cursor touch it)
    /// Back-buffer pixel store: keeps the memory alive for `back` (the
    /// Framebuffer is just a view over it). Never read directly.
    #[allow(dead_code)]
    back_buf: Vec<u32>,
    back: Framebuffer, // view over back_buf; all composing happens here
    /// Packed XRGB wallpaper: initialized as a flat placeholder at
    /// takeover, then REPLACED row-pixels at a time as the ring-3
    /// wallpaper task streams the real image over IPC (WIN_WALL_*).
    /// The kernel never decodes; userspace owns the wallpaper.
    wallpaper: Vec<u32>,
    /// Wallpaper rows received since SETUP (for the DONE proof line).
    wall_msgs: u32,
    /// M15 canvas pixel-runs received since the last SETUP (proof line).
    canvas_msgs: u64,
    font: FontSystem,
    line_h: u32,
    /// Windows, bottom-to-top. The terminal (id 0) is NOT here at boot —
    /// it is created on demand by ensure_terminal() (see menu_launch).
    windows: Vec<Window>,
    /// Focused window id, or NO_FOCUS on a clean desktop.
    focus: u32,
    /// Damage to compose+blit this lap (union of invalidated rects).
    dirty: Option<Rect>,
    drag: Option<Drag>,
    last_clock: u64,
    // Mouse.
    mx: u32,
    my: u32,
    mouse_buf: [u32; 12 * 14],
    mouse_drawn: bool,
    pkt: [u8; 3],
    pkt_i: usize,
    prev_btn: u8,
    // Start card.
    start_open: bool,
    /// Scheduler tick the card was opened at (fade+rise animation).
    anim_start: u64,
    /// Start search query (filters the pinned tile grid live).
    search: [u8; 32],
    search_len: usize,
    // Canonical line discipline (set-1 scancodes -> line-buffered stdin).
    /// The line being typed (not yet committed with Enter).
    line_buf: [u8; 120],
    line_len: usize,
    /// Completed, unread terminal input lines (ring).
    in_ring: [u8; 128],
    in_head: usize,
    in_len: usize,
    /// Terminal text buffered while its window is closed (early boot
    /// output before the user opens it): capped like window scrollback.
    term_hist: VecDeque<String>,
    /// Terminal reader waiting for input (dev server: at most one).
    pending_reader: Option<u32>,
    shift: bool,
    caps: bool,
    e0: bool,
    next_id: u32,
    /// Double-click detector: last titlebar click (window id, tick).
    last_click: Option<(u32, u64)>,
    /// M15 canvas budget: total canvas pixels live across all windows
    /// (each pixel = one xrgb u32 from the 16 MiB kernel heap — 4 MiB of
    /// back buffer + 4 MiB cached wallpaper already live there).
    canvas_used: u64,
}

static SRV: Mutex<Option<WinSrv>> = Mutex::new(None);

impl WinSrv {
    fn take_over() -> Option<Self> {
        let (fb, font) = fb::takeover()?;
        let (w, h) = (fb.width, fb.height);
        // Back buffer: a same-size XRGB buffer on the heap (16 MiB heap,
        // 1280x800 = 4 MiB). Pointer stays valid when WinSrv moves (Vec
        // owns its data), so the Framebuffer view never dangles.
        let mut back_buf: Vec<u32> = alloc::vec![0u32; (w * h) as usize];
        let back = unsafe {
            Framebuffer::from_raw(back_buf.as_mut_ptr() as u64, w as u64, h as u64, (w * 4) as u64)
        };
        // Wallpaper: fixed-point, rendered once (heap: 4 MiB back + 4 MiB
        // wallpaper of the 16 MiB kernel heap).
        let wallpaper = render_wallpaper(w, h);
        // Clean desktop: NO windows at boot. The terminal (shell console)
        // opens on demand from the start menu; the demo client opens its
        // own window when it starts. Early terminal text is buffered in
        // term_hist until then (serial mirror stays live regardless).
        let mut srv = Self {
            fb,
            back_buf,
            back,
            wallpaper,
            wall_msgs: 0,
            canvas_msgs: 0,
            font,
            line_h: 20,
            windows: alloc::vec![],
            focus: NO_FOCUS,
            dirty: Some((0, 0, w, h)),
            drag: None,
            last_clock: u64::MAX,
            mx: w / 2,
            my: h / 2,
            mouse_buf: [0; 12 * 14],
            mouse_drawn: false,
            pkt: [0; 3],
            pkt_i: 0,
            prev_btn: 0,
            start_open: false,
            anim_start: 0,
            search: [0; 32],
            search_len: 0,
            line_buf: [0; 120],
            line_len: 0,
            in_ring: [0; 128],
            in_head: 0,
            in_len: 0,
            term_hist: VecDeque::new(),
            pending_reader: None,
            shift: false,
            caps: false,
            e0: false,
            next_id: 2, // 0/1 are system windows (terminal, Files); first client = id 2 (contract)
            last_click: None,
            canvas_used: 0,
        };
        srv.line_h = srv.font.line_height(FONT_PX);
        Some(srv)
    }

    // ---- damage / present -------------------------------------------------

    /// Mark a screen rect for recomposition (clamped to the screen).
    fn invalidate(&mut self, r: Rect) {
        let (sw, sh) = (self.fb.width, self.fb.height);
        let c = r_clip(r, (0, 0, sw, sh));
        if c.2 == 0 || c.3 == 0 {
            return;
        }
        self.dirty = Some(match self.dirty {
            None => c,
            Some(d) => r_union(d, c),
        });
    }

    /// Compose the damage into the back buffer, then blit it to screen.
    fn present(&mut self) {
        let Some(r) = self.dirty.take() else {
            return;
        };
        self.compose(r);
        self.blit(r);
    }

    fn blit(&mut self, r: Rect) {
        if r.2 == 0 || r.3 == 0 {
            return;
        }
        let back_stride = (self.back.pitch() / 4) as usize;
        let scr_stride = (self.fb.pitch() / 4) as usize;
        let back_base = self.back.base_mut() as *mut u32;
        let scr_base = self.fb.base_mut() as *mut u32;
        for row in r.1..(r.1 + r.3) {
            let src = unsafe { back_base.add(row as usize * back_stride + r.0 as usize) };
            let dst = unsafe { scr_base.add(row as usize * scr_stride + r.0 as usize) };
            // M13: SSE2 span copy (4 px/iter) — present() is the single
            // highest-bandwidth path in the compositor.
            simd::copy_span(dst, src, r.2 as usize);
        }
    }

    /// Draw every layer that intersects `r`, bottom-to-top. Writes may
    /// spill outside `r` (harmless: only `r` is blitted, and any later
    /// compose recomputes its rect from state anyway).
    fn compose(&mut self, r: Rect) {
        let (sw, sh) = (self.fb.width, self.fb.height);
        let now = idt::ticks();
        // Field-split borrow: the acrylic layers need the wallpaper slice
        // (immutable) and the back buffer (mutable) at the same time.
        let Self {
            back,
            font,
            wallpaper,
            windows,
            last_clock,
            line_h,
            focus,
            start_open,
            anim_start,
            search,
            search_len,
            mx,
            my,
            next_id,
            ..
        } = self;
        let wall: &[u32] = wallpaper;
        // 1. Desktop: cached wallpaper rows (integer-rendered once at
        //    takeover — SSE2 span copy per row, see simd.rs).
        let q = r_clip(r, (0, 0, sw, sh));
        if q.2 > 0 && q.3 > 0 {
            let stride = (back.pitch() / 4) as usize;
            let back_base = back.base_mut() as *mut u32;
            for y in q.1..q.1 + q.3 {
                let src = unsafe { wall.as_ptr().add(y as usize * stride + q.0 as usize) };
                let dst = unsafe { back_base.add(y as usize * stride + q.0 as usize) };
                // M13: SSE2 span copy (4 px/iter).
                simd::copy_span(dst, src, q.2 as usize);
            }
        }
        // 2. Windows, bottom-to-top (z-order): soft shadow ring first
        //    (blended over whatever is beneath), then the rounded frame.
        //    A freshly created window rises + fades in (wallpaper scrim).
        for w in windows.iter() {
            if !r_inter(r, w.rect()) && !r_inter(r, pad(w.rect())) {
                continue;
            }
            let p = anim_p(now, w.born);
            window_shadow(back, wall, r, w.rect(), p);
            draw_window(back, font, wall, w, *focus, r, *line_h, *mx, *my, now);
        }
        // 3. Taskbar: full-width acrylic bar — centered cluster + tray.
        let strip = strip_rect(sw, sh);
        if r_inter(r, strip) {
            let secs = now / 100;
            draw_taskbar(
                back,
                font,
                wall,
                windows,
                *next_id,
                *focus,
                *start_open,
                secs,
                r,
                sh,
                *mx,
                *my,
            );
            *last_clock = secs;
        }
        // 4. Start card (acrylic above everything, fade-in).
        if *start_open {
            let m = menu_rect(sw, sh);
            if r_inter(r, m) {
                draw_start(
                    back,
                    font,
                    wall,
                    r,
                    sh,
                    now,
                    *anim_start,
                    &search[..*search_len],
                    *mx,
                    *my,
                );
            }
        }
    }

    /// The tray is stale when the clock ticks.
    fn tick_taskbar(&mut self) {
        let secs = idt::ticks() / 100;
        if secs != self.last_clock {
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate(strip_rect(sw, sh));
        }
    }
    /// True while any open animation is still in flight (windows, card):
    /// those laps must compose + blit every tick.
    fn anim_active(&self) -> bool {
        let now = idt::ticks();
        if self.start_open && now.saturating_sub(self.anim_start) < ANIM_TICKS + ANIM_TAIL {
            return true;
        }
        self.windows
            .iter()
            .any(|w| now.saturating_sub(w.born) < ANIM_TICKS + ANIM_TAIL)
    }

    /// Re-mark animating surfaces as damaged so the fade keeps running:
    /// dirty is cleared by every present(), this regenerates it.
    fn tick_anim(&mut self) {
        let now = idt::ticks();
        if self.start_open && now.saturating_sub(self.anim_start) < ANIM_TICKS + ANIM_TAIL {
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate(pad(menu_rect(sw, sh)));
        }
        let mut young: Vec<Rect> = Vec::new();
        for w in &self.windows {
            if now.saturating_sub(w.born) < ANIM_TICKS + ANIM_TAIL {
                young.push(w.rect());
            }
        }
        for r in young {
            self.invalidate(pad(r));
        }
    }

    fn work_due() -> bool {
        match SRV.lock().as_ref() {
            Some(s) => {
                s.dirty.is_some() || idt::ticks() / 100 != s.last_clock || s.anim_active()
            }
            None => false,
        }
    }

    // ---- windows ----------------------------------------------------------

    fn idx_of(&self, id: u32) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    /// Focus + raise (click routing; the focused titlebar brightens and
    /// the taskbar underline turns lime).
    fn focus_window(&mut self, id: u32) {
        let Some(i) = self.idx_of(id) else {
            return;
        };
        let old_focus = self.focus;
        let last = self.windows.len() - 1;
        if i != last {
            let w = self.windows.remove(i);
            let rect = w.rect();
            self.windows.push(w);
            // Raise changes pixels inside the raised window + its shadow.
            self.invalidate(pad(rect));
        }
        self.focus = id;
        if old_focus != id {
            if let Some(j) = self.idx_of(old_focus) {
                let t = self.windows[j].title_rect();
                self.invalidate(t);
            }
            // Taskbar running underlines follow focus.
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate(strip_rect(sw, sh));
            kprintln!("[winsrv] focus win {}", id);
        }
    }

    fn destroy_window(&mut self, id: u32) {
        let Some(idx) = self.idx_of(id) else {
            return;
        };
        // EOF a waiting reader before the window vanishes.
        if let Some(to) = self.windows[idx].pending.take() {
            let m = Message::new(REPLY_ERR);
            ipc::send(to, m, sched::current_id().unwrap_or(u32::MAX));
        }
        // Free the canvas back to the budget before the window drops.
        if let Some((cv, _, _)) = self.windows[idx].canvas.as_ref() {
            self.canvas_used -= cv.len() as u64;
        }
        let rect = pad(self.windows[idx].rect());
        self.windows.remove(idx);
        // Focus falls back to the topmost remaining window (terminal if it
        // exists), or nowhere on an empty desktop.
        if self.focus == id {
            self.focus = self.windows.last().map(|w| w.id).unwrap_or(NO_FOCUS);
        }
        if self.drag.as_ref().map(|d| d.id) == Some(id) {
            self.drag = None;
        }
        kprintln!("[winsrv] win {} closed", id);
        self.invalidate(rect);
        let (sw, sh) = (self.fb.width, self.fb.height);
        self.invalidate(strip_rect(sw, sh));
    }

    /// Live titlebar drag / corner resize (pointer held down + moved).
    fn drag_update(&mut self) {
        let Some(d) = self.drag else {
            return;
        };
        let Some(idx) = self.idx_of(d.id) else {
            self.drag = None;
            return;
        };
        let (sw, sh) = (self.fb.width, self.fb.height);
        let old = self.windows[idx].rect();
        let (nx, ny, nw, nh);
        if d.resize {
            // Grip: resize from the bottom-right; keep a usable minimum.
            nx = self.windows[idx].x;
            ny = self.windows[idx].y;
            let max_w = sw.saturating_sub(nx).max(MIN_W);
            let max_h = sh.saturating_sub(TASKBAR_H + 4).saturating_sub(ny).max(MIN_H);
            nw = self.mx.saturating_sub(nx).max(MIN_W).min(max_w);
            nh = self.my.saturating_sub(ny).max(MIN_H).min(max_h);
        } else {
            nw = self.windows[idx].w;
            nh = self.windows[idx].h;
            nx = self
                .mx
                .saturating_sub(d.dx)
                .min(sw.saturating_sub(nw).max(0));
            ny = self
                .my
                .saturating_sub(d.dy)
                .min(sh.saturating_sub(TASKBAR_H + 4).saturating_sub(nh));
        }
        if (nx, ny, nw, nh) != old {
            self.windows[idx].x = nx;
            self.windows[idx].y = ny;
            self.windows[idx].w = nw;
            self.windows[idx].h = nh;
            let new = self.windows[idx].rect();
            self.invalidate(pad(old));
            self.invalidate(pad(new));
        }
    }

    fn drag_end(&mut self) {
        let Some(d) = self.drag.take() else {
            return;
        };
        if let Some(idx) = self.idx_of(d.id) {
            let w = &self.windows[idx];
            if d.resize {
                kprintln!("[winsrv] win {} resized {}x{}", d.id, w.w, w.h);
            } else {
                kprintln!("[winsrv] win {} moved to {},{}", d.id, w.x, w.y);
            }
        }
    }

    // ---- on-screen cursor (real fb only; never in the back buffer) -------

    fn undraw_cursor(&mut self) {
        if !self.mouse_drawn {
            return;
        }
        for row in 0..CUR_H as usize {
            for col in 0..CUR_W as usize {
                let x = self.mx + col as u32;
                let y = self.my + row as u32;
                if x < self.fb.width && y < self.fb.height {
                    self.fb.put_pixel(x, y, unpack(self.mouse_buf[row * CUR_W as usize + col]));
                }
            }
        }
        self.mouse_drawn = false;
    }

    fn draw_cursor(&mut self) {
        // Save-under: only trustworthy when the sprite was NOT on the fb
        // while we read (a drawn sprite in the zone would leak its ink
        // into mouse_buf — a later undraw would then ghost-replant those
        // pixels). steady_cursor undrew exactly whenever this lap's blit
        // ran over the zone — so if the sprite is already drawn AND its
        // pixels were untouched, the saved background is still exact and
        // must NOT be re-read from an fb that holds the sprite itself.
        let fresh = !self.mouse_drawn;
        for row in 0..CUR_H as usize {
            for col in 0..CUR_W as usize {
                let x = self.mx + col as u32;
                let y = self.my + row as u32;
                if x < self.fb.width && y < self.fb.height {
                    if fresh {
                        self.mouse_buf[row * CUR_W as usize + col] = read_pixel(&mut self.fb, x, y);
                    }
                    if CURSOR_BITS[row] & (1 << (11 - col)) != 0 {
                        self.fb.put_pixel(x, y, CURSOR);
                    }
                }
            }
        }
        self.mouse_drawn = true;
    }

    /// Undraw the sprite only when this lap's present would run over it
    /// (pending damage ∩ sprite zone). Keeps the cursor steady on screen
    /// while the server drains message floods (M15 canvas stream, drag
    /// damage): nothing erases it unless its own pixels are being replaced
    /// — the blit writes exactly the damage rect, so the sprite zone
    /// outside it keeps the true composed background between laps.
    fn steady_cursor(&mut self) {
        if !self.mouse_drawn {
            return; // pointer() already undrew it (a move undoes the paint)
        }
        let Some(r) = self.dirty else {
            return; // nothing blitted this lap: the zone keeps its pixels
        };
        let zone = (self.mx, self.my, CUR_W as u32, CUR_H as u32);
        if r_inter(r, zone) {
            self.undraw_cursor();
        }
    }

    // ---- input ------------------------------------------------------------

    /// Unified pointer event (PS/2 deltas resolve here too): press edge =
    /// click, held+motion = drag, release = drop. Every move damages the
    /// full hovered-widget rects at the old + new position so hover
    /// states (taskbar buttons, tray, tiles, titlebar buttons) stay live
    /// everywhere — see hover_damage.
    fn pointer(&mut self, x: u32, y: u32, btn: u8) {
        let moved = (x, y) != (self.mx, self.my);
        let (ox, oy) = (self.mx, self.my);
        if moved {
            // The save-under belongs to the OLD sprite position: restore
            // the background there BEFORE mx/my move (undraw_cursor reads
            // mx/my). The old zone needs no damage entry: the restore
            // writes back the exact pixels it covered, and any damage
            // already pending under it composes over the restored
            // background — correct either way.
            self.undraw_cursor();
        }
        self.mx = x;
        self.my = y;
        if moved {
            // Hover chips paint on full widget rects: damage the whole
            // hovered-widget set at both the old and new spot so a hover
            // flip paints (and erases) cleanly in one present — no seams
            // and no stale patches when the cursor leaves a wide widget.
            for r in self.hover_damage(ox, oy) {
                if let Some(r) = r {
                    self.invalidate(r);
                }
            }
            for r in self.hover_damage(x, y) {
                if let Some(r) = r {
                    self.invalidate(r);
                }
            }
        }
        let pressed = btn & 1 != 0;
        let was = self.prev_btn & 1 != 0;
        self.prev_btn = btn & 1;
        if pressed && !was {
            self.click(x, y);
        } else if pressed && moved {
            self.drag_update();
        }
        if !pressed && was {
            self.drag_end();
        }
    }

    /// Hover damage: the full rect of every widget whose hover chip
    /// depends on the point — bar cell, tray, start tiles + power button,
    /// window titlebar buttons. Chips paint inside the compose clip, so
    /// damaging a small cursor-following box would repaint only a slice
    /// of wide widgets (the 168px tray, start tiles): seams at the slice
    /// edge and stale highlight patches left where the cursor walked off.
    /// Damage always covers the whole widget, so a hover flip is painted
    /// (and erased) complete in a single present.
    fn hover_damage(&self, x: u32, y: u32) -> [Option<Rect>; 6] {
        let mut out = [None; 6];
        let mut n = 0usize;
        {
            let mut push = |r: Rect| {
                if n < out.len() {
                    out[n] = Some(r);
                    n += 1;
                }
            };
            let (sw, sh) = (self.fb.width, self.fb.height);
            // Taskbar: cells and the tray are disjoint; at most one matches.
            if y >= sh - TASKBAR_H {
                let nclient = self.nclients();
                let nc = bar_cell_count(nclient);
                for i in 0..nc {
                    let c = bar_cell_rect(i, sw, sh, nclient);
                    if point_in(x, y, c) {
                        push(c);
                    }
                }
                if point_in(x, y, tray_rect(sw, sh)) {
                    push(tray_rect(sw, sh));
                }
                return out;
            }
            // Start card tiles + power button (same filtered walk as draw).
            if self.start_open {
                let mut gi = 0u32;
                for (name, _) in APPS.iter() {
                    if !name_matches(name, &self.search[..self.search_len]) {
                        continue;
                    }
                    let t = tile_rect(gi, sw, sh);
                    gi += 1;
                    if point_in(x, y, t) {
                        push(t);
                    }
                }
                let pb = power_rect(sw, sh);
                if point_in(x, y, pb) {
                    push(pb);
                }
            }
            // Titlebar hover chips (maximize + close): draw_window's exact
            // geometry, including the birth rise, for the fading frames.
            let now = idt::ticks();
            for w in &self.windows {
                if w.w < 70 {
                    continue;
                }
                let p = anim_p(now, w.born);
                let rise = (((256 - p) * ANIM_RISE as u32) >> 8) as u32;
                for (bx, by) in [
                    (w.x + w.w - 46, w.y + rise + 4), // maximize (BTN_HOVER)
                    (w.x + w.w - 26, w.y + rise + 4), // close (CLOSE_HOVER)
                ] {
                    if point_in(x, y, (bx, by, 20, 22)) {
                        push((bx, by, 20, 22));
                    }
                }
            }
        }
        out
    }

    /// Left-click edge handling. Coordinates in screen pixels.
    fn click(&mut self, x: u32, y: u32) {
        let (sw, sh) = (self.fb.width, self.fb.height);
        // Taskbar usable with the start card open too. Clicks on the bar
        // but off a button are dead space: close the card.
        if y >= sh - TASKBAR_H {
            let nclient = self.nclients();
            let n = bar_cell_count(nclient);
            for i in 0..n {
                if point_in(x, y, bar_cell_rect(i, sw, sh, nclient)) {
                    self.bar_cell(i);
                    return;
                }
            }
            // Tray: interactive hover only — no flyout by design.
            if point_in(x, y, tray_rect(sw, sh)) {
                return;
            }
            self.close_start(); // empty bar space closes the card
            return;
        }
        // Start card open: tiles + power, else dismiss (click consumed).
        if self.start_open {
            let (mx0, my0, mw, mh) = menu_rect(sw, sh);
            // Tiles (filtered order must match draw_start).
            let mut gi = 0u32;
            for (name, app) in APPS.iter() {
                if !name_matches(name, &self.search[..self.search_len]) {
                    continue;
                }
                let t = tile_rect(gi, sw, sh);
                if point_in(x, y, t) {
                    self.menu_launch(*app);
                    return;
                }
                gi += 1;
            }
            // Footer: power button (right), user chip is inert.
            if point_in(x, y, power_rect(sw, sh)) {
                kprintln!("[winsrv] power");
                return;
            }
            if point_in(x, y, (mx0, my0, mw, mh)) {
                return; // inside card: keep it open
            }
            self.close_start();
            return;
        }
        // Windows, topmost first.
        for i in (0..self.windows.len()).rev() {
            let (wx, wy, ww, wh) = (
                self.windows[i].x,
                self.windows[i].y,
                self.windows[i].w,
                self.windows[i].h,
            );
            if x < wx || x >= wx + ww || y < wy || y >= wy + wh {
                continue;
            }
            let id = self.windows[i].id;
            // Close button (every window has one).
            {
                let c = self.windows[i].close_rect();
                if x >= c.0 && x < c.0 + c.2 && y >= c.1 && y < c.1 + c.3 {
                    self.destroy_window(id);
                    return;
                }
            }
            // Maximize button.
            let mb = self.windows[i].max_rect();
            if x >= mb.0 && x < mb.0 + mb.2 && y >= mb.1 && y < mb.1 + mb.3 {
                self.focus_window(id);
                self.toggle_max(id);
                return;
            }
            // Resize grip.
            let g = self.windows[i].grip_rect();
            if x >= g.0 && y >= g.1 {
                self.focus_window(id);
                self.drag = Some(Drag {
                    id,
                    dx: 0,
                    dy: 0,
                    resize: true,
                });
                return;
            }
            // Titlebar starts a move; the body just focuses. A double-click
            // on the titlebar toggles maximize first.
            let tb = self.windows[i].title_rect();
            let in_title = x >= tb.0 && x < tb.0 + tb.2 && y >= tb.1 && y < tb.1 + tb.3;
            let (dx, dy) = (x - wx, y - wy);
            let now = idt::ticks();
            let dbl = in_title
                && self
                    .last_click
                    .map(|(lid, lt)| lid == id && now <= lt + 25)
                    .unwrap_or(false);
            self.last_click = if in_title { Some((id, now)) } else { None };
            self.focus_window(id);
            if dbl {
                self.toggle_max(id);
                return;
            }
            if in_title {
                self.drag = Some(Drag {
                    id,
                    dx,
                    dy,
                    resize: false,
                });
            } else if id == FILES_ID {
                // Files body: click a directory line to navigate into it.
                self.files_click(id, y);
            }
            return;
        }
        // Desktop click: nothing.
    }

    /// Running client windows (not the Files/Terminal/Settings system
    /// windows): each gets its own taskbar button, id order.
    fn nclients(&self) -> usize {
        self.windows
            .iter()
            .filter(|w| w.id != TERM_ID && w.id != FILES_ID && w.id != SETTINGS_ID)
            .count()
    }

    /// Taskbar button i: 0 = start, 1..3 = pins, 4.. = running clients.
    fn bar_cell(&mut self, i: u32) {
        match i {
            0 => self.toggle_start(),
            1 => self.menu_launch(App::Files),
            2 => self.menu_launch(App::Terminal),
            3 => self.menu_launch(App::Settings),
            _ => {
                self.close_start();
                // Client buttons map in id ASC order (NOT Vec z-order —
                // the same walk the taskbar drawing's button count assumes).
                let target = (i - 4) as usize;
                let mut k = 0;
                for id in 0..self.next_id {
                    if id == TERM_ID || id == FILES_ID || id == SETTINGS_ID {
                        continue; // system windows: no client button
                    }
                    if self.idx_of(id).is_none() {
                        continue; // destroyed window: no button, no gap
                    }
                    if k == target {
                        self.focus_window(id);
                        return;
                    }
                    k += 1;
                }
            }
        }
    }

    fn toggle_start(&mut self) {
        if self.start_open {
            self.close_start();
        } else {
            self.start_open = true;
            self.anim_start = idt::ticks();
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate(pad(menu_rect(sw, sh)));
            self.invalidate(strip_rect(sw, sh));
            // Card flip prints: the card itself has no pixels-in-serial
            // story, so these two lines are how a test (or a human
            // reading the log) knows a start click actually landed —
            // without them a dead click and an open card look identical.
            kprintln!("[winsrv] start card open");
        }
    }

    fn close_start(&mut self) {
        if !self.start_open {
            return;
        }
        self.start_open = false;
        self.search_len = 0;
        kprintln!("[winsrv] start card closed");
        let (sw, sh) = (self.fb.width, self.fb.height);
        self.invalidate(pad(menu_rect(sw, sh)));
        self.invalidate(strip_rect(sw, sh));
    }

    /// Launch a start-menu app: close the card, then perform its action.
    fn menu_launch(&mut self, app: App) {
        self.close_start();
        match app {
            App::Files => self.ensure_files(),
            App::Terminal => self.ensure_terminal(),
            App::Demo => {
                // Demo = the demo client's window (it names itself "GreenLime
                // Demo"): focus it while its task is alive. M15.1 slot reap:
                // once the demo has EXITED (its task slot orbits back to the
                // spawner's free list), the tile boots a fresh demo user
                // task straight from the baked-in ELF — the start card is a
                // real launcher now, not a one-shot. The respawned demo
                // reuses its boot id (freelist LIFO), so the serial proof
                // line reads exactly like boot ("owner 5").
                let id = self
                    .windows
                    .iter()
                    .filter(|w| w.id != TERM_ID && w.id != FILES_ID && w.id != SETTINGS_ID)
                    .map(|w| w.id)
                    .next();
                if let Some(id) = id {
                    self.focus_window(id);
                } else {
                    let tid =
                        crate::sched::spawn_user_task("demo", crate::userspace::DEMO_ELF);
                    kprintln!("[winsrv] launch {} (respawned task {})", app.name(), tid);
                }
            }
            App::Settings => self.ensure_settings(),
        }
        kprintln!("[winsrv] launch {}", app.name());
    }

    /// Open the terminal on demand (it never opens at boot): create the
    /// system window, prefilled with whatever was buffered while it was
    /// closed (early shell banner), or just focus it if already open.
    fn ensure_terminal(&mut self) {
        if self.idx_of(TERM_ID).is_none() {
            let mut lines = core::mem::take(&mut self.term_hist);
            while lines.len() > SCROLLBACK {
                lines.pop_front();
            }
            self.windows.push(Window {
                id: TERM_ID,
                owner: u32::MAX, // system window: no IPC client owns it
                x: TERM_X0,
                y: TERM_Y0,
                w: TERM_X1 - TERM_X0,
                h: TERM_Y1 - TERM_Y0,
                born: idt::ticks(),
                maxim: None,
                title: String::from("Terminal"),
                lines,
                canvas: None,
                ring: [0; 128],
                head: 0,
                len: 0,
                pending: None,
            });
            kprintln!("[winsrv] terminal open");
            self.invalidate(pad((TERM_X0, TERM_Y0, TERM_X1 - TERM_X0, TERM_Y1 - TERM_Y0)));
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate(strip_rect(sw, sh));
        }
        self.focus_window(TERM_ID);
    }

    /// Open the Files window (system window): a vfs explorer. The body
    /// lists the current directory (`title` carries "Files - <path>");
    /// clicking a line ending in '/' navigates into it, "../" pops to the
    /// parent. Listing comes straight from vfs::list_names.
    fn ensure_files(&mut self) {
        if self.idx_of(FILES_ID).is_none() {
            self.files_open("/");
            kprintln!("[winsrv] files open");
        }
        self.focus_window(FILES_ID);
    }

    /// Open the Settings window (system window): a static info card.
    fn ensure_settings(&mut self) {
        if self.idx_of(SETTINGS_ID).is_none() {
            let (sw, sh) = (self.fb.width, self.fb.height);
            let secs = idt::ticks() / 100;
            let lines: Vec<String> = alloc::vec![
                alloc::format!("Display       {} x {}", sw, sh),
                alloc::format!(
                    "Uptime        {:02}:{:02}:{:02}",
                    secs / 3600,
                    (secs % 3600) / 60,
                    secs % 60
                ),
                alloc::format!("Windows       {} open", self.windows.len()),
                String::from("GreenLime OS v1.1"),
                String::from("made by GLM 5.3"),
            ];
            let (x, y) = (sw / 2 - 210, 170u32);
            self.windows.push(Window {
                id: SETTINGS_ID,
                owner: u32::MAX,
                x,
                y,
                w: 420,
                h: 300,
                born: idt::ticks(),
                maxim: None,
                title: String::from("Settings"),
                lines: lines.into_iter().collect(),
                canvas: None,
                ring: [0; 128],
                head: 0,
                len: 0,
                pending: None,
            });
            kprintln!("[winsrv] settings open");
            self.invalidate(pad((x, y, 420, 300)));
            self.invalidate(strip_rect(sw, sh));
        }
        self.focus_window(SETTINGS_ID);
    }

    /// Toggle maximized geometry (double-click on a titlebar or the
    /// maximize button): save the current rect once, stretch to the full
    /// work area; toggling again restores. Shadows repaint via pad().
    fn toggle_max(&mut self, id: u32) {
        let Some(i) = self.idx_of(id) else {
            return;
        };
        let (sw, sh) = (self.fb.width, self.fb.height);
        let old = self.windows[i].rect();
        let target = if let Some(saved) = self.windows[i].maxim {
            saved
        } else {
            self.windows[i].maxim = Some(old);
            (0, 0, sw, sh.saturating_sub(TASKBAR_H))
        };
        self.invalidate(pad(old));
        self.windows[i].x = target.0;
        self.windows[i].y = target.1;
        self.windows[i].w = target.2;
        self.windows[i].h = target.3;
        self.windows[i].maxim = if self.windows[i].maxim == Some(target) {
            None
        } else {
            self.windows[i].maxim
        };
        // A maximized window covers the desktop: refocus logic stays as-is.
        self.focus_window(id);
        self.invalidate(pad(self.windows[i].rect()));
        kprintln!("[winsrv] win {} toggled", id);
    }

    /// (Re)generate / focus the Files window at `path`.
    fn files_open(&mut self, path: &str) {
        let title = alloc::format!("Files - {}", path);
        let lines: Vec<String> = match crate::servers::vfs::list_names(path) {
            Some(names) => {
                let mut out: Vec<String> = Vec::new();
                if path != "/" {
                    out.push(String::from("../"));
                }
                if names.is_empty() {
                    out.push(String::from("(empty)"));
                } else {
                    for n in names {
                        out.push(n);
                    }
                }
                out
            }
            None => alloc::vec![String::from("(unreadable)")],
        };
        let lines: alloc::collections::VecDeque<String> = lines.into();
        if let Some(i) = self.idx_of(FILES_ID) {
            let w = &mut self.windows[i];
            w.title = title;
            w.lines = lines;
            let rect = w.rect();
            self.invalidate(rect);
        } else {
            self.windows.push(Window {
                id: FILES_ID,
                owner: u32::MAX,
                x: 230,
                y: 140,
                w: 540,
                h: 420,
                born: idt::ticks(),
                maxim: None,
                title,
                lines,
                canvas: None,
                ring: [0; 128],
                head: 0,
                len: 0,
                pending: None,
            });
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate(pad((230, 140, 540, 420)));
            self.invalidate(strip_rect(sw, sh));
        }
    }

    /// Files body click: resolve the row under `y` to a logical line and
    /// navigate if it is a directory ("../" or one ending with '/').
    fn files_click(&mut self, id: u32, y: u32) {
        // Font metrics first (borrow split before touching windows).
        let cw = self.font.char_advance('M', FONT_PX).max(1);
        let line_h = self.line_h;
        let Some(i) = self.idx_of(id) else {
            return;
        };
        let cr = self.windows[i].content_rect();
        if y < cr.1 {
            return;
        }
        let row = ((y - cr.1) / line_h.max(1)) as usize;
        let cols = (cr.2 / cw).max(1) as usize;
        let rows_of = |len: usize| len.div_ceil(cols).max(1);
        // draw_lines shows the tail that fits; replicate its skip + wrap
        // math so the clicked screen row maps to the logical line.
        let total: usize = self.windows[i].lines.iter().map(|l| rows_of(l.chars().count())).sum();
        let rows_max = (cr.3 / line_h).max(1) as usize;
        let skip = total.saturating_sub(rows_max);
        let probe = row + skip;
        let mut run = 0usize;
        let mut hit: Option<usize> = None;
        for (idx, l) in self.windows[i].lines.iter().enumerate() {
            let lr = rows_of(l.chars().count());
            if run + lr > probe {
                hit = Some(idx);
                break;
            }
            run += lr;
        }
        let Some(idx) = hit else {
            return;
        };
        let entry = self.windows[i].lines[idx].clone();
        if entry == "(empty)" || entry == "(unreadable)" || !entry.ends_with('/') {
            return; // plain file: nothing to open yet
        }
        // Current path lives in the title ("Files - <path>").
        let title = self.windows[i].title.clone();
        let cur = title.strip_prefix("Files - ").unwrap_or("/");
        let next = if entry == "../" {
            // Parent: "/a/b" -> "/a", "/a" -> "/", "/" stays "/". The path
            // is stored without a trailing slash (root excepted).
            let t = cur.trim_end_matches('/');
            match t.rfind('/') {
                None | Some(0) => String::from("/"),
                Some(p) => String::from(&t[..p]),
            }
        } else {
            let name = entry.trim_end_matches('/');
            if cur == "/" {
                alloc::format!("/{}", name)
            } else {
                alloc::format!("{}/{}", cur.trim_end_matches('/'), name)
            }
        };
        // Only navigate if the target is really a directory.
        if vfs::list_names(&next).is_some() {
            self.files_open(&next);
        }
    }

    /// Enter inside the start card: launch the first filtered app.
    fn menu_enter(&mut self) {
        let q = &self.search[..self.search_len];
        for (name, app) in APPS {
            if name_matches(name, q) {
                self.menu_launch(app);
                return;
            }
        }
    }

    fn poll_input(&mut self) -> bool {
        let mut changed = false;
        while let Some(sc) = input::pop_key() {
            self.decode_key(sc);
            changed = true;
        }
        if virtio_input::available() {
            // Tablet owns the pointer: swallow PS/2 mouse bytes (QEMU may
            // feed both) and track the absolute device instead.
            while input::pop_mouse().is_some() {}
            changed |= self.poll_tablet();
        } else {
            // Cap mouse drain per lap: a babbling device must not wedge us.
            for _ in 0..64 {
                let Some(b) = input::pop_mouse() else {
                    break;
                };
                // Assemble 3-byte packets (bit 3 of byte 0 syncs).
                if self.pkt_i == 0 && b & 0x08 == 0 {
                    continue;
                }
                self.pkt[self.pkt_i] = b;
                self.pkt_i += 1;
                if self.pkt_i == 3 {
                    self.pkt_i = 0;
                    let dx = self.pkt[1] as i32 - if self.pkt[0] & 0x10 != 0 { 256 } else { 0 };
                    let dy = self.pkt[2] as i32 - if self.pkt[0] & 0x20 != 0 { 256 } else { 0 };
                    let nx = (self.mx as i32 + dx).clamp(0, self.fb.width as i32 - 1) as u32;
                    let ny = (self.my as i32 - dy).clamp(0, self.fb.height as i32 - 1) as u32;
                    // Pointer handles motion, click edges, and drag.
                    self.pointer(nx, ny, self.pkt[0] & 1);
                }
                changed = true;
            }
        }
        changed
    }

    /// Absolute pointer report (virtio-tablet): position maps the host
    /// pointer exactly — same unified flow as the PS/2 path. One entry per
    /// complete report, so a press+release batched in a single drain both
    /// surface and pointer() sees the press edge.
    fn poll_tablet(&mut self) -> bool {
        let reports = virtio_input::poll();
        if reports.is_empty() {
            return false;
        }
        let (w, h) = (self.fb.width as u64, self.fb.height as u64);
        for (x, y, btn) in reports {
            let nx = (x as u64 * w.saturating_sub(1) / 32767) as u32;
            let ny = (y as u64 * h.saturating_sub(1) / 32767) as u32;
            self.pointer(nx, ny, btn & 1);
        }
        true
    }

    /// Turn a set-1 make/break code into input for the focused window.
    fn decode_key(&mut self, sc: u8) {
        if sc == 0xE0 {
            self.e0 = true;
            return;
        }
        if self.e0 {
            // Extended key (arrows/nav): not text, swallow it + its break.
            self.e0 = false;
            if sc == 0x5B {
                // Left Windows key: toggles the start card (Windows 11).
                self.toggle_start();
            }
            return;
        }
        if sc & 0x80 != 0 {
            // Break code: only modifier releases matter.
            if matches!(sc & 0x7F, 0x2A | 0x36) {
                self.shift = false;
            }
            return;
        }
        match sc {
            0x2A | 0x36 => {
                self.shift = true;
                return;
            }
            0x3A => {
                self.caps = !self.caps;
                return;
            }
            0x01 => {
                // Esc closes an open start card.
                if self.start_open {
                    self.close_start();
                }
                return;
            }
            0x1C => {
                if self.start_open {
                    self.menu_enter();
                } else {
                    self.press_enter();
                }
                return;
            }
            0x0E => {
                if self.start_open {
                    if self.search_len > 0 {
                        self.search_len -= 1;
                        let (sw, sh) = (self.fb.width, self.fb.height);
                        self.invalidate(menu_rect(sw, sh));
                    }
                } else {
                    self.press_backspace();
                }
                return;
            }
            _ => {}
        }
        let Some(ch) = scancode_ascii(sc, self.shift, self.caps) else {
            return;
        };
        if self.start_open {
            // The card grabs typing: the search field shows the query,
            // the pinned grid filters live.
            if (ch.is_ascii_graphic() || ch == b' ') && self.search_len < self.search.len() {
                self.search[self.search_len] = ch.to_ascii_lowercase();
                self.search_len += 1;
                let (sw, sh) = (self.fb.width, self.fb.height);
                self.invalidate(menu_rect(sw, sh));
            }
            return;
        }
        self.route_char(ch);
    }

    /// A decoded char: terminal = line discipline, client = raw ring.
    fn route_char(&mut self, ch: u8) {
        if self.focus == TERM_ID {
            if self.line_len >= self.line_buf.len() {
                return; // line full: drop (no wrap-around editing yet)
            }
            self.line_buf[self.line_len] = ch;
            self.line_len += 1;
            if let Ok(s) = core::str::from_utf8(&[ch]) {
                self.append_text_to(TERM_ID, s);
            }
        } else {
            self.client_push(self.focus, &[ch]);
        }
    }

    /// Enter: terminal commits the line to the read ring; clients get '\n'.
    fn press_enter(&mut self) {
        if self.focus == TERM_ID {
            self.append_text_to(TERM_ID, "\n");
            let n = self.line_len;
            let mut committed = [0u8; 121];
            committed[..n].copy_from_slice(&self.line_buf[..n]);
            committed[n] = b'\n';
            self.ring_push(&committed[..n + 1]);
            self.line_len = 0;
            self.flush_reader();
        } else {
            self.client_push(self.focus, &[b'\n']);
        }
    }

    /// Backspace: terminal drops the last typed char + its echo glyph.
    /// Client windows own their content: nothing to edit here.
    fn press_backspace(&mut self) {
        if self.focus != TERM_ID || self.line_len == 0 {
            return; // never eat past the prompt/current line start
        }
        self.line_len -= 1;
        // The terminal is index 0 only by convention — look it up, since it
        // may not exist at all (opens on demand, never at boot).
        let Some(ti) = self.idx_of(TERM_ID) else {
            return;
        };
        let cr = {
            let t = &mut self.windows[ti];
            if let Some(last) = t.lines.back_mut() {
                last.pop();
            }
            t.content_rect()
        };
        self.invalidate(cr);
    }

    // ---- terminal line discipline ----------------------------------------

    /// Append console text to window `id` (state only; invalidate follows).
    /// Terminal text with no terminal window is buffered in term_hist so
    /// early boot output survives until the user opens it.
    fn append_text_to(&mut self, id: u32, text: &str) {
        let Some(idx) = self.idx_of(id) else {
            if id == TERM_ID {
                push_text(&mut self.term_hist, text);
            }
            return;
        };
        push_text(&mut self.windows[idx].lines, text);
        let cr = self.windows[idx].content_rect();
        self.invalidate(cr);
    }

    /// Completed terminal input into the read ring (drops when full).
    fn ring_push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.in_len >= self.in_ring.len() {
                break;
            }
            let idx = (self.in_head + self.in_len) % self.in_ring.len();
            self.in_ring[idx] = b;
            self.in_len += 1;
        }
    }

    /// Hand buffered terminal input to a waiting reader, if any.
    fn flush_reader(&mut self) {
        let Some(to) = self.pending_reader.take() else {
            return;
        };
        if self.in_len == 0 {
            self.pending_reader = Some(to);
            return;
        }
        let n = self.in_len.min(MAX_PAYLOAD);
        let mut buf = [0u8; MAX_PAYLOAD];
        for i in 0..n {
            buf[i] = self.in_ring[(self.in_head + i) % self.in_ring.len()];
        }
        self.in_head = (self.in_head + n) % self.in_ring.len();
        self.in_len -= n;
        let mut m = Message::new(REPLY_OK);
        m.arg0 = n as u64;
        m.set_payload(&buf[..n]);
        ipc::send(to, m, sched::current_id().unwrap_or(u32::MAX));
    }

    // ---- client window input ----------------------------------------------

    fn client_push(&mut self, id: u32, bytes: &[u8]) {
        let Some(idx) = self.idx_of(id) else {
            return;
        };
        for &b in bytes {
            let win = &mut self.windows[idx];
            if win.len >= win.ring.len() {
                break; // slow reader: drop
            }
            let at = (win.head + win.len) % win.ring.len();
            win.ring[at] = b;
            win.len += 1;
        }
        self.flush_client(idx);
    }

    /// Hand a client window's buffered keys to its waiting reader.
    fn flush_client(&mut self, idx: usize) {
        let Some(to) = self.windows[idx].pending.take() else {
            return;
        };
        let win = &mut self.windows[idx];
        if win.len == 0 {
            win.pending = Some(to);
            return;
        }
        let n = win.len.min(MAX_PAYLOAD);
        let mut buf = [0u8; MAX_PAYLOAD];
        for i in 0..n {
            buf[i] = win.ring[(win.head + i) % win.ring.len()];
        }
        win.head = (win.head + n) % win.ring.len();
        win.len -= n;
        let mut m = Message::new(REPLY_OK);
        m.arg0 = n as u64;
        m.set_payload(&buf[..n]);
        ipc::send(to, m, sched::current_id().unwrap_or(u32::MAX));
    }

    fn reply(&self, to: u32, kind: u32, arg0: u64, payload: &[u8]) {
        let mut m = Message::new(kind);
        m.arg0 = arg0;
        m.set_payload(payload);
        ipc::send(to, m, sched::current_id().unwrap_or(u32::MAX));
    }

    /// Wallpaper ops (win.rs v1.1): the ring-3 wallpaper task streams RGB
    /// bands from its own decoded image; winsrv converts to XRGB + blits.
    /// Gate: the stamped sender must hold CAP_WALL (kernel-granted to the
    /// wallpaper task only — fuzz noise gets dropped at the gate).
    fn handle_wall(&mut self, msg: Message) {
        if sched::caps_of(msg.sender) & sched::caps::CAP_WALL == 0 {
            if msg.kind == WIN_WALL_SETUP {
                // SETUP replies (flow control); rows/done are fire-and-forget.
                self.reply(msg.sender, REPLY_ERR, 0, &[]);
            }
            return;
        }
        match msg.kind {
            WIN_WALL_SETUP => {
                let (tw, th) = (self.fb.width as u64, self.fb.height as u64);
                self.wall_msgs = 0;
                self.reply(msg.sender, REPLY_OK, tw | (th << 32), &[]);
                kprintln!(
                    "[winsrv] wallpaper: task {} decoded {}x{}, target {}x{}",
                    msg.sender, msg.arg0, msg.arg1, tw, th
                );
            }
            WIN_WALL_ROWS => {
                let w = self.fb.width as usize;
                let h = self.fb.height as usize;
                let px = msg.payload_bytes();
                let total = w * h;
                let mut base = msg.arg0 as usize;
                if base >= total || base + px.len() / 3 > total {
                    return; // out-of-bounds junk: drop silently
                }
                for c in px.chunks_exact(3) {
                    self.wallpaper[base] =
                        ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32;
                    base += 1;
                }
                let first = msg.arg0 as usize;
                let span = base - first; // pixels written in this message
                let rows = span / w + u32::from(span % w != 0) as usize;
                self.invalidate((0, (first / w) as u32, self.fb.width, rows as u32));
                self.wall_msgs += 1;
            }
            WIN_WALL_DONE => {
                let (tw, th) = (self.fb.width, self.fb.height);
                kprintln!(
                    "[winsrv] wallpaper: {}x{} streamed ({} msgs, userspace task {})",
                    tw, th, self.wall_msgs, msg.sender
                );
                self.invalidate((0, 0, tw, th));
            }
            _ => {}
        }
    }

    // ---- protocol ---------------------------------------------------------

    fn handle(&mut self, msg: Message) {
        match msg.kind {
            WIN_WRITE => {
                // Terminal text (DEV relays term0 writes): fire-and-forget.
                if let Ok(text) = core::str::from_utf8(msg.payload_bytes()) {
                    self.append_text_to(TERM_ID, text);
                }
            }
            WIN_READ => {
                // Canonical read: hold the request until Enter commits a
                // line. flush_reader replies the moment input lands.
                // GLS-006: only DEV (task 1) relays terminal reads. Anyone
                // else claiming this slot would steal the shell's next
                // typed line AND wedge dev's relay forever (it waits for a
                // reply that now goes to the thief). Drop silently: no
                // reply owed, nothing overwritten.
                if msg.sender != 1 {
                    return;
                }
                self.pending_reader = Some(msg.sender);
                self.flush_reader();
            }
            WIN_CREATE => {
                if self.windows.len() >= MAX_WINS {
                    self.reply(msg.sender, REPLY_ERR, 0, &[]);
                    return;
                }
                let title = match core::str::from_utf8(msg.payload_bytes()) {
                    Ok(t) if !t.is_empty() => String::from(t),
                    _ => String::from("Untitled"),
                };
                let (sw, sh) = (self.fb.width, self.fb.height);
                let w = 540u32.min(sw.saturating_sub(8));
                let h = 330u32.min(sh.saturating_sub(TASKBAR_H + 8));
                // Cascade down-right per existing client window.
                let k = self.nclients() as u32;
                let x = (sw / 2 + 60).min(sw.saturating_sub(w + 4));
                let y = (120 + 40 * k).min(sh.saturating_sub(TASKBAR_H + h + 4));
                let id = self.next_id;
                self.next_id += 1;
                self.windows.push(Window {
                    id,
                    // GLS-006: stamped creator owns this window. Only the
                    // owner may read its keys, write its text, or destroy
                    // it over IPC (the titlebar X uses the direct path).
                    owner: msg.sender,
                    x,
                    y,
                    w,
                    h,
                    born: idt::ticks(),
                    maxim: None,
                    title,
                    lines: VecDeque::new(),
                    canvas: None,
                    ring: [0; 128],
                    head: 0,
                    len: 0,
                    pending: None,
                });
                // New windows do NOT steal keyboard focus (a boot-time
                // client must not hijack the console); click to focus.
                self.invalidate(pad((x, y, w, h)));
                self.invalidate(strip_rect(sw, sh));
                kprintln!("[winsrv] win {} open at {},{} {}x{}", id, x, y, w, h);
                self.reply(msg.sender, REPLY_OK, id as u64, &[]);
            }
            WIN_CLIENT_WRITE => {
                let id = msg.arg0 as u32;
                // GLS-006: only the owning client may scribble here.
                // Fire-and-forget: nothing to reply, just drop it.
                match self.idx_of(id) {
                    Some(idx) if self.windows[idx].owner == msg.sender => {}
                    _ => return,
                }
                if let Ok(text) = core::str::from_utf8(msg.payload_bytes()) {
                    self.append_text_to(id, text);
                }
            }
            WIN_CLIENT_READ => {
                let id = msg.arg0 as u32;
                match self.idx_of(id) {
                    // GLS-006: only the owner may claim the key stream.
                    Some(idx) if self.windows[idx].owner == msg.sender => {
                        self.windows[idx].pending = Some(msg.sender);
                        self.flush_client(idx);
                    }
                    Some(_) => self.reply(msg.sender, REPLY_ERR, 0, &[]),
                    None => self.reply(msg.sender, REPLY_ERR, 0, &[]),
                }
            }
            WIN_DESTROY => {
                let id = msg.arg0 as u32;
                // IPC clients can't kill the terminal (only its own X
                // button can, via the direct destroy path in click()),
                // and GLS-006: only the owning client may destroy its
                // window over IPC.
                let owned = self.idx_of(id).is_some_and(|i| {
                    id != TERM_ID && self.windows[i].owner == msg.sender
                });
                if owned {
                    self.destroy_window(id);
                    self.reply(msg.sender, REPLY_OK, 0, &[]);
                } else {
                    self.reply(msg.sender, REPLY_ERR, 0, &[]);
                }
            }
            // Wallpaper service (v1.1): pixels streamed by the ring-3
            // wallpaper task. Caps-gated (CAP_WALL) like every other
            // dangerous surface — the kernel only converts + blits.
            WIN_WALL_SETUP | WIN_WALL_ROWS | WIN_WALL_DONE => {
                self.handle_wall(msg);
            }
            // M15 app graphics API: the owning client streams RGB runs
            // into its own window's canvas. Owner-checked (like
            // WIN_CLIENT_*), budget-capped; the kernel only converts +
            // blits — same trust shape as the wallpaper service.
            WIN_BLIT_SETUP | WIN_BLIT | WIN_BLIT_END => {
                self.handle_blit(msg);
            }
            _ => {}
        }
    }

    /// M15 canvas ops. SETUP allocates a canvas clamped to the content
    /// viewport (budget-capped across all windows; allocation failure
    /// answers REPLY_ERR instead of OOM-ing the server). BLIT lands one
    /// RGB run (owner-checked, bounds-checked, fire-and-forget). END
    /// frees the canvas and hands the window back to text mode with a
    /// synced REPLY_OK.
    fn handle_blit(&mut self, msg: Message) {
        let id = msg.arg0 as u32;
        match msg.kind {
            WIN_BLIT_SETUP => {
                // The content viewport is the upper bound for a canvas:
                // clamping here (instead of rejecting) lets a client ask
                // for "as big as my window" and trust the reply dims.
                let (cw_max, ch_max, cr0, cr1) = match self.idx_of(id) {
                    Some(i) if self.windows[i].owner == msg.sender => {
                        let c = self.windows[i].content_rect();
                        (c.2, c.3, c.0, c.1)
                    }
                    _ => {
                        self.reply(msg.sender, REPLY_ERR, 0, &[]);
                        return;
                    }
                };
                if cw_max == 0 || ch_max == 0 {
                    self.reply(msg.sender, REPLY_ERR, 0, &[]);
                    return;
                }
                let w = ((msg.arg1 & 0xFFFF_FFFF) as u32).clamp(1, cw_max);
                let h = ((msg.arg1 >> 32) as u32).clamp(1, ch_max);
                let n = w as u64 * h as u64;
                // Replace-or-allocate: a re-SETUP frees the old canvas
                // first so the budget never double-counts one window.
                if let Some((cv, _, _)) = self
                    .idx_of(id)
                    .and_then(|i| self.windows[i].canvas.as_ref())
                {
                    self.canvas_used -= cv.len() as u64;
                    if let Some(i) = self.idx_of(id) {
                        self.windows[i].canvas = None;
                    }
                }
                if self.canvas_used + n > CANVAS_BUDGET_PX {
                    self.reply(msg.sender, REPLY_ERR, 0, &[]);
                    return;
                }
                // Failure-proof alloc: try_reserve first, so a full or
                // fragmented heap answers REPLY_ERR instead of panicking
                // via the OOM handler INSIDE the compositor task.
                let mut pix: Vec<u32> = Vec::new();
                if pix.try_reserve_exact(n as usize).is_err() {
                    self.reply(msg.sender, REPLY_ERR, 0, &[]);
                    return;
                }
                pix.resize(n as usize, ((BG.0 as u32) << 16) | ((BG.1 as u32) << 8) | BG.2 as u32);
                let Some(i) = self.idx_of(id) else {
                    return;
                };
                self.windows[i].canvas = Some((pix, w, h));
                self.canvas_used += n;
                self.canvas_msgs = 0;
                // Fresh canvas: erase the viewport once (a re-SETUP may be
                // smaller than the previous canvas' damage rows).
                self.invalidate((cr0, cr1, cw_max, ch_max));
                kprintln!("[winsrv] canvas: win {} {}x{} (owner {})", id, w, h, msg.sender);
                self.reply(msg.sender, REPLY_OK, w as u64 | ((h as u64) << 32), &[]);
            }
            WIN_BLIT => {
                let Some(i) = self
                    .idx_of(id)
                    .filter(|&i| self.windows[i].owner == msg.sender)
                else {
                    return; // not the owner / gone: drop silently (no reply owed)
                };
                // Viewport first (immutable borrow ends at the copy), then
                // the canvas by &mut — the pixel run lands directly in the
                // cached canvas; the compose pass copies it out later.
                let cr = self.windows[i].content_rect();
                let Some((pix, cw, ch)) = self.windows[i].canvas.as_mut() else {
                    return; // text mode: no canvas to paint
                };
                let (cw, ch) = (*cw as usize, *ch as usize);
                let px = msg.payload_bytes();
                let run = px.len() / 3; // trailing partial pixels drop (chunks_exact)
                if run == 0 {
                    return;
                }
                let total = cw * ch;
                let base = msg.arg1 as usize;
                if base >= total || base + run > total {
                    return; // out-of-bounds junk: drop silently
                }
                // Convert + blit into the cached canvas (the compose pass
                // copies it later; the kernel never interprets pixels).
                for (k, c) in px[..run * 3].chunks_exact(3).enumerate() {
                    pix[base + k] = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32;
                }
                // Damage the affected ROW strip (run may wrap rows; the x
                // extent is the canvas width either way — cheap + correct).
                let row0 = base / cw;
                let row1 = (base + run - 1) / cw;
                let rows = (row1 - row0 + 1) as u32;
                let y = cr.1 + row0 as u32;
                // Columns clip to the viewport (a shrunken window may be
                // narrower than the canvas — never paint outside its body).
                let wvis = (cw as u32).min(cr.2);
                self.invalidate((cr.0, y, wvis, rows.min(ch as u32 - row0 as u32)));
                self.canvas_msgs += 1;
            }
            WIN_BLIT_END => {
                let owned = self
                    .idx_of(id)
                    .is_some_and(|i| self.windows[i].owner == msg.sender);
                if !owned {
                    self.reply(msg.sender, REPLY_ERR, 0, &[]);
                    return;
                }
                if let Some(i) = self.idx_of(id) {
                    if let Some((cv, cw, _)) = self.windows[i].canvas.take() {
                        self.canvas_used -= cv.len() as u64;
                        let cr = self.windows[i].content_rect();
                        self.invalidate((cr.0, cr.1, cw.min(cr.2), cr.3));
                    }
                }
                kprintln!(
                    "[winsrv] canvas: win {} released ({} msgs)",
                    id, self.canvas_msgs
                );
                self.reply(msg.sender, REPLY_OK, 0, &[]);
            }
            _ => {}
        }
    }
}

fn unpack(v: u32) -> Rgb {
    Rgb(((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8)
}

// ---- geometry shared by draw + hit-testing -------------------------------

/// Taskbar buttons: 0 = start, 1 = Files pin, 2 = Terminal pin,
/// 3 = Settings pin, then one button per running client (id order),
/// capped so the centered cluster can never reach the tray.
fn bar_cell_count(nclient: usize) -> u32 {
    4 + (nclient as u32).min(MAX_CLIENTS as u32)
}

/// Cluster width: (n-1) pitches + one button (no trailing gap).
fn bar_cluster_w(n: u32) -> u32 {
    (n - 1) * PITCH + CELL
}

/// Button `i` of the centered cluster (40px square, 4px air below the
/// bar's top line).
fn bar_cell_rect(i: u32, sw: u32, sh: u32, nclient: usize) -> Rect {
    let n = bar_cell_count(nclient);
    let x0 = (sw - bar_cluster_w(n)) / 2;
    (x0 + i * PITCH, sh - TASKBAR_H + 4, CELL, CELL)
}

/// The bar repaint rect (full width, flush bottom).
fn strip_rect(sw: u32, sh: u32) -> Rect {
    (0, sh - TASKBAR_H, sw, TASKBAR_H)
}

/// Start card: horizontally centered, floating MENU_GAP above the bar.
fn menu_rect(sw: u32, sh: u32) -> Rect {
    ((sw - MENU_W) / 2, sh - TASKBAR_H - MENU_GAP - MENU_H, MENU_W, MENU_H)
}

/// Pinned tile `gi` in the FILTERED grid (4 columns). Same walk as
/// draw_start's, so clicks and pixels always agree.
fn tile_rect(gi: u32, sw: u32, sh: u32) -> Rect {
    let (mx, my, _, _) = menu_rect(sw, sh);
    let col = gi % 4;
    let row = gi / 4;
    (
        mx + 16 + col * (TILE_W + 8),
        my + 88 + row * (TILE_H + 8),
        TILE_W,
        TILE_H,
    )
}

/// Footer power button (card-relative geometry, right side).
fn power_rect(sw: u32, sh: u32) -> Rect {
    let (mx, my, mw, _) = menu_rect(sw, sh);
    (mx + mw - 60, my + MENU_H - 54, 44, 40)
}

/// Right clock zone: the two-line wall clock (time over date,
/// right-aligned); the hover chip covers the whole zone.
fn tray_rect(sw: u32, sh: u32) -> Rect {
    (sw - TRAY_W - 12, sh - TASKBAR_H + 4, TRAY_W, CELL)
}

/// Console-rules append into a line deque (shared by window lines and the
/// buffered terminal history): a write continues the open line, only '\n'
/// breaks (treating every write as a fresh line would shred "$ help" into
/// two rows). Caps line count (SCROLLBACK) AND line length (MAX_LINE — the
/// open line is uncapped by the count, so without this any client could
/// grow one String until the heap dies).
fn push_text(lines: &mut VecDeque<String>, text: &str) {
    if lines.back().is_none() {
        lines.push_back(String::new());
    }
    let mut parts = text.split('\n');
    let head = parts.next().unwrap_or("");
    if !head.is_empty() {
        if let Some(last) = lines.back_mut() {
            let room = MAX_LINE.saturating_sub(last.len());
            last.push_str(&head[..head.len().min(room)]);
        }
    }
    for part in parts {
        let mut s = String::from(part);
        if s.len() > MAX_LINE {
            s.truncate(MAX_LINE);
        }
        lines.push_back(s);
        while lines.len() > SCROLLBACK {
            lines.pop_front();
        }
    }
}

// ---- wallpaper -----------------------------------------------------------

/// Placeholder wallpaper: a dark "bloom" gradient rendered ONCE at
/// takeover, until the ring-3 wallpaper task streams the real image
/// (assets/wallpapers/wallpaper.png, decoded in userspace) over IPC —
/// the kernel never parses image data. Matches the Windows 11 mood:
/// deep navy with a big blue bloom center-right and a faint purple glow
/// upper-left. All channels stay < 239 so the white mouse cursor remains
/// the only >=239 blob on screen (the input/tablet test predicate).
fn render_wallpaper(w: u32, h: u32) -> Vec<u32> {
    let mut out: Vec<u32> = alloc::vec![0u32; (w * h) as usize];
    let ww = w.max(1) as u64;
    let hh = h.max(1) as u64;
    // Per-column dx^2 tables (distances to both bloom centers, Q16).
    // Bloom A: deep blue, big, center. Bloom B: purple, small, upper
    // left. Base: vertical navy gradient.
    let (ax, ay) = (ww / 2, hh * 3 / 5);
    let (bx, by) = (ww / 4, hh / 3);
    let rad_a = ((ww + hh) / 3).max(1); // Q0 radii squared below
    let rad_b = ((ww + hh) / 5).max(1);
    for x in 0..w {
        let x64 = x as u64;
        let dxa = (ax as i64 - x64 as i64) * (ax as i64 - x64 as i64);
        let dxb = (bx as i64 - x64 as i64) * (bx as i64 - x64 as i64);
        for y in 0..h {
            let y64 = y as u64;
            // Vertical base: (14,15,26) -> (26,28,46), Q16 lerp.
            let t = ((y64 << 16) / hh) as u32;
            let mut r = (14 * 65536 + 12 * t) >> 16;
            let mut g = (15 * 65536 + 13 * t) >> 16;
            let mut b = (26 * 65536 + 20 * t) >> 16;
            // Radiance = max(0, 1 - d2/rad2), linear in d2 (no sqrt).
            let da = dxa + (ay as i64 - y64 as i64) * (ay as i64 - y64 as i64);
            let db = dxb + (by as i64 - y64 as i64) * (by as i64 - y64 as i64);
            let qa = if da < (rad_a * rad_a) as i64 {
                (1 << 12) - (da << 12) / ((rad_a * rad_a) as i64)
            } else {
                0
            } as u32;
            let qb = if db < (rad_b * rad_b) as i64 {
                (1 << 12) - (db << 12) / ((rad_b * rad_b) as i64)
            } else {
                0
            } as u32;
            // A adds deep blue light, B a faint purple glow.
            let ar = (qa * 26) >> 12;
            let ag = (qa * 78) >> 12;
            let ab = (qa * 146) >> 12;
            let br = (qb * 66) >> 12;
            let bg = (qb * 40) >> 12;
            let bb = (qb * 92) >> 12;
            r += ar + br;
            g += ag + bg;
            b += ab + bb;
            // Vignette: corners darken by up to 14% (distance-in-box est.).
            let cx64 = (x64.max(ww - x64)) as u32;
            let cy64 = (y64.max(hh - y64)) as u32;
            let vin = ((cx64 * cy64) >> 12).min(28) as u32;
            r = r.saturating_sub(vin / 2);
            g = g.saturating_sub(vin / 2);
            b = b.saturating_sub(vin / 2);
            out[(y * w + x) as usize] =
                ((r.min(238) as u32) << 16) | ((g.min(238) as u32) << 8) | b.min(238) as u32;
        }
    }
    out
}

/// Integer square root (Newton) — icon circles only, no libm in core.
fn isqrt(n: u32) -> u32 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

// ---- acrylic / shadow / rounded-fill engine ------------------------------

/// One blended pixel: wallpaper + tint at Q8 alpha (0..256). Scalar
/// fixed-point REFERENCE for the SSE2 blend kernel (simd.rs); the compositor
/// calls the vector path, tests keep this around for bit-exact comparison.
#[inline(always)]
fn blend_px(w: u32, tint: Rgb, a: u32) -> u32 {
    let wr = ((w >> 16) & 0xFF) as u32;
    let wg = ((w >> 8) & 0xFF) as u32;
    let wb = (w & 0xFF) as u32;
    let t = tint;
    let r = (wr * (256 - a) + t.0 as u32 * a) >> 8;
    let g = (wg * (256 - a) + t.1 as u32 * a) >> 8;
    let b = (wb * (256 - a) + t.2 as u32 * a) >> 8;
    (r << 16) | (g << 8) | b
}

/// Open-animation progress 0..=256 (integer Q8): 0 = just created,
/// 256 = settled. ANIM_TICKS scheduler ticks long.
fn anim_p(now: u64, born: u64) -> u32 {
    (now.saturating_sub(born) * 256 / ANIM_TICKS).min(256) as u32
}

/// Acrylic fill with rounded corners: blends the wallpaper (the true
/// backdrop beneath every panel) with `tint` at alpha `a`, clipped to the
/// damage rect `r`. Writes straight to the back buffer — compose() has
/// already stamped the wallpaper there, but the blend reads the WALL (not
/// the buffer) so panel pixels stay perfectly repeatable on partial
/// repaints: glass = tint*(a) + wallpaper*(1-a), always. rad 0 = square
/// (the taskbar uses that).
fn acrylic_round_clip(
    back: &mut Framebuffer,
    wall: &[u32],
    r: Rect,
    area: Rect,
    rad: u32,
    tint: Rgb,
    a: u32,
) {
    let (x0, y0, w, h) = area;
    if w == 0 || h == 0 || a == 0 {
        return;
    }
    let width = back.width;
    let stride = width as usize; // back buffer is packed width*4 (per blit)
    let base = back.base_mut() as *mut u32;
    let rad = rad.min(w / 2).min(h / 2) as u32;
    for row in 0..h {
        let dy: i32 = if row < rad {
            rad as i32 - row as i32
        } else if row >= h - rad {
            (row as i32) - (h as i32 - rad as i32 - 1)
        } else {
            0
        };
        let sq = (rad * rad) as i32 - dy * dy;
        let inset = if sq <= 0 { rad } else { rad - isqrt(sq as u32) };
        let vy = y0 + row;
        if vy < r.1 || vy >= r.1 + r.3 || r.2 == 0 {
            continue;
        }
        let mut xs = x0 + inset;
        let xe = x0 + w - inset;
        xs = xs.max(r.0);
        let xe = xe.min(r.0 + r.2);
        if xe <= xs {
            continue;
        }
        unsafe {
            let dst = base.add(vy as usize * stride + xs as usize);
            let src = wall.as_ptr().add(vy as usize * stride + xs as usize);
            // M13: SSE2 blend (4 px/iter), bit-exact vs blend_px.
            let tint_px = (tint.0 as u32) << 16 | (tint.1 as u32) << 8 | tint.2 as u32;
            simd::blend_const_span(src, dst, (xe - xs) as usize, tint_px, a);
        }
    }
}

/// Blend wallpaper OVER whatever is already composed in the back buffer
/// at `area` (wall alpha `a`): the open-animation scrim. Windows fade in
/// by dissolving into the desktop: out = back*(1-a) + wall*a.
fn scrim_rect_clip(back: &mut Framebuffer, wall: &[u32], r: Rect, area: Rect, a: u32) {
    if a == 0 {
        return;
    }
    let width = back.width;
    let stride = width as usize;
    let base = back.base_mut() as *mut u32;
    let (x0, y0, w, h) = area;
    let y_lo = y0.max(r.1);
    let y_hi = (y0 + h).min(r.1 + r.3);
    let x_lo = x0.max(r.0);
    let x_hi = (x0 + w).min(r.0 + r.2);
    if x_hi <= x_lo || y_hi <= y_lo {
        return;
    }
    for vy in y_lo..y_hi {
        unsafe {
            let dst = base.add(vy as usize * stride + x_lo as usize);
            let src = wall.as_ptr().add(vy as usize * stride + x_lo as usize);
            // M13: SSE2 blend (4 px/iter), bit-exact vs blend_px. The base
            // pixel is the already-composed back buffer (read before write,
            // same as the scalar loop).
            simd::blend_over_span(dst, src, (x_hi - x_lo) as usize, a);
        }
    }
}

/// Soft 4-ring drop shadow around `wr`, blended over whatever is already
/// composed (wallpaper OR lower windows). `scale` attenuates ring alpha
/// for the open animation. Each ring is 3px thick; clip scrims keep it
/// inside the screen and the damage rect.
fn window_shadow(back: &mut Framebuffer, wall: &[u32], r: Rect, wr: Rect, scale: u32) {
    if scale == 0 || wr.2 == 0 || wr.3 == 0 {
        return;
    }
    for band in 0..4u32 {
        let a = SHADOW_A[band as usize] * scale / 256;
        if a == 0 {
            continue;
        }
        let e = band * 3; // inner edge distance from the window rect
        let (top_x, top_y, top_w) = (
            wr.0.saturating_sub(e + 3),
            wr.1.saturating_sub(e + 3),
            wr.2 + 2 * (e + 3),
        );
        // Top and bottom full-width strips (corners included).
        scrim_rect_clip(back, wall, r, (top_x, top_y, top_w, 3), a);
        let bottom_y = wr.1.saturating_add(wr.3 + e);
        scrim_rect_clip(back, wall, r, (top_x, bottom_y, top_w, 3), a);
        // Left and right 3px columns (between the strips).
        let mid_y = wr.1.saturating_sub(e);
        let mid_h = wr.3 + 2 * e;
        scrim_rect_clip(back, wall, r, (top_x, mid_y, 3, mid_h), a);
        scrim_rect_clip(
            back,
            wall,
            r,
            (wr.0.wrapping_add(wr.2).saturating_add(e), mid_y, 3, mid_h),
            a,
        );
    }
}

fn fill_circle(back: &mut Framebuffer, cx: u32, cy: u32, r: u32, c: Rgb) {
    let rr = r as i32;
    for dy in -rr..=rr {
        let rest = (r * r) as i32 - dy * dy;
        if rest < 0 {
            continue;
        }
        let hw = isqrt(rest as u32) as i32;
        let y = cy as i32 + dy;
        if y < 0 {
            continue;
        }
        let x0 = (cx as i32 - hw).max(0);
        let x1 = cx as i32 + hw + 1;
        if x1 <= x0 {
            continue;
        }
        back.fill_rect(x0 as u32, y as u32, (x1 - x0) as u32, 1, c);
    }
}

/// Top-half annulus (wifi arcs): rows above (cx, cy), radii r_in..r_out.
/// Paints ONLY arc pixels — safe over acrylic (no bg punching).
fn fill_arc_top(back: &mut Framebuffer, cx: u32, cy: u32, r_out: u32, r_in: u32, c: Rgb) {
    if r_out <= r_in || cy < r_out {
        return;
    }
    let cxi = cx as i32;
    for dy in 1..=r_out as i32 {
        let rest = (r_out * r_out) as i32 - dy * dy;
        if rest < 0 {
            continue;
        }
        let hw_out = isqrt(rest as u32) as i32;
        let hw_in = if dy as i32 >= r_in as i32 {
            0
        } else {
            isqrt(((r_in * r_in) as i32 - dy * dy).max(0) as u32) as i32
        };
        let y = cy as i32 - dy;
        if y < 0 {
            continue;
        }
        for (a, b) in [(cxi - hw_out, cxi - hw_in), (cxi + hw_in, cxi + hw_out)] {
            let a = a.max(0) as u32;
            let b = b.max(0) as u32;
            if b > a {
                back.fill_rect(a, y as u32, b - a, 1, c);
            }
        }
    }
}

/// Bottom quarter-annulus rows below (cx, cy) on one side — used by the
/// browser ball icon's colored wedges.
fn fill_quarter_bottom(
    back: &mut Framebuffer,
    cx: u32,
    cy: u32,
    r_out: u32,
    r_in: u32,
    right: bool,
    c: Rgb,
) {
    if r_out <= r_in || cy >= back.height {
        return;
    }
    let cxi = cx as i32;
    for dy in 1..=r_out as i32 {
        let rest = (r_out * r_out) as i32 - dy * dy;
        if rest < 0 {
            continue;
        }
        let hw_out = isqrt(rest as u32) as i32;
        let hw_in = if dy >= r_in as i32 {
            0
        } else {
            isqrt(((r_in * r_in) as i32 - dy * dy).max(0) as u32) as i32
        };
        let y = cy as i32 + dy;
        if y < 0 {
            continue;
        }
        let (a, b) = if right {
            (cxi + hw_in, cxi + hw_out)
        } else {
            (cxi - hw_out + 1, cxi - hw_in)
        };
        let a = a.max(0) as u32;
        let b = b.max(0) as u32;
        if b > a {
            back.fill_rect(a, y as u32, b - a, 1, c);
        }
    }
}

/// Annulus ring around (cx, cy): full-circle outline-style mark, used for
/// the magnifier, gear, and power glyphs.
fn fill_ring(back: &mut Framebuffer, cx: u32, cy: u32, r_out: u32, r_in: u32, c: Rgb) {
    if r_out <= r_in {
        return;
    }
    let cxi = cx as i32;
    let cyi = cy as i32;
    for dy in 1..=r_out as i32 {
        let rest = (r_out * r_out) as i32 - dy * dy;
        if rest < 0 {
            continue;
        }
        let hw_out = isqrt(rest as u32) as i32;
        let hw_in = if dy as i32 >= r_in as i32 {
            0
        } else {
            isqrt(((r_in * r_in) as i32 - dy * dy).max(0) as u32) as i32
        };
        for y in [cyi - dy, cyi + dy] {
            if y < 0 {
                continue;
            }
            for (a, b) in [(cxi - hw_out, cxi - hw_in), (cxi + hw_in, cxi + hw_out)] {
                let a = a.max(0) as u32;
                let b = b.max(0) as u32;
                if b > a {
                    back.fill_rect(a, y as u32, b - a, 1, c);
                }
            }
        }
    }
}

/// Rounded-corner filled rect, clipped to the damage rect `r`: scanline
/// fill with quarter-circle insets (isqrt). Core primitive for every
/// rounded surface in the theme (windows, pills, tiles, buttons).
fn fill_round_clip(back: &mut Framebuffer, r: Rect, rect: Rect, rad: u32, c: Rgb) {
    let (x0, y0, w, h) = rect;
    if w == 0 || h == 0 {
        return;
    }
    let rad = rad.min(w / 2).min(h / 2) as i32;
    for row in 0..h {
        let rad_u = rad as u32;
        let dy: i32 = if row < rad_u {
            rad as i32 - row as i32 // rows 0..rad: grows to full shave at 0
        } else if row >= h - rad_u {
            (row as i32) - (h as i32 - rad as i32 - 1)
        } else {
            0 // middle of the rect: no inset
        };
        let sq = rad * rad - dy * dy;
        let inset = if sq <= 0 { rad } else { rad - isqrt(sq as u32) as i32 };
        let ry = y0 + row;
        if ry < r.1 || ry >= r.1 + r.3 {
            continue;
        }
        let x1 = x0 + inset as u32;
        let x2 = x0 + w - inset as u32;
        let rx0 = x1.max(r.0);
        let rx1 = x2.min(r.0 + r.2);
        if rx1 <= rx0 {
            continue;
        }
        back.fill_rect(rx0, ry, rx1 - rx0, 1, c);
    }
}

/// Rounded corners on the TOP rows only (window titlebar band above a
/// straight body edge).
fn fill_round_top_clip(back: &mut Framebuffer, r: Rect, rect: Rect, rad: u32, c: Rgb) {
    let (x0, y0, w, h) = rect;
    if w == 0 || h == 0 || h <= rad + 1 {
        fill_clip(back, r, rect, c);
        return;
    }
    let rad = rad.min(w / 2) as i32;
    for row in 0..h {
        let rad_u = rad as u32;
        let dy: i32 = if row < rad_u {
            rad as i32 - row as i32
        } else {
            0
        };
        let sq = rad * rad - dy * dy;
        let inset = if sq <= 0 { rad } else { rad - isqrt(sq as u32) as i32 };
        let ry = y0 + row;
        if ry < r.1 || ry >= r.1 + r.3 {
            continue;
        }
        let x1 = x0 + inset as u32;
        let x2 = x0 + w - inset as u32;
        let rx0 = x1.max(r.0);
        let rx1 = x2.min(r.0 + r.2);
        if rx1 <= rx0 {
            continue;
        }
        back.fill_rect(rx0, ry, rx1 - rx0, 1, c);
    }
}

/// Blend a rounded chip of `c` at Q8 alpha `a` over the ALREADY-composed
/// back buffer (rows clipped to the damage rect). Unlike the acrylic
/// helpers this does not punch through to the wallpaper cache — it layers
/// one tint step on top of whatever the surface has painted, which is
/// what hover states on buttons, tiles, and rows need.
fn chip_round_clip(back: &mut Framebuffer, r: Rect, rect: Rect, rad: u32, c: Rgb, a: u32) {
    let (x0, y0, w, h) = rect;
    if w == 0 || h == 0 || a == 0 {
        return;
    }
    let rad = rad.min(w / 2).min(h / 2) as i32;
    for row in 0..h {
        let rad_u = rad as u32;
        let dy: i32 = if row < rad_u {
            rad as i32 - row as i32
        } else if row >= h - rad_u {
            (row as i32) - (h as i32 - rad as i32 - 1)
        } else {
            0
        };
        let sq = rad * rad - dy * dy;
        let inset = if sq <= 0 { rad } else { rad - isqrt(sq as u32) as i32 };
        let ry = y0 + row;
        if ry < r.1 || ry >= r.1 + r.3 {
            continue;
        }
        let x1 = x0 + inset as u32;
        let x2 = x0 + w - inset as u32;
        let rx0 = x1.max(r.0);
        let rx1 = x2.min(r.0 + r.2);
        if rx1 <= rx0 {
            continue;
        }
        for x in rx0..rx1 {
            let px = unpack(blend_px(read_pixel(back, x, ry), c, a));
            back.put_pixel(x, ry, px);
        }
    }
}

// ---- drawing (compose layers) --------------------------------------------

/// Flat minimize/close glyph strokes.

/// Close glyph: two 9px diagonals crossing at (cx, cy), 2px thick.
fn draw_x_glyph(back: &mut Framebuffer, cx: u32, cy: u32, c: Rgb) {
    for t in -4i32..=4i32 {
        let x = (cx as i32 + t).max(0) as u32;
        let y1 = (cy as i32 + t).max(0) as u32;
        let y2 = (cy as i32 - t).max(0) as u32;
        back.put_pixel(x, y1, c);
        back.put_pixel(x, y2, c);
        if t < 4 {
            back.put_pixel(x, y1.saturating_sub(1), c);
            back.put_pixel(x, y2 + 1, c);
        }
    }
}

/// Maximize glyph: 9x8 hollow square (1px stroke).
fn draw_square_glyph(back: &mut Framebuffer, cx: u32, cy: u32, c: Rgb) {
    back.fill_rect(cx - 4, cy - 4, 9, 1, c);
    back.fill_rect(cx - 4, cy + 3, 9, 1, c);
    back.fill_rect(cx - 4, cy - 3, 1, 6, c);
    back.fill_rect(cx + 4, cy - 3, 1, 6, c);
}

/// Glyph-walking text with a right-edge clip: the next character that
/// would cross `limit` terminates the run (titles, search query).
fn draw_text_clip(
    fb: &mut Framebuffer,
    font: &mut FontSystem,
    x: u32,
    y: u32,
    s: &str,
    px: f32,
    fg: Rgb,
    limit: u32,
) {
    let mut cx = x;
    for ch in s.chars() {
        if ch == '\n' || ch == '\r' {
            continue;
        }
        let adv = font.char_advance(ch, px);
        if cx + adv > limit {
            break;
        }
        cx += font.draw_char_blended(fb, cx, y, ch, px, fg);
    }
}

/// Compose one window: 1px border + rounded silhouette (focused = lighter
/// border + lighter titlebar; maximized windows square their corners),
/// flat maximize/close buttons (close hovers red), left title text,
/// content lines, and the open-animation scrim (wallpaper dissolves in as
/// the window rises into place). The soft shadow is drawn by compose()
/// just before this.
#[allow(clippy::too_many_arguments)]
fn draw_window(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    wall: &[u32],
    win: &Window,
    focus: u32,
    r: Rect,
    line_h: u32,
    mx: u32,
    my: u32,
    now: u64,
) {
    let p = anim_p(now, win.born);
    let rise: u32 = (((256 - p) * ANIM_RISE as u32) >> 8) as u32;
    let (x, y, w, h) = (win.x, win.y + rise, win.w, win.h);
    let focused = win.id == focus;
    // Win11: maximized windows square their corners.
    let rad = if win.maxim.is_some() { 0 } else { RAD_WIN };
    // Border: silhouette fill, then the inner silhouette is overlaid by
    // the titlebar + body fills — leaves a 1px border ring.
    let border = if focused { FRAME } else { FRAME_UNFOC };
    fill_round_clip(back, r, (x, y, w, h), rad, border);
    // Body: full rounded fill under everything.
    fill_round_clip(back, r, (x + 1, y + 1, w - 2, h - 2), rad.saturating_sub(1), BG);
    // Titlebar band: rounded top corners only.
    let tbg = if focused { TITLE } else { TITLE_UNFOC };
    fill_round_top_clip(
        back,
        r,
        (x + 1, y + 1, w - 2, TITLE_H),
        rad.saturating_sub(1),
        tbg,
    );
    // Hairline under the titlebar.
    fill_clip(back, r, (x + 1, y + 1 + TITLE_H, w.saturating_sub(2), 1), HAIRLINE);
    // Flat window buttons: maximize + close (rounded hover tiles).
    if w >= 70 {
        let crect = (x + w - 26, y + 4, 20, 22);
        let mrect = (x + w - 46, y + 4, 20, 22);
        if point_in(mx, my, crect) {
            chip_round_clip(back, r, crect, 6, CLOSE_HOVER, 256);
        }
        if point_in(mx, my, mrect) {
            chip_round_clip(back, r, mrect, 6, BTN_HOVER, 256);
        }
        draw_x_glyph(back, x + w - 16, y + 15, ICON_FG);
        draw_square_glyph(back, x + w - 36, y + 15, ICON_FG);
    }
    // Title text, left-aligned, vertically centered (unfocused dims).
    let lh = font.line_height(12.0);
    draw_text_clip(
        back,
        font,
        x + 14,
        y + (TITLE_H - lh) / 2 + 1,
        &win.title,
        12.0,
        if focused { TITLE_FG } else { SUB_FG },
        x + w.saturating_sub(56),
    );
    // Content area: graphics mode blits the per-client canvas (M15),
    // text mode renders console lines. content_rect keeps its geometry
    // for hit-testing; the rise shifts only the drawing during the open
    // animation.
    let cr = win.content_rect();
    let crd = (cr.0, cr.1 + rise, cr.2, cr.3);
    if r_inter(r, crd) {
        match win.canvas.as_ref() {
            Some((pix, cw, ch)) => {
                blit_canvas(back, pix, *cw, *ch, crd, r);
            }
            None => draw_lines(back, font, win, crd, r, line_h),
        }
    }
    // Open fade: the whole rect dissolves out of the wallpaper as p -> 1.
    if p < 256 {
        scrim_rect_clip(back, wall, r, (x, y, w, h), 256 - p);
    }
}

/// Render a window's lines statelessly: wrap at the content width, show
/// the tail that fits (scrollback beyond it), draw only damaged rows.
fn draw_lines(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    win: &Window,
    cr: Rect,
    r: Rect,
    line_h: u32,
) {
    let cw = font.char_advance('M', FONT_PX).max(1);
    let cols = (cr.2 / cw).max(1) as usize;
    let rows_max = (cr.3 / line_h).max(1) as usize;
    let rows_of = |len: usize| len.div_ceil(cols).max(1);
    let total: usize = win.lines.iter().map(|l| rows_of(l.chars().count())).sum();
    let skip = total.saturating_sub(rows_max);
    let mut row = 0usize;
    for l in &win.lines {
        let lr = rows_of(l.chars().count());
        let first = row;
        row += lr;
        if row <= skip {
            continue; // scrolled out entirely
        }
        for k in 0..lr {
            let ri = first + k;
            if ri < skip {
                continue;
            }
            let vy = ri - skip;
            if vy >= rows_max {
                break;
            }
            let y0 = cr.1 + vy as u32 * line_h;
            if !r_inter(r, (cr.0, y0, cr.2, line_h)) {
                continue; // damage misses this row
            }
            let mut x = cr.0;
            let end = cr.0 + cr.2;
            for ch in l.chars().skip(k * cols).take(cols) {
                if x >= end {
                    break;
                }
                x += font.draw_char(back, x, y0, ch, FONT_PX, line_h, FG, BG);
            }
        }
    }
}

/// M15: copy a client canvas over the content viewport (crd = content
/// rect as drawn, rise shift included; r = damage). Opaque 1:1 pixel
/// copy — the canvas is anchored at the viewport's top-left, clipped to
/// the viewport (a shrunken window may be smaller; maximize just
/// reveals body BG around it). Same per-row shape as the desktop
/// wallpaper copy: M13 SSE2 span copy, integer-only.
fn blit_canvas(back: &mut Framebuffer, pix: &[u32], cw: u32, ch: u32, crd: Rect, r: Rect) {
    let stride = back.width as usize;
    let base = back.base_mut() as *mut u32;
    // Visible area: viewport ∩ canvas, then rows ∩ damage.
    let vis_w = cw.min(crd.2);
    let vis_h = ch.min(crd.3);
    let y_lo = crd.1.max(r.1);
    let y_hi = (crd.1 + vis_h).min(r.1 + r.3);
    let x0 = crd.0.max(r.0);
    let x1 = (crd.0 + vis_w).min(r.0 + r.2);
    if x1 <= x0 || y_hi <= y_lo {
        return;
    }
    if cw == 0 {
        return;
    }
    let cx = (x0 - crd.0) as usize; // first visible canvas column
    for sy in y_lo..y_hi {
        let cy = (sy - crd.1) as usize;
        // SAFETY: cy < vis_h <= ch, so cy*cw+cx stays inside pix; the
        // screen row sy and column x0..x1 are inside the back buffer
        // (the viewport lives inside the window, the window on screen).
        let src = unsafe { pix.as_ptr().add(cy * cw as usize + cx) };
        let dst = unsafe { base.add(sy as usize * stride + x0 as usize) };
        simd::copy_span(dst, src, (x1 - x0) as usize);
    }
}

/// Rounded running/focused indicator under a taskbar button: a 16x3
/// rounded pill (brand lime when focused, soft gray when running).
fn underline(back: &mut Framebuffer, r: Rect, cx: u32, y: u32, c: Rgb) {
    fill_round_clip(back, r, (cx - 8, y, 16, 3), 1, c);
}

/// Brand "G" launcher mark — the taskbar start chip's glyph, in the
/// brand lime. Font-rendered at a 22 px em (ink ≈16 px tall) and
/// centered horizontally; +2 px vertical optical nudge because capital
/// letters hang high in the line box. The size is chosen so the glyph's
/// ink ends by y≈782 — keeping the exact-LIME underline probe boxes
/// (y 786+) clean for the headless pixel tests (see qmp_desktop_test /
/// qmp_tablet_test launch_px asserts).
fn draw_start_g(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    cx: u32,
    cy: u32,
    c: Rgb,
) {
    let px = 22.0_f32;
    let adv = font.char_advance('G', px);
    let lh = font.line_height(px);
    let x = cx.saturating_sub(adv / 2);
    let top = cy.saturating_sub(lh / 2) + 2;
    draw_text_blended(back, font, x, top, "G", px, c);
}

/// Magnifier glyph: 9px ring + 2px-thick diagonal handle. Paints only its
/// own pixels — safe over the acrylic search field.
fn draw_glass(back: &mut Framebuffer, cx: u32, cy: u32, c: Rgb) {
    fill_ring(back, cx, cy, 4, 2, c);
    for i in 0..3u32 {
        back.put_pixel(cx + 4 + i, cy + 4 + i, c);
        back.put_pixel(cx + 5 + i, cy + 4 + i, c);
    }
}

/// Files folder glyph in an `s` box centered at (cx, cy): darker back
/// panel, lighter front sheet.
fn draw_folder(back: &mut Framebuffer, r: Rect, cx: u32, cy: u32, s: u32) {
    let half = s / 2;
    let rad = (s / 6).max(2);
    fill_round_clip(back, r, (cx - half, cy - half + 1, s, s * 2 / 3), rad, BLUE);
    fill_round_clip(
        back,
        r,
        (
            cx.saturating_sub(half + 1),
            cy - half + s * 3 / 10,
            s + 2,
            s * 6 / 11,
        ),
        rad,
        BLUE_HI,
    );
}

/// Terminal glyph in an `s` box: rounded dark square + ">_" prompt.
fn draw_term_icon(
    back: &mut Framebuffer,
    r: Rect,
    font: &mut FontSystem,
    cx: u32,
    cy: u32,
    s: u32,
) {
    let half = s / 2;
    fill_round_clip(back, r, (cx - half, cy - half, s, s), 3, TERM_SQ);
    let px = (s as f32) * 0.55;
    let total = font.char_advance('>', px) + font.char_advance('_', px);
    let lh = font.line_height(px);
    draw_text_blended(
        back,
        font,
        cx.saturating_sub(total / 2),
        cy.saturating_sub(lh / 2),
        ">_",
        px,
        WHITE,
    );
}

/// Settings gear in an `s` box: ring + 4 teeth + hub.
fn draw_gear(back: &mut Framebuffer, cx: u32, cy: u32, s: u32) {
    let r_out = s * 5 / 12; // ~10 for s=24
    let r_in = (s / 4).max(3); // ~6
    fill_ring(back, cx, cy, r_out, r_in, GEAR);
    let t = (s / 10).max(2); // tooth thickness
    let tooth = 4;
    back.fill_rect(
        cx.saturating_sub(t / 2),
        cy.saturating_sub(r_out + tooth),
        t,
        tooth + 2,
        GEAR,
    );
    back.fill_rect(cx.saturating_sub(t / 2), cy + r_in, t, tooth + 1, GEAR);
    back.fill_rect(
        cx.saturating_sub(r_out + tooth),
        cy.saturating_sub(t / 2),
        tooth + 2,
        t,
        GEAR,
    );
    back.fill_rect(cx + r_in, cy.saturating_sub(t / 2), tooth + 1, t, GEAR);
    fill_circle(back, cx, cy, r_in / 3 + 1, GEAR);
}

/// Demo-app ball glyph in an `s` box (the Demo pin): blue ball, red top
/// arc, green/yellow bottom wedges, white hub.
fn draw_ball(back: &mut Framebuffer, cx: u32, cy: u32, s: u32) {
    let rad = s * 5 / 12; // ~10 for s=24
    fill_circle(back, cx, cy, rad, BALL_BLU);
    fill_arc_top(back, cx, cy, rad, rad / 2, BALL_RED);
    fill_quarter_bottom(back, cx, cy, rad, rad / 2, true, BALL_GRN);
    fill_quarter_bottom(back, cx, cy, rad, rad / 2, false, BALL_YEL);
    fill_circle(back, cx, cy, rad * 3 / 10, BALL_WHITE);
    fill_circle(back, cx, cy, rad * 3 / 20, BALL_BLU);
}

/// Tile icon dispatch (28px box in start tiles).
fn draw_tile_icon(
    back: &mut Framebuffer,
    r: Rect,
    font: &mut FontSystem,
    app: App,
    cx: u32,
    cy: u32,
    s: u32,
) {
    match app {
        App::Settings => draw_gear(back, cx, cy, s),
        App::Demo => draw_ball(back, cx, cy, s),
        App::Files => draw_folder(back, r, cx, cy, s),
        App::Terminal => draw_term_icon(back, r, font, cx, cy, s),
    }
}

/// Power glyph: ring + vertical stroke poking out the top.
fn draw_power_glyph(back: &mut Framebuffer, cx: u32, cy: u32, c: Rgb) {
    fill_ring(back, cx, cy, 5, 3, c);
    back.fill_rect(cx.saturating_sub(1), cy.saturating_sub(9), 2, 7, c);
}

/// The Windows 11 taskbar: full-width acrylic bar (square corners, flush
/// to the bottom edge, hairline top), one CENTERED cluster of 40px
/// rounded buttons — start, Files/Terminal/Settings pins, then one per
/// running client — and the right clock zone (two-line wall clock only;
/// the wifi/volume/battery glyphs were removed). Open apps wear a
/// rounded underline under their button; hover lifts any button. Clicks route through the same bar_cell_rect
/// walk as here.
#[allow(clippy::too_many_arguments)]
fn draw_taskbar(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    wall: &[u32],
    windows: &[Window],
    next_id: u32,
    focus: u32,
    start_open: bool,
    secs: u64,
    r: Rect,
    sh: u32,
    mx: u32,
    my: u32,
) {
    let sw = back.width;
    let bar = (0, sh - TASKBAR_H, sw, TASKBAR_H);
    // Acrylic bar: full-rect blend over the wallpaper (rad 0 = square).
    acrylic_round_clip(back, wall, r, bar, 0, BAR_TINT, BAR_A);
    // Top hairline.
    fill_clip(back, r, (0, sh - TASKBAR_H, sw, 1), HAIRLINE);

    let nclient: usize = windows
        .iter()
        .filter(|w| w.id != TERM_ID && w.id != FILES_ID && w.id != SETTINGS_ID)
        .count();
    let files_open = windows.iter().any(|w| w.id == FILES_ID);
    let term_open = windows.iter().any(|w| w.id == TERM_ID);
    let set_open = windows.iter().any(|w| w.id == SETTINGS_ID);

    // ---- centered cluster ----
    let n = bar_cell_count(nclient);
    for i in 0..n {
        let c = bar_cell_rect(i, sw, sh, nclient);
        if !r_inter(r, c) {
            continue;
        }
        let cx = c.0 + CELL / 2;
        let cy = c.1 + 18; // glyph center sits high; the underline lives below
        let hover = point_in(mx, my, c);
        if hover {
            chip_round_clip(back, r, c, RAD_CELL, HOVER, 170);
        }
        match i {
            0 => {
                // Start button: brand-lime G mark; pressed tile + lime
                // underline while the start card is open.
                if start_open {
                    chip_round_clip(back, r, c, RAD_CELL, ACTIVE, 256);
                }
                draw_start_g(back, font, cx, cy, LIME);
                if start_open {
                    underline(back, r, cx, sh - 10, LIME);
                }
            }
            1 => {
                // Files pin: blue folder glyph.
                draw_folder(back, r, cx, cy, 24);
                if files_open {
                    underline(
                        back,
                        r,
                        cx,
                        sh - 10,
                        if focus == FILES_ID { LIME } else { UND_SOFT },
                    );
                }
            }
            2 => {
                // Terminal pin: dark square with the prompt glyph.
                draw_term_icon(back, r, font, cx, cy, 24);
                if term_open {
                    underline(
                        back,
                        r,
                        cx,
                        sh - 10,
                        if focus == TERM_ID { LIME } else { UND_SOFT },
                    );
                }
            }
            3 => {
                // Settings pin: gear glyph.
                draw_gear(back, cx, cy, 24);
                if set_open {
                    underline(
                        back,
                        r,
                        cx,
                        sh - 10,
                        if focus == SETTINGS_ID { LIME } else { UND_SOFT },
                    );
                }
            }
            k => {
                // nth running client (id ASC, same walk as click()):
                // mini rounded-window glyph with an azure title strip.
                let target = (k - 4) as usize;
                let mut seen = 0usize;
                let mut focused_client = false;
                for id in 0..next_id {
                    if id == TERM_ID || id == FILES_ID || id == SETTINGS_ID {
                        continue;
                    }
                    if !windows.iter().any(|w| w.id == id) {
                        continue; // destroyed window: no button, no gap
                    }
                    if seen == target {
                        focused_client = focus == id;
                        break;
                    }
                    seen += 1;
                }
                fill_round_clip(back, r, (cx - 9, cy - 8, 18, 15), 3, Rgb(62, 64, 72));
                fill_clip(back, r, (cx - 9, cy - 8, 18, 5), BLUE_HI);
                underline(
                    back,
                    r,
                    cx,
                    sh - 10,
                    if focused_client { LIME } else { UND_SOFT },
                );
            }
        }
    }

    // ---- right clock zone: two-line wall clock (icon glyphs removed;
    //      the hover chip is the whole zone) ----
    let tr = tray_rect(sw, sh);
    if r_inter(r, tr) {
        if point_in(mx, my, tr) {
            chip_round_clip(back, r, tr, RAD_CELL, HOVER, 170);
        }
        // Two-line clock, right-aligned: "HH:MM AM" over "M/D/YYYY".
        let minute = crate::drivers::rtc::now_minute(secs);
        let (hh, mm) = (minute / 60, minute % 60);
        let (h12, ap) = (
            if hh % 12 == 0 { 12 } else { hh % 12 },
            if hh < 12 { "AM" } else { "PM" },
        );
        let (dy, dm, dd) = crate::drivers::rtc::now_date(secs);
        let time_s = alloc::format!("{:02}:{:02} {}", h12, mm, ap);
        let date_s = alloc::format!("{}/{}/{}", dm, dd, dy);
        let tw: u32 = time_s.chars().map(|c| font.char_advance(c, 11.0)).sum();
        draw_text_blended(
            back,
            font,
            tr.0 + TRAY_W - 10 - tw,
            tr.1 + 4,
            &time_s,
            11.0,
            WHITE,
        );
        let dw: u32 = date_s.chars().map(|c| font.char_advance(c, 8.0)).sum();
        draw_text_blended(
            back,
            font,
            tr.0 + TRAY_W - 10 - dw,
            tr.1 + 22,
            &date_s,
            8.0,
            SUB_FG,
        );
    }
}

/// Windows 11 start menu: a centered acrylic card floating above the bar
/// — search field on top (typing live-filters the pinned grid), a 4-wide
/// pinned-tile grid, and a footer with a user chip (lime avatar) and a
/// power button. Fades in (alpha ramp) + rises with the open animation.
#[allow(clippy::too_many_arguments)]
fn draw_start(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    wall: &[u32],
    r: Rect,
    sh: u32,
    now: u64,
    menu_tick: u64,
    search: &[u8],
    mx: u32,
    my: u32,
) {
    let sw = back.width;
    let (px0, py0, pw, ph) = menu_rect(sw, sh);
    let p = anim_p(now, menu_tick);
    let rise = (((256 - p) * ANIM_RISE as u32) >> 8) as u32;
    let py = py0 + rise;
    // Acrylic card: soft shadow + hairline border + wallpaper-blended
    // interior whose opacity ramps with the open animation.
    window_shadow(back, wall, r, (px0, py, pw, ph), p);
    fill_round_clip(back, r, (px0, py, pw, ph), RAD_PANEL, PANEL_BORDER);
    acrylic_round_clip(
        back,
        wall,
        r,
        (px0 + 1, py + 1, pw - 2, ph - 2),
        RAD_PANEL - 1,
        PANEL_TINT,
        PANEL_A * p / 256,
    );
    // Search field on top.
    let sb = (px0 + 16, py + 16, pw - 32, 36);
    if r_inter(r, sb) {
        fill_round_clip(back, r, sb, 18, SEARCH_BG);
        draw_glass(back, sb.0 + 18, sb.1 + 18, FOOT_FG);
        let utf8 = core::str::from_utf8(search)
            .map(|q| q.chars().all(|c| c >= ' ' && c != '\u{7f}'))
            .unwrap_or(false);
        let (label, colored) = if search.is_empty() || !utf8 {
            // Fuzz-injected control bytes have no glyphs — fall
            // back to the placeholder instead of tofu boxes.
            ("Search", false)
        } else {
            (core::str::from_utf8(search).unwrap_or_default(), true)
        };
        draw_text_clip(
            back,
            font,
            sb.0 + 32,
            sb.1 + 12,
            label,
            10.0,
            if colored { LABEL_FG } else { SUB_FG },
            sb.0 + sb.2 - 12,
        );
    }
    // "Pinned" caption.
    if r_inter(r, (px0, py + 56, pw, 20)) {
        draw_text_blended(back, font, px0 + 18, py + 62, "Pinned", 10.0, SUB_FG);
    }
    let ta = (p * 2).min(256); // tiles fade in slightly ahead of the card
    // Tiles (filtered; identical walk to the click handler).
    let mut gi = 0u32;
    for (name, app) in APPS.iter() {
        if !name_matches(name, search) {
            continue;
        }
        let t0 = tile_rect(gi, sw, sh);
        gi += 1;
        let t = (t0.0, t0.1 + rise, t0.2, t0.3);
        if !r_inter(r, t) {
            continue;
        }
        if ta == 256 && point_in(mx, my, t) {
            chip_round_clip(back, r, t, RAD_TILE, HOVER, 256);
        } else if ta < 256 {
            // Blend-in mirroring the card's alpha ramp (over the card,
            // not through to the wallpaper).
            chip_round_clip(back, r, t, RAD_TILE, PANEL_TINT, ta / 8);
        }
        if ta >= 256 {
            let (tcx, tcy) = (t.0 + TILE_W / 2, t.1 + 40);
            draw_tile_icon(back, r, font, *app, tcx, tcy, 28);
            let wpx: u32 = name.chars().map(|c| font.char_advance(c, 10.0)).sum();
            draw_text_blended(back, font, tcx - wpx / 2, t.1 + 66, name, 10.0, LABEL_FG);
        }
    }
    // Footer: separator, user chip (lime avatar + name), power button.
    fill_clip(back, r, (px0 + 16, py + MENU_H - 68, pw - 32, 1), HAIRLINE);
    if ta >= 160 && r_inter(r, (px0, py + MENU_H - 68, pw, 68)) {
        let fcy = py + MENU_H - 34;
        // User chip.
        fill_circle(back, px0 + 40, fcy, 12, LIME);
        let gadv = font.char_advance('G', 10.0);
        draw_text_blended(
            back,
            font,
            px0 + 40 - gadv / 2,
            fcy.saturating_sub(6),
            "G",
            10.0,
            AVATAR_INK,
        );
        draw_text_blended(back, font, px0 + 62, fcy.saturating_sub(6), "GreenLime", 11.0, LABEL_FG);
        // Power button (hover tile + glyph).
        let pb0 = power_rect(sw, sh);
        let pb = (pb0.0, pb0.1 + rise, pb0.2, pb0.3);
        if point_in(mx, my, pb) {
            chip_round_clip(back, r, pb, RAD_TILE, HOVER, 256);
        }
        draw_power_glyph(back, pb.0 + 22, fcy, FOOT_FG);
    }
}

/// PS/2 set-1 make code -> ASCII (US layout, shift/caps aware).
fn scancode_ascii(sc: u8, shift: bool, caps: bool) -> Option<u8> {
    let ch: u8 = match (sc, shift) {
        // Digit row: shift gives the top-row symbols.
        (0x02, false) => b'1',
        (0x02, true) => b'!',
        (0x03, false) => b'2',
        (0x03, true) => b'@',
        (0x04, false) => b'3',
        (0x04, true) => b'#',
        (0x05, false) => b'4',
        (0x05, true) => b'$',
        (0x06, false) => b'5',
        (0x06, true) => b'%',
        (0x07, false) => b'6',
        (0x07, true) => b'^',
        (0x08, false) => b'7',
        (0x08, true) => b'&',
        (0x09, false) => b'8',
        (0x09, true) => b'*',
        (0x0A, false) => b'9',
        (0x0A, true) => b'(',
        (0x0B, false) => b'0',
        (0x0B, true) => b')',
        (0x0C, false) => b'-',
        (0x0C, true) => b'_',
        (0x0D, false) => b'=',
        (0x0D, true) => b'+',
        // Punctuation rows.
        (0x1A, false) => b'[',
        (0x1A, true) => b'{',
        (0x1B, false) => b']',
        (0x1B, true) => b'}',
        (0x27, false) => b';',
        (0x27, true) => b':',
        (0x28, false) => b'\'',
        (0x28, true) => b'"',
        (0x29, false) => b'`',
        (0x29, true) => b'~',
        (0x2B, false) => b'\\',
        (0x2B, true) => b'|',
        (0x33, false) => b',',
        (0x33, true) => b'<',
        (0x34, false) => b'.',
        (0x34, true) => b'>',
        (0x35, false) => b'/',
        (0x35, true) => b'?',
        (0x37, _) => b'*', // keypad
        // Letter rows (case applied below).
        (0x10, _) => b'q',
        (0x11, _) => b'w',
        (0x12, _) => b'e',
        (0x13, _) => b'r',
        (0x14, _) => b't',
        (0x15, _) => b'y',
        (0x16, _) => b'u',
        (0x17, _) => b'i',
        (0x18, _) => b'o',
        (0x19, _) => b'p',
        (0x1E, _) => b'a',
        (0x1F, _) => b's',
        (0x20, _) => b'd',
        (0x21, _) => b'f',
        (0x22, _) => b'g',
        (0x23, _) => b'h',
        (0x24, _) => b'j',
        (0x25, _) => b'k',
        (0x26, _) => b'l',
        (0x2C, _) => b'z',
        (0x2D, _) => b'x',
        (0x2E, _) => b'c',
        (0x2F, _) => b'v',
        (0x30, _) => b'b',
        (0x31, _) => b'n',
        (0x32, _) => b'm',
        // Misc: space, Enter, Tab (one space keeps parsing simple).
        (0x39, _) => b' ',
        (0x1C, _) => b'\n',
        (0x0F, _) => b' ',
        _ => return None,
    };
    if ch.is_ascii_lowercase() && (shift ^ caps) {
        return Some(ch.to_ascii_uppercase());
    }
    Some(ch)
}

fn read_pixel(fb: &mut Framebuffer, x: u32, y: u32) -> u32 {
    // Reuse put path in reverse: volatile read of the XRGB word.
    let stride = (fb.pitch() / 4) as usize;
    unsafe {
        let base = fb.base_mut() as *const u32;
        core::ptr::read_volatile(base.add(y as usize * stride + x as usize))
    }
}

/// Opaque-cell-free text (tiles, icons over acrylic, hover backgrounds).
fn draw_text_blended(
    fb: &mut Framebuffer,
    font: &mut FontSystem,
    mut x: u32,
    y: u32,
    s: &str,
    px: f32,
    fg: Rgb,
) {
    for ch in s.chars() {
        if ch == '\n' || ch == '\r' {
            continue;
        }
        x += font.draw_char_blended(fb, x, y, ch, px, fg);
    }
}

pub fn init() {
    kprintln!("[servers] winsrv: desktop compositor (takes over fb)");
}

/// M13 one-shot probes from winsrv's task context.
/// 1. XMM roundtrip: sentinels in XMM0–15, then yield — other tasks run and
///    would trash the registers if the switch did not FXSAVE/FXRSTOR.
/// 2. SSE2 blend: bit-exact check vs the scalar `blend_px` reference, plus
///    cycle timings (RDTSC) for scalar vs vector over the same work.
fn m13_probes() {
    extern "C" fn yield_cb() {
        sched::yield_now();
    }
    let bad = simd::fx_roundtrip_probe(yield_cb);
    if bad == 0 {
        kprintln!("[m13] xmm: 256/256 sentinel bytes survived a task switch");
    } else {
        kprintln!("[m13] xmm: FAIL — {}/256 sentinel bytes changed", bad);
    }

    const N: usize = 1280 * 16; // full-width strip, desktop-shaped span
    const A: u32 = 214; // the tray acrylic alpha (Q8)
    const ITERS: u32 = 20;
    let tint = Rgb(0x2B, 0x34, 0x46);
    let tint_px = (tint.0 as u32) << 16 | (tint.1 as u32) << 8 | tint.2 as u32;
    let mut src: Vec<u32> = Vec::with_capacity(N);
    let mut refb = alloc::vec![0u32; N];
    let mut out = alloc::vec![0u32; N];
    for i in 0..N {
        // Non-uniform pattern: catches lane-shuffle mistakes a flat fill hides.
        src.push((i as u32).wrapping_mul(2654435761) & 0x00FF_FFFF);
    }
    for i in 0..N {
        refb[i] = blend_px(src[i], tint, A);
    }
    simd::blend_const_span(src.as_ptr(), out.as_mut_ptr(), N, tint_px, A);
    let mism = refb
        .iter()
        .zip(out.iter())
        .filter(|(a, b)| a != b)
        .count();
    if mism == 0 {
        kprintln!("[m13] simd: SSE2 blend bit-exact on {} px", N);
    } else {
        kprintln!("[m13] simd: FAIL — {} px differ from scalar", mism);
    }
    // Throughput: identical spans, identical iteration count.
    let t0 = simd::rdtsc();
    for _ in 0..ITERS {
        for i in 0..N {
            refb[i] = blend_px(src[i], tint, A);
        }
    }
    let t_scalar = simd::rdtsc() - t0;
    let t0 = simd::rdtsc();
    for _ in 0..ITERS {
        simd::blend_const_span(src.as_ptr(), out.as_mut_ptr(), N, tint_px, A);
    }
    let t_simd = simd::rdtsc() - t0;
    // Fixed-point speedup x10 (kernel has no float codegen, keep it integer).
    let x10 = if t_simd > 0 { t_scalar * 10 / t_simd } else { 0 };
    kprintln!(
        "[m13] simd: {} px/it — scalar {} Mcyc vs sse2 {} Mcyc -> {}.{}x",
        N,
        t_scalar / 1_000_000,
        t_simd / 1_000_000,
        x10 / 10,
        x10 % 10
    );
}

/// Kernel task entry: own the screen, serve WIN_* forever.
pub unsafe extern "C" fn server_main() -> ! {
    {
        let mut guard = SRV.lock();
        match WinSrv::take_over() {
            Some(srv) => *guard = Some(srv),
            None => {
                drop(guard);
                kprintln!("[servers] winsrv: no fb, parking");
                loop {
                    let m = ipc::recv(PORT_WINSRV);
                    // Readers and creators must never wedge: answer err.
                    if matches!(
                        m.kind,
                        WIN_READ | WIN_CREATE | WIN_CLIENT_READ | WIN_DESTROY | WIN_BLIT_SETUP
                    ) {
                        let r = Message::new(REPLY_ERR);
                        ipc::send(
                            m.sender,
                            r,
                            sched::current_id().unwrap_or(u32::MAX),
                        );
                    }
                }
            }
        }
    }
    {
        let mut guard = SRV.lock();
        let srv = guard.as_mut().expect("[servers] winsrv vanished");
        // First paint: full-screen dirty from take_over -> compose + blit.
        srv.present();
        srv.draw_cursor();
        // Test anchor: serial sees this the instant the splash is covered
        // (boot-splash assertions must land before it).
        kprintln!("[winsrv] desktop painted");
    }
    // M13 proofs (one-shot, from real task context): XMM survives a task
    // switch, and the SSE2 blend path is bit-exact vs the scalar reference.
    m13_probes();
    loop {
        // Fast idle path: no messages, no input, no damage, clock fresh ->
        // touch nothing (zero framebuffer traffic, zero allocator churn).
        // Tablet events bypass the PS/2 rings (device DMA), so they get
        // their own gate — a cheap RAM read of the used index.
        let idle = !ipc::has_pending(PORT_WINSRV)
            && !input::has_pending()
            && !virtio_input::events_pending()
            && !WinSrv::work_due();
        if idle {
            sched::yield_now();
            continue;
        }
        // Lap: message/input damage is composed and blitted, and the
        // on-screen cursor stays STEADY unless its own pixels are touched
        // today — see steady_cursor (the old blanket undraw at lap start
        // left the cursor invisible for every full drain+present lap,
        // which flickers visibly under a message flood like the M15
        // canvas stream).
        if let Some(srv) = SRV.lock().as_mut() {
            while let Some(m) = ipc::try_recv(PORT_WINSRV) {
                // Hold the guard across handle (single task, no nesting).
                srv.handle(m);
            }
            srv.poll_input();
            srv.tick_taskbar();
            srv.tick_anim(); // keeps fades running lap to lap
            srv.steady_cursor(); // undraw only if present() would stomp it
            srv.present(); // compose damage into back buffer + blit
            srv.draw_cursor(); // cursor on top (fresh save-under)
        }
        // Cooperative yield every lap: keeps latency low, never wedges.
        sched::yield_now();
    }
}
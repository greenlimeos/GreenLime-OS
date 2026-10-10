//! Window server (kernel task, v0.8): owns the screen after fb takeover.
//! Damage-rect compositor over a back buffer: state changes invalidate
//! rects; each lap composes ONLY the damage into the back buffer (layers
//! in z-order: wallpaper desktop, windows, taskbar, start menu, popup)
//! and blits it. The on-screen mouse cursor never lives in the back
//! buffer: it is undrawn before present and redrawn on top after, so
//! compositing can never stomp it.
//!
//! UI theme (v0.9 rework): dark neutral, GreenLime's own look — procedural
//! blue wallpaper, left-aligned taskbar (G start mark, magnifier, window
//! icons with accent underline, right uptime tray), left-docked launcher
//! (search filter, app rows, recent), Settings/About popups. Window
//! content stays text-only: terminal (id 0, line discipline, opens on
//! demand from the launcher — never at boot) + client
//! windows (WIN_CREATE, raw keys while focused), focus follows clicks,
//! titlebar drag moves, corner grip resizes, taskbar icons focus/raise.
//! Keys arrive via PS/2 or virtio-tablet pointer; while the Start menu
//! is open keys filter its search instead. Hybrid note: in-kernel like
//! vfs/dev were at first; migrates out with shared-memory windows later.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use greenlime_abi::ipc::{Message, PORT_WINSRV, MAX_PAYLOAD, REPLY_ERR, REPLY_OK};
use greenlime_abi::win::*;
use greenlime_font::{FontSystem, Framebuffer, Rgb};
use spin::Mutex;

use crate::arch::x86_64::idt;
use crate::drivers::{fb, input, virtio_input};
use crate::{ipc, kprintln, sched};

const FONT_PX: f32 = 16.0;
// ---- Palette: dark neutral + blue accent (no green) -----------
const BG: Rgb = Rgb(16, 18, 24); // window body
const FG: Rgb = Rgb(214, 222, 235); // window text
const TITLE_BG: Rgb = Rgb(38, 44, 58); // focused titlebar
const TITLE_BG2: Rgb = Rgb(26, 30, 40); // unfocused titlebar
const TITLE_FG: Rgb = Rgb(232, 238, 246); // r < 239: cursor tests need that
const FRAME: Rgb = Rgb(60, 68, 86);
const CLOSE_BG: Rgb = Rgb(36, 40, 52);
const CLOSE_HOVER: Rgb = Rgb(196, 43, 28);
const TASKBAR_BG: Rgb = Rgb(23, 26, 34);
const TASKBAR_FG: Rgb = Rgb(170, 180, 195);
const HOVER: Rgb = Rgb(46, 52, 66);
const ACCENT: Rgb = Rgb(76, 194, 255); // blue accent
const ACCENT_DIM: Rgb = Rgb(30, 84, 120);
const MENU_BG: Rgb = Rgb(24, 27, 35); // start panel (tests exact-count it)
const MENU_BG2: Rgb = Rgb(34, 38, 49); // launcher rows / search / popup body
const MENU_FG: Rgb = Rgb(230, 236, 244);
const SUB_FG: Rgb = Rgb(130, 140, 155); // hints / dim labels
const POPUP_BG: Rgb = Rgb(20, 23, 30); // menu footer band
const DESK_FG: Rgb = Rgb(140, 155, 180); // watermark over wallpaper
const CURSOR: Rgb = Rgb(240, 240, 240); // tests detect >= 239 in ALL channels
const ICON_BG: Rgb = Rgb(36, 40, 52); // icon boxes

const TITLE_H: u32 = 30;
const TASKBAR_H: u32 = 48;
const SLOT: u32 = 40; // taskbar icon slot
const SLOT_GAP: u32 = 8;
const SCROLLBACK: usize = 200;
const CUR_W: u32 = 12;
const CUR_H: u32 = 14;
// Desktop layout (1280x800): terminal is a window, not fullscreen.
const TERM_X0: u32 = 150;
const TERM_Y0: u32 = 100;
const TERM_X1: u32 = 1130;
const TERM_Y1: u32 = 640;
// Start menu: left-docked launcher panel above the taskbar (GreenLime's
// own layout — a vertical list, not a centered grid).
const MENU_W: u32 = 400;
const MENU_H: u32 = 540;
/// App row geometry (relative to the panel top).
const ROW_Y0: u32 = 92;
const ROW_H: u32 = 50;
const ROW_PITCH: u32 = 58;

const TERM_ID: u32 = 0;
/// No window focused (clean desktop: terminal opens on demand, never at boot).
const NO_FOCUS: u32 = u32::MAX;
/// Longest single terminal line (heap guard: the open line is uncapped by
/// SCROLLBACK, which only limits the line *count*).
const MAX_LINE: usize = 4096;
const MAX_WINS: usize = 8;
const GRIP: u32 = 16;
const MIN_W: u32 = 200;
const MIN_H: u32 = 120;

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

/// Start-menu apps (pinned row + search filter over the same list).
#[derive(Clone, Copy, PartialEq, Eq)]
enum App {
    Terminal,
    Demo,
    Settings,
    About,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Terminal => "Terminal",
            App::Demo => "Demo",
            App::Settings => "Settings",
            App::About => "About",
        }
    }
}

const APPS: [(&str, App); 4] = [
    ("Terminal", App::Terminal),
    ("Demo", App::Demo),
    ("Settings", App::Settings),
    ("About", App::About),
];

/// Popup dialog kind (top layer, click anywhere dismisses).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Popup {
    About,
    Settings,
}

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
struct Window {
    id: u32,
    client: bool, // false = the terminal (system window, no X button)
    /// Creating task (kernel-stamped sender at WIN_CREATE; u32::MAX for
    /// the terminal). Client ops require sender == owner (GLS-006).
    owner: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    title: String,
    /// Text content (console rules; wrapped at render time).
    lines: VecDeque<String>,
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
    fn content_rect(&self) -> Rect {
        (
            self.x + 8,
            self.y + TITLE_H + 8,
            self.w.saturating_sub(16),
            self.h.saturating_sub(TITLE_H + 16),
        )
    }
    /// Close button (top-right of the titlebar, client windows only).
    fn close_rect(&self) -> Rect {
        (self.x + self.w - 30, self.y + 6, 22, 18)
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
    /// Packed XRGB wallpaper, rendered ONCE at takeover (see
    /// render_wallpaper): compose memcpy's rows out of it.
    wallpaper: Vec<u32>,
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
    // Start menu / popup.
    menu_open: bool,
    popup: Option<Popup>,
    search: [u8; 32],
    search_len: usize,
    recent: Vec<App>, // last launched, most recent first (max 3)
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
        // opens on demand from the launcher; the demo client opens its own
        // window when it starts. Early terminal text is buffered in
        // term_hist until then (serial mirror stays live regardless).
        let mut srv = Self {
            fb,
            back_buf,
            back,
            wallpaper,
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
            menu_open: false,
            popup: None,
            search: [0; 32],
            search_len: 0,
            recent: Vec::new(),
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
            next_id: 1,
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
            unsafe { core::ptr::copy_nonoverlapping(src, dst, r.2 as usize) };
        }
    }

    /// Draw every layer that intersects `r`, bottom-to-top. Writes may
    /// spill outside `r` (harmless: only `r` is blitted, and any later
    /// compose recomputes its rect from state anyway).
    fn compose(&mut self, r: Rect) {
        let (sw, sh) = (self.fb.width, self.fb.height);
        // 1. Desktop: cached wallpaper rows (integer-rendered once at
        //    takeover — this target has no SSE, so per-pixel f32 here
        //    would be soft-float builtin calls: ~350s at -O0).
        let q = r_clip(r, (0, 0, sw, sh));
        if q.2 > 0 && q.3 > 0 {
            let stride = (self.back.pitch() / 4) as usize;
            let back_base = self.back.base_mut() as *mut u32;
            let wall = self.wallpaper.as_ptr();
            for y in q.1..q.1 + q.3 {
                let src = unsafe { wall.add(y as usize * stride + q.0 as usize) };
                let dst = unsafe { back_base.add(y as usize * stride + q.0 as usize) };
                unsafe { core::ptr::copy_nonoverlapping(src, dst, q.2 as usize) };
            }
        }
        // Watermark (blended, no solid cell box over the gradient).
        let wm = (sw.saturating_sub(300), sh.saturating_sub(TASKBAR_H + 34), 300, 24);
        if r_inter(r, wm) {
            draw_text_blended(
                &mut self.back,
                &mut self.font,
                wm.0,
                wm.1,
                "GreenLime OS v0.9",
                FONT_PX,
                DESK_FG,
            );
        }
        // 2. Windows, bottom-to-top (z-order).
        let n = self.windows.len();
        for i in 0..n {
            if r_inter(r, self.windows[i].rect()) {
                let focus = self.focus;
                let (mx, my) = (self.mx, self.my);
                draw_window(
                    &mut self.back,
                    &mut self.font,
                    &self.windows[i],
                    focus,
                    r,
                    self.line_h,
                    mx,
                    my,
                );
            }
        }
        // 3. Taskbar (left-aligned icon slots + right tray).
        let tb = (0u32, sh.saturating_sub(TASKBAR_H), sw, TASKBAR_H);
        if r_inter(r, tb) {
            let secs = idt::ticks() / 100;
            draw_taskbar(
                &mut self.back,
                &mut self.font,
                &self.windows,
                self.next_id,
                self.focus,
                self.menu_open,
                secs,
                r,
                sh,
                self.mx,
                self.my,
            );
            self.last_clock = secs;
        }
        // 4. Start menu.
        if self.menu_open {
            let m = menu_rect(sw, sh);
            if r_inter(r, m) {
                draw_menu(
                    &mut self.back,
                    &mut self.font,
                    r,
                    sw,
                    sh,
                    &self.search[..self.search_len],
                    &self.recent,
                    self.mx,
                    self.my,
                );
            }
        }
        // 5. Popup (top layer).
        if let Some(kind) = self.popup {
            let p = popup_rect(sw, sh);
            if r_inter(r, p) {
                let nwin = self.windows.len();
                draw_popup(&mut self.back, &mut self.font, r, sw, sh, kind, nwin);
            }
        }
    }

    /// The taskbar is stale when the clock ticks.
    fn tick_taskbar(&mut self) {
        let secs = idt::ticks() / 100;
        if secs != self.last_clock {
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate((0, sh - TASKBAR_H, sw, TASKBAR_H));
        }
    }

    fn work_due() -> bool {
        match SRV.lock().as_ref() {
            Some(s) => s.dirty.is_some() || idt::ticks() / 100 != s.last_clock,
            None => false,
        }
    }

    // ---- windows ----------------------------------------------------------

    fn idx_of(&self, id: u32) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    /// Focus + raise (click routing; the focused titlebar brightens).
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
            // Raise changes pixels only inside the raised window.
            self.invalidate(rect);
        }
        self.focus = id;
        if old_focus != id {
            if let Some(j) = self.idx_of(old_focus) {
                let t = self.windows[j].title_rect();
                self.invalidate(t);
            }
            // Taskbar accent underline follows focus.
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate((0, sh - TASKBAR_H, sw, TASKBAR_H));
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
        let rect = self.windows[idx].rect();
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
        self.invalidate((0, sh - TASKBAR_H, sw, TASKBAR_H));
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
            self.invalidate(old);
            self.invalidate(new);
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
        for row in 0..CUR_H as usize {
            for col in 0..CUR_W as usize {
                let x = self.mx + col as u32;
                let y = self.my + row as u32;
                if x < self.fb.width && y < self.fb.height {
                    self.mouse_buf[row * CUR_W as usize + col] = read_pixel(&mut self.fb, x, y);
                    if CURSOR_BITS[row] & (1 << (11 - col)) != 0 {
                        self.fb.put_pixel(x, y, CURSOR);
                    }
                }
            }
        }
        self.mouse_drawn = true;
    }

    // ---- input ------------------------------------------------------------

    /// Unified pointer event (PS/2 deltas resolve here too): press edge =
    /// click, held+motion = drag, release = drop.
    fn pointer(&mut self, x: u32, y: u32, btn: u8) {
        let moved = (x, y) != (self.mx, self.my);
        let (ox, oy) = (self.mx, self.my);
        self.mx = x;
        self.my = y;
        if moved {
            self.hover_invalidate(ox, oy, x, y);
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

    /// Hover feedback: entering/leaving the taskbar or an open menu panel
    /// marks them stale (icons/rows highlight under the cursor).
    fn hover_invalidate(&mut self, ox: u32, oy: u32, nx: u32, ny: u32) {
        let (sw, sh) = (self.fb.width, self.fb.height);
        let ty = sh - TASKBAR_H;
        if oy >= ty || ny >= ty {
            self.invalidate((0, ty, sw, TASKBAR_H));
        }
        if self.menu_open {
            let m = menu_rect(sw, sh);
            if point_in(ox, oy, m) || point_in(nx, ny, m) {
                self.invalidate(m);
            }
        }
    }

    /// Left-click edge handling. Coordinates in screen pixels.
    fn click(&mut self, x: u32, y: u32) {
        let (sw, sh) = (self.fb.width, self.fb.height);
        // Popup: any click dismisses (top layer).
        if self.popup.is_some() {
            self.popup = None;
            self.invalidate(popup_rect(sw, sh));
            return;
        }
        // Taskbar slots (usable with the menu open too).
        if y >= sh - TASKBAR_H {
            let n = slot_count(self.windows.len());
            for i in 0..n {
                let sx = slot_x(i, n, sw);
                if x >= sx && x < sx + SLOT {
                    self.taskbar_slot(i);
                    return;
                }
            }
            self.close_menu(); // empty taskbar space closes the menu
            return;
        }
        // Menu open: panel interactions, else dismiss (click consumed).
        if self.menu_open {
            let (mx0, my0, mw, mh) = menu_rect(sw, sh);
            // App rows (filtered order must match draw_menu).
            let mut row = 0u32;
            for (name, app) in APPS {
                if !name_matches(name, &self.search[..self.search_len]) {
                    continue;
                }
                let t = row_rect(row, sw, sh);
                if point_in(x, y, t) {
                    self.menu_launch(app);
                    return;
                }
                row += 1;
            }
            if point_in(x, y, (mx0, my0, mw, mh)) {
                return; // inside panel (incl. search box): keep it open
            }
            self.close_menu();
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
            // Close button (every window has one now, terminal included).
            {
                let c = self.windows[i].close_rect();
                if x >= c.0 && x < c.0 + c.2 && y >= c.1 && y < c.1 + c.3 {
                    self.destroy_window(id);
                    return;
                }
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
            // Titlebar starts a move; the body just focuses.
            let tb = self.windows[i].title_rect();
            let in_title = x >= tb.0 && x < tb.0 + tb.2 && y >= tb.1 && y < tb.1 + tb.3;
            let (dx, dy) = if in_title { (x - wx, y - wy) } else { (0, 0) };
            self.focus_window(id);
            if in_title {
                self.drag = Some(Drag {
                    id,
                    dx,
                    dy,
                    resize: false,
                });
            }
            return;
        }
        // Desktop click: nothing.
    }

    /// Taskbar slot i: 0 = Start, 1.. = window (id order).
    fn taskbar_slot(&mut self, i: u32) {
        let (sw, sh) = (self.fb.width, self.fb.height);
        let open = |s: &mut Self| {
            s.menu_open = true;
            let (sw, sh) = (s.fb.width, s.fb.height);
            s.invalidate(menu_rect(sw, sh));
            s.invalidate((0, sh - TASKBAR_H, sw, TASKBAR_H));
        };
        match i {
            0 => {
                if self.menu_open {
                    self.close_menu();
                } else {
                    open(self);
                }
            }
            1.. => {
                self.close_menu();
                let target = (i - 1) as usize;
                let mut k = 0;
                for id in 0..self.next_id {
                    if self.idx_of(id).is_none() {
                        continue; // destroyed window: no slot, no gap
                    }
                    if k == target {
                        self.focus_window(id);
                        return;
                    }
                    k += 1;
                }
                let _ = (sw, sh);
            }
        }
    }

    fn close_menu(&mut self) {
        if !self.menu_open {
            return;
        }
        self.menu_open = false;
        self.search_len = 0;
        let (sw, sh) = (self.fb.width, self.fb.height);
        self.invalidate(menu_rect(sw, sh));
        self.invalidate((0, sh - TASKBAR_H, sw, TASKBAR_H));
    }

    /// Launch a Start-menu app: record it in Recent, close the menu,
    /// then perform its action.
    fn menu_launch(&mut self, app: App) {        self.recent.retain(|a| *a != app);
        self.recent.insert(0, app);
        self.recent.truncate(3);
        self.close_menu();
        let (sw, sh) = (self.fb.width, self.fb.height);
        match app {
            App::Terminal => self.ensure_terminal(),
            App::Demo => {
                let demo = self.windows.iter().find(|w| w.client).map(|w| w.id);
                if let Some(id) = demo {
                    self.focus_window(id);
                }
            }
            App::Settings => {
                self.popup = Some(Popup::Settings);
                self.invalidate(popup_rect(sw, sh));
            }
            App::About => {
                self.popup = Some(Popup::About);
                self.invalidate(popup_rect(sw, sh));
            }
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
                client: false,
                owner: u32::MAX, // system window: no IPC client owns it
                x: TERM_X0,
                y: TERM_Y0,
                w: TERM_X1 - TERM_X0,
                h: TERM_Y1 - TERM_Y0,
                title: String::from("Terminal - greenlime-shell"),
                lines,
                ring: [0; 128],
                head: 0,
                len: 0,
                pending: None,
            });
            kprintln!("[winsrv] terminal open");
            self.invalidate((TERM_X0, TERM_Y0, TERM_X1 - TERM_X0, TERM_Y1 - TERM_Y0));
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate((0, sh - TASKBAR_H, sw, TASKBAR_H));
        }
        self.focus_window(TERM_ID);
    }

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
    /// pointer exactly — same unified flow as the PS/2 path.
    fn poll_tablet(&mut self) -> bool {
        let Some((x, y, btn)) = virtio_input::poll() else {
            return false;
        };
        let (w, h) = (self.fb.width as u64, self.fb.height as u64);
        let nx = (x as u64 * w.saturating_sub(1) / 32767) as u32;
        let ny = (y as u64 * h.saturating_sub(1) / 32767) as u32;
        self.pointer(nx, ny, btn & 1);
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
                // Esc closes an open Start menu.
                if self.menu_open {
                    self.close_menu();
                }
                return;
            }
            0x1C => {
                if self.menu_open {
                    self.menu_enter();
                } else {
                    self.press_enter();
                }
                return;
            }
            0x0E => {
                if self.menu_open {
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
        if self.menu_open {
            // Start menu grabs typing: filter the app list.
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
                    _ => String::from("Window"),
                };
                let (sw, sh) = (self.fb.width, self.fb.height);
                let w = 540u32.min(sw.saturating_sub(8));
                let h = 330u32.min(sh.saturating_sub(TASKBAR_H + 8));
                // Cascade down-right per existing client window.
                let k = self.windows.iter().filter(|w| w.client).count() as u32;
                let x = (sw / 2 + 60).min(sw.saturating_sub(w + 4));
                let y = (120 + 40 * k).min(sh.saturating_sub(TASKBAR_H + h + 4));
                let id = self.next_id;
                self.next_id += 1;
                self.windows.push(Window {
                    id,
                    client: true,
                    // GLS-006: stamped creator owns this window. Only the
                    // owner may read its keys, write its text, or destroy
                    // it over IPC (the titlebar X uses the direct path).
                    owner: msg.sender,
                    x,
                    y,
                    w,
                    h,
                    title,
                    lines: VecDeque::new(),
                    ring: [0; 128],
                    head: 0,
                    len: 0,
                    pending: None,
                });
                // New windows do NOT steal keyboard focus (a boot-time
                // client must not hijack the console); click to focus.
                self.invalidate((x, y, w, h));
                self.invalidate((0, sh - TASKBAR_H, sw, TASKBAR_H));
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
            _ => {}
        }
    }
}

fn unpack(v: u32) -> Rgb {
    Rgb(((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8)
}

// ---- geometry shared by draw + hit-testing -------------------------------

/// Taskbar slot count: Start + one per window.
fn slot_count(nwin: usize) -> u32 {
    1 + nwin as u32
}

/// Left edge of slot `i`: slots run left-to-right from the screen edge
/// (GreenLime taskbar is left-aligned, not centered).
fn slot_x(i: u32, _n: u32, _sw: u32) -> u32 {
    8 + i * (SLOT + SLOT_GAP)
}

/// Start menu: left-docked launcher above the taskbar.
fn menu_rect(_sw: u32, sh: u32) -> Rect {
    (
        8,
        sh - TASKBAR_H - 16 - MENU_H,
        MENU_W,
        MENU_H,
    )
}

fn search_rect(_sw: u32, sh: u32) -> Rect {
    let (mx0, my0, _, _) = menu_rect(_sw, sh);
    (mx0 + 16, my0 + 16, MENU_W - 32, 40)
}

/// App row `i` (index in the *filtered* app list): full-width row with
/// icon + name, not a tile.
fn row_rect(i: u32, _sw: u32, sh: u32) -> Rect {
    let (mx0, my0, _, _) = menu_rect(_sw, sh);
    (mx0 + 16, my0 + ROW_Y0 + i * ROW_PITCH, MENU_W - 32, ROW_H)
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

fn popup_rect(sw: u32, sh: u32) -> Rect {
    let (pw, ph) = (460u32, 210u32);
    ((sw - pw) / 2, (sh - ph) / 2, pw, ph)
}

// ---- wallpaper -----------------------------------------------------------

/// Render the wallpaper ONCE as packed XRGB, using Q16 fixed-point integer
/// math. This target is soft-float (no SSE on x86_64-unknown-none): a
/// per-pixel f32 loop compiles to compiler_builtins soft-float calls and
/// took ~350s at -O0 — it stalled compose, which stalled dev->winsrv
/// sends, which stalled every userland client. Integer ops are native.
///
/// Visual: deep-blue vertical gradient + horizontal tint + 4 soft radial
/// glows ((1-d²)² falloff) — soft bloom vibe. The blue channel stays
/// >= 42 everywhere so the exact-count menu color (24,27,35) and any
/// cursor-like pixel (>=239 in all channels) can never occur here.
fn render_wallpaper(w: u32, h: u32) -> Vec<u32> {
    // (cx, cy, radius, dr, dg, db)
    const GLOWS: [(i32, i32, i32, u32, u32, u32); 4] = [
        (320, 200, 600, 15, 35, 90),
        (1050, 650, 700, 25, 55, 110),
        (900, 150, 450, 8, 40, 85),
        (640, 480, 520, 12, 30, 70),
    ];
    let mut out: Vec<u32> = alloc::vec![0u32; (w * h) as usize];
    if w == 0 || h == 0 {
        return out;
    }
    // Horizontal tint Q16, computed once per column (w divisions total).
    let tx: Vec<u32> = (0..w).map(|x| (((x as u64) << 16) / w as u64) as u32).collect();
    for y in 0..h {
        let t_q16 = (((y as u64) << 16) / h as u64) as u32; // Q16 vertical factor
        // Base gradient in Q16 (row constant).
        let br = 10 * 65536 + 8 * t_q16;
        let bg = 16 * 65536 + 22 * t_q16;
        let bb = 42 * 65536 + 42 * t_q16;
        // dy² per glow, hoisted out of the pixel loop.
        let fy = y as i32;
        let dy2: [u32; 4] = core::array::from_fn(|i| {
            let dy = fy - GLOWS[i].1;
            (dy * dy) as u32
        });
        let row = (y * w) as usize;
        for x in 0..w {
            let mut r = br + 3 * tx[x as usize];
            let mut g = bg + 2 * tx[x as usize];
            let mut b = bb;
            let fx = x as i32;
            for (gi, (cx, _cy, rad, dr, dg, db)) in GLOWS.iter().enumerate() {
                let dx = fx - cx;
                let dsum = (dx * dx) as u32 + dy2[gi];
                let rad2 = (*rad * *rad) as u32;
                if dsum < rad2 {
                    // inv = 1 - d2 in Q16; f = inv² in Q16.
                    let inv = (((rad2 - dsum) as u64) << 16) / rad2 as u64;
                    let f = ((inv * inv) >> 16) as u32;
                    r += dr * f;
                    g += dg * f;
                    b += db * f;
                }
            }
            out[row + x as usize] =
                ((r >> 16).min(255) << 16) | (((g >> 16).min(255)) << 8) | (b >> 16).min(255);
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

/// Magnifier glyph: ring + diagonal handle.
fn draw_magnifier(back: &mut Framebuffer, cx: u32, cy: u32, fg: Rgb, bg: Rgb) {
    fill_circle(back, cx, cy, 7, fg);
    fill_circle(back, cx, cy, 5, bg);
    for i in 0..4 {
        back.fill_rect(cx + 5 + i, cy + 5 + i, 2, 2, fg);
    }
}

/// Start mark: a bold "G" for GreenLime — our own mark.
fn draw_start_mark(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    sx: u32,
    sy: u32,
) {
    let s = 22.0f32;
    let tw = font.char_advance('G', s);
    draw_text_blended(back, font, sx + (SLOT - tw) / 2, sy + 8, "G", s, ACCENT);
}

/// 40x40 app icon at (x, y): glyph only, no box (rows draw their own bg).
fn draw_app_icon(back: &mut Framebuffer, font: &mut FontSystem, app: App, x: u32, y: u32) {
    match app {
        App::Terminal => {
            let s = 32.0f32;
            let tw = font.char_advance('>', s) + font.char_advance('_', s);
            draw_text_blended(back, font, x + (40 - tw) / 2, y + 6, ">_", s, ACCENT);
        }
        App::Demo => {
            // Mini window: body + accent title strip.
            back.fill_rect(x, y + 4, 40, 30, ICON_BG);
            back.fill_rect(x, y + 4, 40, 8, ACCENT_DIM);
            back.fill_rect(x + 5, y + 18, 30, 2, SUB_FG);
            back.fill_rect(x + 5, y + 24, 20, 2, SUB_FG);
        }
        App::Settings => {
            // Sliders glyph.
            for (i, knob) in [(0u32, 6u32), (1, 24), (2, 14)] {
                let yy = y + 6 + i * 12;
                back.fill_rect(x, yy + 3, 40, 2, Rgb(120, 130, 145));
                back.fill_rect(x + knob, yy, 7, 8, ACCENT);
            }
        }
        App::About => {
            fill_circle(back, x + 20, y + 18, 16, ICON_BG);
            draw_text_blended(back, font, x + 17, y + 8, "i", 24.0, ACCENT);
        }
    }
}

// ---- drawing (compose layers) --------------------------------------------

/// Compose one window: frame, titlebar (bright when focused, accent
/// underline), close button (client; red on hover), content lines.
fn draw_window(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    win: &Window,
    focus: u32,
    r: Rect,
    line_h: u32,
    mx: u32,
    my: u32,
) {
    let (x, y, w, h) = (win.x, win.y, win.w, win.h);
    // Frame.
    fill_clip(back, r, (x, y, w, 2), FRAME);
    fill_clip(back, r, (x, y + h - 2, w, 2), FRAME);
    fill_clip(back, r, (x, y, 2, h), FRAME);
    fill_clip(back, r, (x + w - 2, y, 2, h), FRAME);
    // Titlebar.
    let focused = win.id == focus;
    let tbg = if focused { TITLE_BG } else { TITLE_BG2 };
    let tb = win.title_rect();
    fill_clip(back, r, tb, tbg);
    if focused {
        // Accent underline marks the active window.
        fill_clip(back, r, (tb.0, tb.1 + tb.3 - 2, tb.2, 2), ACCENT);
    }
    if r_inter(r, tb) {
        draw_text(back, font, x + 12, y + 7, &win.title, FONT_PX, TITLE_FG, tbg);
    }
    // Close button (every window, terminal included): neutral, red while
    // hovered. The terminal is a system window but closable — the shell
    // keeps running and it reopens from the launcher with buffered text.
    {
        let c = win.close_rect();
        let hover = point_in(mx, my, c);
        let cbg = if hover { CLOSE_HOVER } else { CLOSE_BG };
        fill_clip(back, r, c, cbg);
        if r_inter(r, c) {
            draw_text(back, font, c.0 + 6, c.1 + 1, "X", FONT_PX, TITLE_FG, cbg);
        }
    }
    // Content.
    let cr = win.content_rect();
    fill_clip(back, r, cr, BG);
    if r_inter(r, cr) {
        draw_lines(back, font, win, cr, r, line_h);
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

/// Taskbar (GreenLime style): left-aligned icon slots — G start mark,
/// magnifier, one window icon per app (focused = accent underline) —
/// hover fills, right-aligned two-line tray (uptime clock).
#[allow(clippy::too_many_arguments)]
fn draw_taskbar(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    windows: &[Window],
    next_id: u32,
    focus: u32,
    menu_open: bool,
    secs: u64,
    r: Rect,
    sh: u32,
    mx: u32,
    my: u32,
) {
    let sw = back.width;
    let ty = sh - TASKBAR_H;
    fill_clip(back, r, (0, ty, sw, TASKBAR_H), TASKBAR_BG);
    fill_clip(back, r, (0, ty, sw, 1), Rgb(46, 52, 68)); // top hairline
    let n = slot_count(windows.len());
    let sy = ty + (TASKBAR_H - SLOT) / 2;
    for i in 0..n {
        let sx = slot_x(i, n, sw);
        if !r_inter(r, (sx, sy, SLOT, SLOT)) {
            continue;
        }
        let hover = point_in(mx, my, (sx, sy, SLOT, SLOT));
        let active = i == 0 && menu_open;
        let slot_bg = if active {
            Rgb(40, 48, 66)
        } else if hover {
            HOVER
        } else {
            TASKBAR_BG
        };
        if slot_bg != TASKBAR_BG {
            fill_clip(back, r, (sx, sy, SLOT, SLOT), slot_bg);
        }
        match i {
            0 => draw_start_mark(back, font, sx, sy),
            _ => {
                // nth window in id order (same mapping as click handling).
                let target = (i - 1) as usize;
                let mut k = 0;
                let mut wid = None;
                for id in 0..next_id {
                    if !windows.iter().any(|w| w.id == id) {
                        continue;
                    }
                    if k == target {
                        wid = Some(id);
                        break;
                    }
                    k += 1;
                }
                if let Some(id) = wid {
                    if id == TERM_ID {
                        fill_clip(back, r, (sx + 8, sy + 8, 24, 24), ICON_BG);
                        draw_text_blended(back, font, sx + 11, sy + 13, ">_", 14.0, ACCENT);
                    } else {
                        fill_clip(back, r, (sx + 8, sy + 9, 24, 22), ICON_BG);
                        fill_clip(back, r, (sx + 8, sy + 9, 24, 5), ACCENT_DIM);
                    }
                    if id == focus {
                        // Accent underline under the running app.
                        fill_clip(back, r, (sx + 10, sy + 35, 20, 3), ACCENT);
                    }
                }
            }
        }
    }
    // Right tray: two-line uptime clock.
    let (h, m, d) = (secs / 3600, (secs % 3600) / 60, secs / 86400);
    let t1 = alloc::format!("{:02}:{:02}", h, m);
    let t2 = alloc::format!("Day {}", d);
    let tw1: u32 = t1.chars().map(|c| font.char_advance(c, FONT_PX)).sum();
    if r_inter(r, (sw - tw1 - 16, ty, tw1, 24)) {
        draw_text(
            back,
            font,
            sw - tw1 - 16,
            ty + 5,
            &t1,
            FONT_PX,
            Rgb(225, 230, 240),
            TASKBAR_BG,
        );
    }
    let px2 = 12.0f32;
    let tw2: u32 = t2.chars().map(|c| font.char_advance(c, px2)).sum();
    if r_inter(r, (sw - tw2 - 16, ty + 26, tw2, 18)) {
        draw_text(
            back,
            font,
            sw - tw2 - 16,
            ty + 27,
            &t2,
            px2,
            TASKBAR_FG,
            TASKBAR_BG,
        );
    }
}

/// Start menu (left-docked launcher): search box, app rows (filtered),
/// Recent list, footer.
#[allow(clippy::too_many_arguments)]
fn draw_menu(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    r: Rect,
    sw: u32,
    sh: u32,
    search: &[u8],
    _recent: &[App],
    mx: u32,
    my: u32,
) {
    let (mx0, my0, mw, mh) = menu_rect(sw, sh);
    // Panel.
    fill_clip(back, r, (mx0, my0, mw, mh), MENU_BG);
    fill_clip(back, r, (mx0, my0, mw, 1), Rgb(70, 78, 96)); // top hairline
    // Search box.
    let sb = search_rect(sw, sh);
    if r_inter(r, sb) {
        fill_clip(back, r, sb, MENU_BG2);
        fill_clip(back, r, (sb.0, sb.1 + sb.3 - 2, sb.2, 2), ACCENT);
        draw_magnifier(back, sb.0 + 18, sb.1 + 20, Rgb(210, 218, 230), MENU_BG2);
        if search.is_empty() {
            draw_text(
                back,
                font,
                sb.0 + 36,
                sb.1 + 10,
                "Type here to search",
                FONT_PX,
                SUB_FG,
                MENU_BG2,
            );
        } else if let Ok(q) = core::str::from_utf8(search) {
            draw_text_blended(back, font, sb.0 + 36, sb.1 + 10, q, FONT_PX, MENU_FG);
        }
    }
    // "Apps" header.
    if r_inter(r, (mx0 + 16, my0 + 64, 220, 24)) {
        draw_text(
            back,
            font,
            mx0 + 16,
            my0 + 64,
            "Apps",
            FONT_PX,
            MENU_FG,
            MENU_BG,
        );
    }
    // App rows (filtered by the search query, contiguous indices): icon +
    // name + chevron. Same order as the click handler's row_rect walk.
    let mut row = 0u32;
    for (name, app) in APPS {
        if !name_matches(name, search) {
            continue;
        }
        let t = row_rect(row, sw, sh);
        row += 1;
        if !r_inter(r, t) {
            continue;
        }
        let hover = point_in(mx, my, t);
        let tbg = if hover { HOVER } else { MENU_BG2 };
        fill_clip(back, r, t, tbg);
        draw_app_icon(back, font, app, t.0 + 6, t.1 + 5);
        draw_text(
            back,
            font,
            t.0 + 56,
            t.1 + 15,
            name,
            FONT_PX,
            MENU_FG,
            tbg,
        );
        draw_text(back, font, t.0 + t.2 - 24, t.1 + 15, ">", FONT_PX, SUB_FG, tbg);
    }
    // Separator + footer (account row).
    fill_clip(back, r, (mx0 + 16, my0 + 472, mw - 32, 1), Rgb(52, 58, 74));
    let foot = (mx0, my0 + 476, mw, mh - 476);
    if r_inter(r, foot) {
        fill_clip(back, r, foot, POPUP_BG);
        fill_circle(back, mx0 + 44, my0 + 512, 14, Rgb(50, 60, 84));
        draw_text_blended(back, font, mx0 + 37, my0 + 501, "G", 18.0, ACCENT);
        draw_text(
            back,
            font,
            mx0 + 72,
            my0 + 498,
            "GreenLime",
            FONT_PX,
            MENU_FG,
            POPUP_BG,
        );
        draw_text(
            back,
            font,
            mx0 + 72,
            my0 + 518,
            "Local account",
            12.0,
            SUB_FG,
            POPUP_BG,
        );
    }
}

/// Centered dialog (About / Settings), top layer: any click dismisses.
fn draw_popup(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    r: Rect,
    sw: u32,
    sh: u32,
    kind: Popup,
    nwin: usize,
) {
    let (px, py, pw, ph) = popup_rect(sw, sh);
    fill_clip(back, r, (px, py, pw, 2), FRAME);
    fill_clip(back, r, (px, py + ph - 2, pw, 2), FRAME);
    fill_clip(back, r, (px, py, 2, ph), FRAME);
    fill_clip(back, r, (px + pw - 2, py, 2, ph), FRAME);
    let tb = (px + 2, py + 2, pw - 4, TITLE_H);
    fill_clip(back, r, tb, TITLE_BG);
    if r_inter(r, tb) {
        let title = match kind {
            Popup::About => "About GreenLime OS",
            Popup::Settings => "Settings",
        };
        draw_text(back, font, px + 12, py + 7, title, FONT_PX, TITLE_FG, TITLE_BG);
        // X mark (any click dismisses the dialog; this says so visually).
        let xb = (px + pw - 32, py + 6, 22, 18);
        fill_clip(back, r, xb, CLOSE_BG);
        draw_text(back, font, xb.0 + 6, xb.1 + 1, "X", FONT_PX, TITLE_FG, CLOSE_BG);
    }
    let body = (px + 2, py + 2 + TITLE_H, pw - 4, ph - 4 - TITLE_H);
    if r_inter(r, body) {
        fill_clip(back, r, body, MENU_BG2);
        match kind {
            Popup::About => {
                let lines = [
                    "GreenLime OS v0.9",
                    "modern hybrid kernel, Rust from scratch",
                    "click anywhere to dismiss",
                ];
                for (i, line) in lines.iter().enumerate() {
                    draw_text(
                        back,
                        font,
                        px + 20,
                        py + 14 + TITLE_H + i as u32 * 28,
                        line,
                        FONT_PX,
                        MENU_FG,
                        MENU_BG2,
                    );
                }
            }
            Popup::Settings => {
                let secs = idt::ticks() / 100;
                let lines = [
                    alloc::format!("Display      {} x {}", sw, sh),
                    alloc::format!(
                        "Uptime       {:02}:{:02}:{:02}",
                        secs / 3600,
                        (secs % 3600) / 60,
                        secs % 60
                    ),
                    alloc::format!("Windows      {} open", nwin),
                    alloc::format!("Taskbar      left, {}px", TASKBAR_H),
                    String::from("GreenLime OS v0.9"),
                ];
                for (i, line) in lines.iter().enumerate() {
                    draw_text(
                        back,
                        font,
                        px + 20,
                        py + 14 + TITLE_H + i as u32 * 26,
                        line,
                        FONT_PX,
                        MENU_FG,
                        MENU_BG2,
                    );
                }
            }
        }
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

fn draw_text(
    fb: &mut Framebuffer,
    font: &mut FontSystem,
    mut x: u32,
    y: u32,
    s: &str,
    px: f32,
    fg: Rgb,
    bg: Rgb,
) {
    // draw_char needs line_h for the opaque cell; our cells are line boxes.
    let lh = font.line_height(px);
    for ch in s.chars() {
        if ch == '\n' || ch == '\r' {
            continue;
        }
        x += font.draw_char(fb, x, y, ch, px, lh, fg, bg);
    }
}

/// Opaque-cell-free text (watermark/icons over the wallpaper, hover bgs).
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
                        WIN_READ | WIN_CREATE | WIN_CLIENT_READ | WIN_DESTROY
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
        if let Some(srv) = SRV.lock().as_mut() {
            // Cursor off first: at lap boundaries the screen must show the
            // composed state (the cursor itself is not in the back buffer).
            srv.undraw_cursor();
        }
        while let Some(m) = ipc::try_recv(PORT_WINSRV) {
            // Hold the guard across handle (single task, no nesting).
            if let Some(srv) = SRV.lock().as_mut() {
                srv.handle(m);
            }
        }
        if let Some(srv) = SRV.lock().as_mut() {
            srv.poll_input();
            srv.tick_taskbar();
            srv.present(); // compose damage into back buffer + blit
            srv.draw_cursor(); // cursor back on top (fresh save-under)
        }
        // Cooperative yield every lap: keeps latency low, never wedges.
        sched::yield_now();
    }
}

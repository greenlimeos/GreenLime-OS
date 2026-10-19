//! Window server (kernel task, v0.8): owns the screen after fb takeover.
//! Damage-rect compositor over a back buffer: state changes invalidate
//! rects; each lap composes ONLY the damage into the back buffer (layers
//! in z-order: wallpaper desktop, windows, taskbar, start menu, popup)
//! and blits it. The on-screen mouse cursor never lives in the back
//! buffer: it is undrawn before present and redrawn on top after, so
//! compositing can never stomp it.
//!
//! UI theme (v1.2 "Fluent dark" — Windows 11 mica/acrylic design language):
//! deep-charcoal surfaces over a userspace-streamed wallpaper
//! (assets/wallpapers/wallpaper.jpeg, decoded by the ring-3 wallpaper task
//! and streamed via WIN_WALL_* IPC; the kernel only converts + blits),
//! floating acrylic dock pill + weather/tray pills, centered start card
//! (greeting, pinned tile grid, bottom search; no Recent), quick-settings
//! panel (tray click) + Settings/About popups. Windows are rounded,
//! soft-shadowed, and fade-and-rise on open. Real translucency: every
//! panel is alpha-blended with the wallpaper pixels beneath it (mica
//! simulation — the wallpaper vec is the shared backdrop). Window content
//! stays text-only: terminal (id 0, line discipline, opens on demand from
//! the launcher — never at boot) + client windows (WIN_CREATE, raw keys
//! while focused), focus follows clicks, titlebar drag moves, corner grip
//! resizes, taskbar icons focus/raise, double-click titlebar maximizes.
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
use crate::servers::vfs;
use crate::{ipc, kprintln, sched};

const FONT_PX: f32 = 16.0;
// ---- Palette: "Fluent dark" (the Windows 11 mica concept target) --------
// RULE: every painted pixel stays strictly < 239 on ALL channels. The
// mouse cursor (240,240,240) must remain the only >=239 blob on screen —
// qmp_input_test / qmp_tablet_test locate the cursor by that predicate,
// and the wallpaper task clamps its own pixels the same way. A dark theme
// still draws bright text and accents, so every bright constant is capped
// just under the cursor by design (visually reads as near-white).
const BG: Rgb = Rgb(32, 34, 38); // window body (mica dark)
const FG: Rgb = Rgb(208, 214, 224); // window text
const TITLE_BG: Rgb = Rgb(44, 46, 52); // focused titlebar (tests exact-count it)
const TITLE_BG2: Rgb = Rgb(33, 35, 40); // unfocused titlebar (tests exact-count it)
const TITLE_FG: Rgb = Rgb(214, 220, 232);
const FRAME: Rgb = Rgb(74, 79, 90); // focused window border
const FRAME_UNFOC: Rgb = Rgb(48, 52, 60); // unfocused window border
const CLOSE_BG: Rgb = Rgb(56, 58, 66);
const CLOSE_HOVER: Rgb = Rgb(196, 43, 28);
const HOVER: Rgb = Rgb(62, 66, 76); // hover fill on dark panels
const ACTIVE: Rgb = Rgb(50, 58, 74); // pressed / open pill (accent-tinted)
const ACCENT: Rgb = Rgb(76, 194, 255); // fluent dark cyan-blue
const ACCENT_FILL: Rgb = Rgb(44, 78, 110); // accent-tinted fills (LED bars, strips)
const MENU_BG: Rgb = Rgb(38, 41, 48); // acrylic panel tint (dock/tray/weather)
const CARD: Rgb = Rgb(34, 37, 44); // acrylic card tint (start menu / popups)
const MENU_BG2: Rgb = Rgb(52, 56, 64); // solid tiles / search / quick rows (tests count it)
const MENU_FG: Rgb = Rgb(226, 230, 238);
const SUB_FG: Rgb = Rgb(126, 136, 154); // hints / dim labels
const DESK_FG: Rgb = Rgb(148, 166, 196); // watermark over the dark wallpaper
const CURSOR: Rgb = Rgb(240, 240, 240); // tests detect >= 239 in ALL channels
const ICON_BG: Rgb = Rgb(60, 64, 74); // icon wells
const SHADOW: Rgb = Rgb(2, 3, 8); // window/panel drop shadow tint (reserved)
const INK: Rgb = Rgb(198, 204, 216); // tray glyph strokes
const FOLDER_BG: Rgb = Rgb(206, 160, 78); // amber folder body
const FOLDER_HI: Rgb = Rgb(240, 216, 140); // folder flap highlight
const PANEL_BORDER: Rgb = Rgb(66, 72, 84); // pill hairline
const CARD_BORDER: Rgb = Rgb(62, 68, 80); // card / dialog hairline

/// Acrylic panel strengths (Q8 alpha): how opaque the tint sits over the
/// wallpaper backdrop. 214 ≈ 84% — glassy but very readable.
const PANEL_A: u32 = 214; // dock / tray / weather pills
const CARD_A: u32 = 226; // start card / popups / quick settings

/// Corner radii (px) — the W11 rounding scale.
const RAD_WIN: u32 = 10; // windows (outer border)
const RAD_PANEL: u32 = 10; // floating pills + cards
const RAD_TILE: u32 = 8; // tiles + buttons inside panels

/// Per-band drop-shadow strength for the 4 shadow rings (inner -> outer).
const SHADOW_A: [u32; 4] = [120, 76, 42, 24];

/// Open animation: length in scheduler ticks (10 ms each) + how far a
/// window rises from (in px). 12 ticks = 120 ms — quick, never in the way.
const ANIM_TICKS: u64 = 12;
const ANIM_RISE: i32 = 14;
/// One compose lap can take LONGER than the whole animation (a loaded
/// debug VM laps at ~1 s), so the fade invalidation keeps animating
/// surfaces alive for this much longer: the first post-expiry lap paints
/// the final crisp (p=256) frame. Release laps at ~10 ms, so the tail is
/// a few invisible crisp frames — no UX difference.
const ANIM_TAIL: u64 = 180;

const TITLE_H: u32 = 30;
const TASKBAR_H: u32 = 48;
const DOCK_H: u32 = 44; // floating pill height inside the strip
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
// Start menu: centered floating card (concept layout) — greeting header,
// one grid of pinned tiles, search pill at the very bottom. No Recent.
const MENU_W: u32 = 560;
const MENU_H: u32 = 436;
/// Pinned-tile grid (relative to the card): 5 columns.
const GRID_Y: u32 = 100;
const TILE: u32 = 92;
const TILE_PITCH: u32 = 100;
const TILE_PITCH_Y: u32 = 106;
/// Bottom search pill (relative to the card).
const SEARCH_Y: u32 = 372;
const SEARCH_H: u32 = 44;

const TERM_ID: u32 = 0;
/// Files window (system window, like the terminal): browsing the vfs.
const FILES_ID: u32 = 1;
/// No window focused (clean desktop: terminal opens on demand, never at boot).
const NO_FOCUS: u32 = u32::MAX;
/// Longest single terminal line (heap guard: the open line is uncapped by
/// SCROLLBACK, which only limits the line *count*).
const MAX_LINE: usize = 4096;
const MAX_WINS: usize = 8;
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

/// Start-menu apps (pinned row + search filter over the same list).
#[derive(Clone, Copy, PartialEq, Eq)]
enum App {
    Files,
    Terminal,
    Demo,
    Settings,
    About,
}

impl App {
    fn name(self) -> &'static str {
        match self {
            App::Files => "Files",
            App::Terminal => "Terminal",
            App::Demo => "Demo",
            App::Settings => "Settings",
            App::About => "About",
        }
    }
}

const APPS: [(&str, App); 5] = [
    ("Files", App::Files),
    ("Terminal", App::Terminal),
    ("Demo", App::Demo),
    ("Settings", App::Settings),
    ("About", App::About),
];

/// Popup dialog kind (top layer; quick is interactive, rest dismiss).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Popup {
    About,
    Settings,
    Quick,
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
    /// Scheduler tick of creation: drives the open animation (fade+rise).
    born: u64,
    /// Saved geometry while maximized (double-click titlebar toggles).
    maxim: Option<(u32, u32, u32, u32)>,
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
    /// Packed XRGB wallpaper: initialized as a flat placeholder at
    /// takeover, then REPLACED row-pixels at a time as the ring-3
    /// wallpaper task streams the real image over IPC (WIN_WALL_*).
    /// The kernel never decodes; userspace owns the wallpaper.
    wallpaper: Vec<u32>,
    /// Wallpaper rows received since SETUP (for the DONE proof line).
    wall_msgs: u32,
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
    /// Tick the start menu opened (open animation fade/slide-in).
    anim_menu: u64,
    /// Open dialog: kind + the tick it opened (open animation).
    popup: Option<(Popup, u64)>,
    search: [u8; 32],
    search_len: usize,
    recent: Vec<App>, // last launched, most recent first (max 3)
    // Quick settings panel (tray click): W11-style toggles + sliders.
    qs_wifi: bool,
    qs_bt: bool,
    qs_night: bool,
    qs_saver: bool,
    qs_volume: u32,
    qs_bright: u32,
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
            wall_msgs: 0,
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
            anim_menu: 0,
            popup: None,
            search: [0; 32],
            search_len: 0,
            recent: Vec::new(),
            qs_wifi: true,
            qs_bt: false,
            qs_night: false,
            qs_saver: false,
            qs_volume: 65,
            qs_bright: 80,
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
            next_id: 2, // 0/1 are system windows (terminal, Files)
            last_click: None,
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
            next_id,
            focus,
            menu_open,
            anim_menu,
            popup,
            search,
            search_len,
            mx,
            my,
            qs_wifi,
            qs_bt,
            qs_night,
            qs_saver,
            qs_volume,
            qs_bright,
            ..
        } = self;
        let wall: &[u32] = wallpaper;
        // 1. Desktop: cached wallpaper rows (integer-rendered once at
        //    takeover — this target has no SSE, so per-pixel f32 here
        //    would be soft-float builtin calls: ~350s at -O0).
        let q = r_clip(r, (0, 0, sw, sh));
        if q.2 > 0 && q.3 > 0 {
            let stride = (back.pitch() / 4) as usize;
            let back_base = back.base_mut() as *mut u32;
            for y in q.1..q.1 + q.3 {
                let src = unsafe { wall.as_ptr().add(y as usize * stride + q.0 as usize) };
                let dst = unsafe { back_base.add(y as usize * stride + q.0 as usize) };
                unsafe { core::ptr::copy_nonoverlapping(src, dst, q.2 as usize) };
            }
        }
        // Watermark (blended, no solid cell box over the gradient).
        let wm = (sw.saturating_sub(300), sh.saturating_sub(TASKBAR_H + 34), 300, 24);
        if r_inter(r, wm) {
            draw_text_blended(
                back,
                font,
                wm.0,
                wm.1,
                "GreenLime OS v1.1",
                FONT_PX,
                DESK_FG,
            );
        }
        // 2. Windows, bottom-to-top (z-order): soft shadow ring first
        //    (blended over whatever is beneath), then the rounded chrome.
        //    A freshly created window rises + fades in (wallpaper scrim).
        for w in windows.iter() {
            if !r_inter(r, w.rect()) && !r_inter(r, pad(w.rect())) {
                continue;
            }
            let p = anim_p(now, w.born);
            window_shadow(back, wall, r, w.rect(), p);
            draw_window(back, font, wall, w, *focus, r, *line_h, *mx, *my, now);
        }
        // 3. Taskbar (floating acrylic pills: weather garnish, dock, tray).
        let tb = strip_rect(sw, sh);
        if r_inter(r, tb) {
            let secs = now / 100;
            draw_taskbar(
                back,
                font,
                wall,
                windows,
                *next_id,
                *focus,
                *menu_open,
                secs,
                r,
                sh,
                *mx,
                *my,
            );
            *last_clock = secs;
        }
        // 4. Start menu (acrylic card above the dock, fade-in on open).
        if *menu_open {
            let m = menu_rect(sw, sh);
            if r_inter(r, m) {
                draw_menu(
                    back,
                    font,
                    wall,
                    r,
                    sw,
                    sh,
                    now,
                    *anim_menu,
                    &search[..*search_len],
                    *mx,
                    *my,
                );
            }
        }
        // 5. Popup (top layer: About / Settings / Quick settings).
        if let Some((kind, born)) = *popup {
            let p = popup_rect(kind, sw, sh);
            if r_inter(r, p) {
                let nwin = windows.len();
                draw_popup(
                    back,
                    font,
                    wall,
                    r,
                    sw,
                    sh,
                    kind,
                    born,
                    nwin,
                    [
                        *qs_wifi,
                        *qs_bt,
                        *qs_night,
                        *qs_saver,
                    ],
                    *qs_volume,
                    *qs_bright,
                );
            }
        }
    }

    /// The taskbar is stale when the clock ticks.
    fn tick_taskbar(&mut self) {
        let secs = idt::ticks() / 100;
        if secs != self.last_clock {
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate(strip_rect(sw, sh));
        }
    }
    /// True while any open animation is still in flight (windows, start
    /// card, popup): those laps must compose + blit every tick.
    fn anim_active(&self) -> bool {
        let now = idt::ticks();
        if self.menu_open && now.saturating_sub(self.anim_menu) < ANIM_TICKS + ANIM_TAIL {
            return true;
        }
        if let Some((_, born)) = self.popup {
            if now.saturating_sub(born) < ANIM_TICKS + ANIM_TAIL {
                return true;
            }
        }
        self.windows
            .iter()
            .any(|w| now.saturating_sub(w.born) < ANIM_TICKS + ANIM_TAIL)
    }

    /// Re-mark animating surfaces as damaged so the fade keeps running:
    /// dirty is cleared by every present(), this regenerates it.
    fn tick_anim(&mut self) {
        let now = idt::ticks();
        if self.menu_open && now.saturating_sub(self.anim_menu) < ANIM_TICKS + ANIM_TAIL {
            let (sw, sh) = (self.fb.width, self.fb.height);
            self.invalidate(pad(menu_rect(sw, sh)));
        }
        let mut pop = None;
        if let Some((kind, born)) = self.popup {
            if now.saturating_sub(born) < ANIM_TICKS + ANIM_TAIL {
                let (sw, sh) = (self.fb.width, self.fb.height);
                pop = Some(pad(popup_rect(kind, sw, sh)));
            }
        }
        if let Some(p) = pop {
            self.invalidate(p);
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
            // Raise changes pixels inside the raised window + its shadow.
            self.invalidate(pad(rect));
        }
        self.focus = id;
        if old_focus != id {
            if let Some(j) = self.idx_of(old_focus) {
                let t = self.windows[j].title_rect();
                self.invalidate(t);
            }
            // Taskbar accent underline follows focus.
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
        // Popup (top layer): the quick panel is interactive inside; any
        // other dialog dismisses on any click.
        if let Some((kind, _)) = self.popup {
            let p = popup_rect(kind, sw, sh);
            let inside = point_in(x, y, p);
            if !inside {
                self.popup = None;
                self.invalidate(pad(p));
                return;
            }
            if self.quick_click(x, y, p) {
                return;
            }
            return; // inside panel, not on a control: keep it open
        }
        // Taskbar slots (usable with the menu open too). Clicks on the
        // strip but off a slot/pill are dead space: close the menu.
        if y >= sh - TASKBAR_H {
            let n = slot_count(self.windows.len());
            for i in 0..n {
                let s = dock_slot_rect(i, sw, sh, self.windows.len());
                if point_in(x, y, s) {
                    self.taskbar_slot(i);
                    return;
                }
            }
            // Tray: W11-style quick settings flyout toggles here.
            if point_in(x, y, tray_rect(sw, sh)) {
                self.toggle_quick();
                return;
            }
            // Weather garnish: harmless shortcut to Settings.
            if point_in(x, y, weather_rect(sw, sh)) {
                self.menu_launch(App::Settings);
                return;
            }
            self.close_menu(); // empty strip space closes the menu
            return;
        }
        // Menu open: panel interactions, else dismiss (click consumed).
        if self.menu_open {
            let (mx0, my0, mw, mh) = menu_rect(sw, sh);
            // Pinned tiles (filtered order must match draw_menu).
            let mut tile = 0u32;
            for (name, app) in APPS {
                if !name_matches(name, &self.search[..self.search_len]) {
                    continue;
                }
                let t = tile_rect(tile, sw, sh);
                if point_in(x, y, t) {
                    self.menu_launch(app);
                    return;
                }
                tile += 1;
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

    /// Taskbar slot i: 0 = Start, 1.. = window (id order).
    fn taskbar_slot(&mut self, i: u32) {
        let (sw, sh) = (self.fb.width, self.fb.height);
        let open = |s: &mut Self| {
            s.menu_open = true;
            s.anim_menu = idt::ticks();
            let (sw, sh) = (s.fb.width, s.fb.height);
            s.invalidate(pad(menu_rect(sw, sh)));
            s.invalidate(strip_rect(sw, sh));
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
        self.invalidate(pad(menu_rect(sw, sh)));
        self.invalidate(strip_rect(sw, sh));
    }

    /// Tray click: open the quick settings flyout (and close anything else
    /// that's open — one flyout at a time, like Windows 11), or close it
    /// if it was already up. Clicks outside the panel dismiss it.
    fn toggle_quick(&mut self) {
        let (sw, sh) = (self.fb.width, self.fb.height);
        if let Some((k, _)) = self.popup {
            let p = popup_rect(k, sw, sh);
            self.popup = None;
            self.invalidate(pad(p));
            if k != Popup::Quick {
                // A dialog was up: just close it, the click toggles nothing.
                return;
            }
            return;
        }
        if self.menu_open {
            self.close_menu();
        }
        self.popup = Some((Popup::Quick, idt::ticks()));
        self.invalidate(pad(popup_rect(Popup::Quick, sw, sh)));
        self.invalidate(strip_rect(sw, sh));
    }

    /// A click inside the open quick panel: routing for toggles, slider
    /// tracks, and the "All settings" shortcut. Returns true when the
    /// press was consumed (panel stays open).
    fn quick_click(&mut self, x: u32, y: u32, p: Rect) -> bool {
        let (sw, sh) = (self.fb.width, self.fb.height);
        // Toggle tiles (2 x 2 grid at the top).
        if let Some(i) = (0u32..4).find(|i| point_in(x, y, quick_tile_rect(*i, sw, sh))) {
            match i {
                0 => self.qs_wifi = !self.qs_wifi,
                1 => self.qs_bt = !self.qs_bt,
                2 => self.qs_night = !self.qs_night,
                _ => self.qs_saver = !self.qs_saver,
            }
            self.invalidate(pad(p));
            return true;
        }
        // Slider tracks: click sets the level directly (W11 seek style).
        if let Some(i) = (0u32..2).find(|i| point_in(x, y, quick_slider_rect(*i, sw, sh))) {
            let s = quick_slider_rect(i, sw, sh);
            let frac = (x - s.0).min(s.2) * 100 / s.2.max(1);
            if i == 0 {
                self.qs_volume = frac;
            } else {
                self.qs_bright = frac;
            }
            self.invalidate(pad(p));
            return true;
        }
        // "All settings" row: swap this flyout for the Settings dialog.
        if point_in(x, y, quick_footer_rect(sw, sh)) {
            self.invalidate(pad(p));
            self.popup = Some((Popup::Settings, idt::ticks()));
            self.invalidate(pad(popup_rect(Popup::Settings, sw, sh)));
            return true;
        }
        false
    }

    /// Launch a Start-menu app: record it in Recent, close the menu,
    /// then perform its action.
    fn menu_launch(&mut self, app: App) {
        self.recent.retain(|a| *a != app);
        self.recent.insert(0, app);
        self.recent.truncate(3);
        self.close_menu();
        let (sw, sh) = (self.fb.width, self.fb.height);
        match app {
            App::Files => self.ensure_files(),
            App::Terminal => self.ensure_terminal(),
            App::Demo => {
                let demo = self.windows.iter().find(|w| w.client).map(|w| w.id);
                if let Some(id) = demo {
                    self.focus_window(id);
                }
            }
            App::Settings => {
                self.popup = Some((Popup::Settings, idt::ticks()));
                self.invalidate(pad(popup_rect(Popup::Settings, sw, sh)));
            }
            App::About => {
                self.popup = Some((Popup::About, idt::ticks()));
                self.invalidate(pad(popup_rect(Popup::About, sw, sh)));
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
                born: idt::ticks(),
                maxim: None,
                title: String::from("Terminal - greenlime-shell"),
                lines,
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

    /// Toggle maximized geometry (double-click on a titlebar): save the
    /// current rect once, stretch to the full work area; toggling again
    /// restores. Shadows repaint via pad().
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
                client: false,
                owner: u32::MAX,
                x: 230,
                y: 140,
                w: 540,
                h: 420,
                born: idt::ticks(),
                maxim: None,
                title,
                lines,
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
                    born: idt::ticks(),
                    maxim: None,
                    title,
                    lines: VecDeque::new(),
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

/// Bottom-strip layout (concept): a centered floating dock pill plus a
/// left weather pill and a right tray pill, all floating inside the
/// TASKBAR_H strip with dead margins at the strip edges.
fn strip_y(sh: u32) -> u32 {
    sh - TASKBAR_H + (TASKBAR_H - DOCK_H) / 2
}

/// The strip repaint rect including the pills' drop-shadow overhang
/// (shadows rise ~SHADOW_PAD px above the strip line — partial repaints
/// must cover it or stale ring pixels linger over the wallpaper).
fn strip_rect(sw: u32, sh: u32) -> Rect {
    (
        0,
        sh - TASKBAR_H - SHADOW_PAD,
        sw,
        TASKBAR_H + SHADOW_PAD,
    )
}

/// The centered dock pill: Start + one slot per window.
fn dock_rect(sw: u32, sh: u32, nwin: usize) -> Rect {
    let n = slot_count(nwin);
    let w = 16 + n * (SLOT + SLOT_GAP) - SLOT_GAP + 16;
    ((sw - w) / 2, strip_y(sh), w, DOCK_H)
}

/// Slot `i` (0 = Start) inside the dock pill.
fn dock_slot_rect(i: u32, sw: u32, sh: u32, nwin: usize) -> Rect {
    let (px, py, _, _) = dock_rect(sw, sh, nwin);
    (
        px + 16 + i * (SLOT + SLOT_GAP),
        py + (DOCK_H - SLOT) / 2,
        SLOT,
        SLOT,
    )
}

/// Right tray pill: wifi/volume/battery glyphs + wall clock. Click opens
/// the quick settings flyout (W11 tray gesture).
fn tray_rect(sw: u32, sh: u32) -> Rect {
    (sw - 12 - 160, strip_y(sh), 160, DOCK_H)
}

/// Left weather pill (decorative garnish — no live weather feed).
fn weather_rect(_sw: u32, sh: u32) -> Rect {
    (14, strip_y(sh), 124, DOCK_H)
}

/// Start menu: centered floating card above the dock.
fn menu_rect(sw: u32, sh: u32) -> Rect {
    ((sw - MENU_W) / 2, sh - TASKBAR_H - 12 - MENU_H, MENU_W, MENU_H)
}

/// Search pill: docked at the bottom of the card.
fn search_rect(sw: u32, sh: u32) -> Rect {
    let (mx0, my0, _, _) = menu_rect(sw, sh);
    (mx0 + 20, my0 + SEARCH_Y, MENU_W - 40, SEARCH_H)
}

/// Pinned tile `i` (index in the *filtered* app list): 5-column grid.
fn tile_rect(i: u32, sw: u32, sh: u32) -> Rect {
    let (mx0, my0, _, _) = menu_rect(sw, sh);
    (
        mx0 + 24 + (i % 5) * TILE_PITCH,
        my0 + GRID_Y + (i / 5) * TILE_PITCH_Y,
        TILE,
        TILE + 8,
    )
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

/// Quick settings flyout geometry: right-anchored above the tray.
const QS_W: u32 = 240;
const QS_H: u32 = 280;

/// Dialog rects: About/Settings centered; Quick settings anchored right.
fn popup_rect(kind: Popup, sw: u32, sh: u32) -> Rect {
    match kind {
        Popup::Quick => (sw - 12 - QS_W, sh - TASKBAR_H - 12 - QS_H, QS_W, QS_H),
        _ => {
            let (pw, ph) = (460u32, 210u32);
            ((sw - pw) / 2, (sh - ph) / 2, pw, ph)
        }
    }
}

/// Toggle tile i (0..4) inside the quick panel: 2x2 grid.
fn quick_tile_rect(i: u32, sw: u32, sh: u32) -> Rect {
    let (px, py, _, _) = popup_rect(Popup::Quick, sw, sh);
    (
        px + 16 + (i % 2) * 104,
        py + 40 + (i / 2) * 64,
        96,
        56,
    )
}

/// Slider row i (0 = volume, 1 = brightness): the CLICK band; the drawn
/// track sits inside it, label above.
fn quick_slider_rect(i: u32, sw: u32, sh: u32) -> Rect {
    let (px, py, _, _) = popup_rect(Popup::Quick, sw, sh);
    (px + 16, py + 164 + i * 44, 208, 30)
}

/// "All settings" footer row in the quick panel (opens Settings).
fn quick_footer_rect(sw: u32, sh: u32) -> Rect {
    let (px, py, _, _) = popup_rect(Popup::Quick, sw, sh);
    (px + 16, py + QS_H - 34, 208, 24)
}

// ---- wallpaper -----------------------------------------------------------

/// Placeholder wallpaper: a dark "bloom" gradient rendered ONCE at
/// takeover, until the ring-3 wallpaper task streams the real image
/// (assets/wallpapers/wallpaper.jpeg, decoded in userspace) over IPC —
/// the kernel never parses image data. Windows 11 dark desktop vibe:
/// deep charcoal-blue with two soft teal/lime blooms + a corner vignette.
/// All channels stay < 239 so the white mouse cursor remains the only
/// >=239 blob on screen (the input/tablet test predicate).
fn render_wallpaper(w: u32, h: u32) -> Vec<u32> {
    let mut out: Vec<u32> = alloc::vec![0u32; (w * h) as usize];
    let ww = w.max(1) as u64;
    let hh = h.max(1) as u64;
    // Per-column dx^2 tables (distances to both bloom centers, Q16).
    // Bloom A: deep teal-blue, big, lower right. Bloom B: brand lime,
    // small, upper left. Base: vertical charcoal gradient.
    let (ax, ay) = (ww * 3 / 4, hh * 3 / 4);
    let (bx, by) = (ww / 4, hh / 3);
    let rad_a = ((ww + hh) / 3).max(1); // Q0 radii squared below
    let rad_b = ((ww + hh) / 5).max(1);
    for x in 0..w {
        let x64 = x as u64;
        let dxa = (ax as i64 - x64 as i64) * (ax as i64 - x64 as i64);
        let dxb = (bx as i64 - x64 as i64) * (bx as i64 - x64 as i64);
        for y in 0..h {
            let y64 = y as u64;
            // Vertical base: (14,17,26) -> (28,34,50), Q16 lerp.
            let t = ((y64 << 16) / hh) as u32;
            let mut r = (14 * 65536 + 14 * t) >> 16;
            let mut g = (17 * 65536 + 17 * t) >> 16;
            let mut b = (26 * 65536 + 24 * t) >> 16;
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
            // A adds teal-blue light, B adds the brand lime accent.
            let ar = (qa * 24) >> 12;
            let ag = (qa * 68) >> 12;
            let ab = (qa * 96) >> 12;
            let br = (qb * 40) >> 12;
            let bg = (qb * 92) >> 12;
            let bb = (qb * 70) >> 12;
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

// ---- acrylic / shadow / rounded-fill engine (v1.2 Fluent dark) -----------

/// One blended pixel: wallpaper + tint at Q8 alpha (0..256). Integer math
/// only — this target soft-floats, so the compositors stay fixed-point.
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
/// repaints: mica = tint*(a) + wallpaper*(1-a), always.
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
            for x in 0..(xe - xs) {
                let wp = *src.add(x as usize);
                *dst.add(x as usize) = blend_px(wp, tint, a);
            }
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
    let base = unsafe { back.base_mut() as *mut u32 };
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
            for x in 0..(x_hi - x_lo) {
                let cur = core::ptr::read_volatile(dst.add(x as usize));
                *dst.add(x as usize) =
                    blend_px(cur, unpack(core::ptr::read(src.add(x as usize))), a);
            }
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

/// Filled triangle, apex up at x = cx, top y = y0 (BT glyph halves).
fn fill_tri_up(back: &mut Framebuffer, cx: i32, y0: i32, h: u32, half: u32, c: Rgb) {
    if y0 < 0 || cx < half as i32 {
        return;
    }
    for row in 0..h {
        let hw = (row * half) / h;
        back.fill_rect((cx - hw as i32).max(0) as u32, (y0 + row as i32) as u32, hw * 2 + 1, 1, c);
    }
}

/// Filled triangle, apex down (base at y0) — mirrors fill_tri_up.
fn fill_tri_down(back: &mut Framebuffer, cx: i32, y0: i32, h: u32, half: u32, c: Rgb) {
    if y0 < 0 || cx < half as i32 {
        return;
    }
    for row in 0..h {
        let hw = (half * (h - row)) / h;
        back.fill_rect((cx - hw as i32).max(0) as u32, (y0 + row as i32) as u32, hw * 2 + 1, 1, c);
    }
}

/// Shrink one px per side (rounded panels keep borders via insets).
fn inner(r: Rect, d: u32) -> Rect {
    (
        r.0 + d,
        r.1 + d,
        r.2.saturating_sub(2 * d),
        r.3.saturating_sub(2 * d),
    )
}

/// A floating panel: shadow ring, 1px hairline border, acrylic interior.
/// Everything on the desktop that floats (dock/weather/tray) goes through
/// this so the whole chrome shares one visual recipe.
fn pill(back: &mut Framebuffer, wall: &[u32], r: Rect, rect: Rect, a: u32) {
    window_shadow(back, wall, r, rect, 256);
    fill_round_clip(back, r, rect, RAD_PANEL, PANEL_BORDER);
    acrylic_round_clip(back, wall, r, inner(rect, 1), RAD_PANEL - 1, MENU_BG, a);
}

/// Magnifier glyph: ring + diagonal handle.
fn draw_magnifier(back: &mut Framebuffer, cx: u32, cy: u32, fg: Rgb, bg: Rgb) {
    fill_circle(back, cx, cy, 7, fg);
    fill_circle(back, cx, cy, 5, bg);
    for i in 0..4 {
        back.fill_rect(cx + 5 + i, cy + 5 + i, 2, 2, fg);
    }
}

/// Start mark: 2x2 accent squares — the Windows 11 logo shape, our own
/// mark in the fluent palette (rows provide the damage clip).
fn draw_start_mark(back: &mut Framebuffer, r: Rect, sx: u32, sy: u32) {
    for (ox, oy) in [(4u32, 4u32), (23u32, 4u32), (4u32, 23u32), (23u32, 23u32)] {
        fill_round_clip(back, r, (sx + ox, sy + oy, 13, 13), 3, ACCENT);
    }
}

/// 40x40 app icon at (x, y): glyph only, no box (rows draw their own bg).
fn draw_app_icon(back: &mut Framebuffer, font: &mut FontSystem, app: App, x: u32, y: u32) {
    match app {
        App::Files => {
            // Folder: tab + body + flap highlights (amber, tuned for dark).
            back.fill_rect(x + 4, y + 6, 14, 4, FOLDER_BG);
            back.fill_rect(x + 4, y + 10, 32, 22, FOLDER_BG);
            back.fill_rect(x + 8, y + 18, 24, 3, FOLDER_HI);
            back.fill_rect(x + 8, y + 24, 24, 3, FOLDER_HI);
        }
        App::Terminal => {
            let s = 32.0f32;
            let tw = font.char_advance('>', s) + font.char_advance('_', s);
            draw_text_blended(back, font, x + (40 - tw) / 2, y + 6, ">_", s, ACCENT);
        }
        App::Demo => {
            // Mini window: dark well + accent title strip.
            back.fill_rect(x, y + 4, 40, 30, ICON_BG);
            back.fill_rect(x, y + 4, 40, 8, ACCENT_FILL);
            back.fill_rect(x + 5, y + 18, 30, 2, SUB_FG);
            back.fill_rect(x + 5, y + 24, 20, 2, SUB_FG);
        }
        App::Settings => {
            // Sliders glyph: rails + accent knobs.
            for (i, knob) in [(0u32, 6u32), (1, 24), (2, 14)] {
                let yy = y + 6 + i * 12;
                back.fill_rect(x, yy + 3, 40, 2, SUB_FG);
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

/// Compose one window: 1px border + rounded silhouette (focused = lighter
/// border + slightly lighter titlebar), close button (red on hover),
/// content lines, and the open-animation scrim (wallpaper dissolves in
/// from the bottom-left as the window rises into place). The soft shadow
/// is drawn by compose() just before this (under the chrome).
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
    // Border: silhouette fill, then the inner silhouette is overlaid by
    // the titlebar + body fills — leaves a 1px border ring.
    let border = if focused { FRAME } else { FRAME_UNFOC };
    fill_round_clip(back, r, (x, y, w, h), RAD_WIN, border);
    let tbg = if focused { TITLE_BG } else { TITLE_BG2 };
    fill_round_clip(back, r, (x + 1, y + 1, w - 2, h - 2), RAD_WIN - 1, tbg);
    // Close button (every window, terminal included): neutral, red while
    // hovered. The terminal is a system window but closable — the shell
    // keeps running and it reopens from the launcher with buffered text.
    let close = (x + w - 30, y + 6, 22, 18);
    let hover = point_in(mx, my, close);
    let cbg = if hover { CLOSE_HOVER } else { CLOSE_BG };
    fill_round_clip(back, r, close, 5, cbg);
    if r_inter(r, close) {
        draw_text(back, font, close.0 + 6, close.1 + 2, "X", 14.0, TITLE_FG, cbg);
    }
    // Body band below the titlebar: BG fill with rounded bottom corners
    // (top edge meets the titlebar cleanly).
    fill_round_bottom_clip(
        back,
        r,
        (x + 1, y + 1 + TITLE_H, w - 2, h - 2 - TITLE_H),
        RAD_WIN - 1,
        BG,
    );
    // Title text (titlebar was fully painted above).
    if r_inter(r, (x + 2, y + 2, w - 4, TITLE_H)) {
        draw_text(back, font, x + 12, y + 7, &win.title, FONT_PX, TITLE_FG, tbg);
    }
    // Content text (content_rect keeps its geometry for hit-testing; the
    // rise shifts only the drawing during the open animation).
    let cr = win.content_rect();
    let crd = (cr.0, cr.1 + rise, cr.2, cr.3);
    if r_inter(r, crd) {
        draw_lines(back, font, win, crd, r, line_h);
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

/// Bottom strip (concept layout): floating acrylic pills — left weather
/// garnish, centered dock (2x2 accent start mark + one icon per window
/// with a round focus LED), right tray (wifi/speaker/battery strokes +
/// RTC wall clock). Hover/active states paint denser rounded slot fills;
/// clicks route through the same dock_slot_rect/tray_rect walk as here.
#[allow(clippy::too_many_arguments)]
fn draw_taskbar(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    wall: &[u32],
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

    // ---- left weather garnish (clickable -> Settings) ----
    let wpx = weather_rect(sw, sh);
    if r_inter(r, wpx) {
        pill(back, wall, r, wpx, PANEL_A);
        if point_in(mx, my, wpx) {
            fill_round_clip(back, r, inner(wpx, 1), RAD_PANEL - 1, HOVER);
        }
        let cy = wpx.1 + wpx.3 / 2;
        fill_circle(back, wpx.0 + 20, cy - 2, 5, Rgb(63, 122, 46));
        fill_circle(back, wpx.0 + 20, cy - 2, 4, Rgb(230, 188, 82));
        fill_circle(back, wpx.0 + 20, cy - 2, 3, Rgb(240, 208, 120));
        draw_text_blended(back, font, wpx.0 + 34, wpx.1 + 8, "18", FONT_PX, MENU_FG);
        let w18: u32 = "18".chars().map(|c| font.char_advance(c, FONT_PX)).sum();
        // Degree glyph: tiny solid dot (the font has no '°').
        fill_circle(back, wpx.0 + 34 + w18 + 3, wpx.1 + 13, 2, SUB_FG);
        draw_text_blended(back, font, wpx.0 + 34, wpx.1 + 25, "Tokyo", 12.0, SUB_FG);
    }

    // ---- centered dock pill (acrylic) ----
    let dock = dock_rect(sw, sh, windows.len());
    if r_inter(r, dock) {
        pill(back, wall, r, dock, PANEL_A);
    }
    let n = slot_count(windows.len());
    for i in 0..n {
        let (sx, sy, _, _) = dock_slot_rect(i, sw, sh, windows.len());
        let s = (sx, sy, SLOT, SLOT);
        if !r_inter(r, s) {
            continue;
        }
        let hover = point_in(mx, my, s);
        let active = i == 0 && menu_open;
        let slot_bg = if active {
            ACTIVE
        } else if hover {
            HOVER
        } else {
            MENU_BG
        };
        if slot_bg != MENU_BG {
            fill_round_clip(back, r, s, SLOT / 4, slot_bg);
        }
        match i {
            0 => draw_start_mark(back, r, sx, sy),
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
                        draw_text_blended(back, font, sx + 11, sy + 13, ">_", 14.0, ACCENT);
                    } else if id == FILES_ID {
                        // Mini amber folder in the dock.
                        fill_clip(back, r, (sx + 10, sy + 11, 9, 4), FOLDER_BG);
                        fill_round_clip(back, r, (sx + 8, sy + 14, 22, 14), 3, FOLDER_BG);
                        fill_clip(back, r, (sx + 11, sy + 20, 15, 2), FOLDER_HI);
                        fill_clip(back, r, (sx + 11, sy + 24, 15, 2), FOLDER_HI);
                    } else {
                        // Mini window: dark well + accent title strip.
                        fill_round_clip(back, r, (sx + 9, sy + 10, 22, 19), 3, ICON_BG);
                        fill_round_clip(back, r, (sx + 9, sy + 10, 22, 6), 3, ACCENT_FILL);
                    }
                    if id == focus {
                        // Accent LED under the running app (W11 style).
                        fill_round_clip(back, r, (sx + 10, sy + 34, 20, 3), 1, ACCENT);
                    }
                }
            }
        }
    }

    // ---- right tray pill (acrylic): glyphs + wall clock ----
    let tr = tray_rect(sw, sh);
    if r_inter(r, tr) {
        pill(back, wall, r, tr, PANEL_A);
        if point_in(mx, my, tr) {
            fill_round_clip(back, r, inner(tr, 1), RAD_PANEL - 1, HOVER);
        }
        let minute = crate::drivers::rtc::now_minute(secs);
        let clock = alloc::format!("{:02}:{:02}", minute / 60, minute % 60);
        let cw: u32 = clock
            .chars()
            .map(|c| font.char_advance(c, FONT_PX))
            .sum();
        let cxr = tr.0 + tr.2 - 14 - cw;
        draw_text_blended(back, font, cxr, tr.1 + 13, &clock, FONT_PX, MENU_FG);
        // wifi: dot + two rising arcs (stroke-style, no bg punching —
        // every glyph paints pure pixels over the acrylic).
        let (wx, wcy) = (tr.0 + 22, tr.1 + 32);
        fill_circle(back, wx, wcy, 2, INK);
        fill_arc_top(back, wx, wcy, 11, 8, INK);
        fill_arc_top(back, wx, wcy, 6, 3, INK);
        // speaker: body + right-fanning wedge.
        let (vx, vy) = (tr.0 + 44, tr.1 + 16);
        back.fill_rect(vx, vy + 4, 4, 6, INK);
        for i in 0..4u32 {
            back.fill_rect(vx + 4 + i, vy + 4 - i, 1, 7 + 2 * i, INK);
        }
        // battery: stroked shell + nub + charge bar.
        let (bx, by) = (tr.0 + 64, tr.1 + 17);
        back.fill_rect(bx, by, 20, 2, INK);
        back.fill_rect(bx, by + 8, 20, 2, INK);
        back.fill_rect(bx, by + 2, 2, 6, INK);
        back.fill_rect(bx + 18, by + 2, 2, 6, INK);
        back.fill_rect(bx + 20, by + 4, 2, 4, INK);
        back.fill_rect(bx + 3, by + 3, 11, 5, INK);
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

/// Rounded corners on the TOP rows only (title bars, strips that sit on a
/// straight bottom edge).
fn fill_round_top_clip(back: &mut Framebuffer, r: Rect, rect: Rect, rad: u32, c: Rgb) {
    let (x0, y0, w, h) = rect;
    if w == 0 || h == 0 {
        return;
    }
    let rad = rad.min(w / 2).min(h / 2) as i32;
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

/// Rounded corners on the BOTTOM rows only (window bodies under a
/// straight title edge, sliders that sit inside panels).
fn fill_round_bottom_clip(back: &mut Framebuffer, r: Rect, rect: Rect, rad: u32, c: Rgb) {
    let (x0, y0, w, h) = rect;
    if w == 0 || h == 0 || h <= rad + 1 {
        fill_clip(back, r, rect, c);
        return;
    }
    let rad = rad.min(w / 2).min(h / 2) as i32;
    for row in 0..h {
        let dy: i32 = if row >= h - rad as u32 {
            (row as i32) - (h as i32 - rad - 1)
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

/// Start card (concept layout): centered floating acrylic panel —
/// greeting header (accent disc + time-of-day + name), "Pinned" label,
/// one grid of app tiles (filtered by search), search pill at the very
/// bottom. Fades in (acrylic alpha ramp) when opened. No Recent section:
/// recents were cut in the redesign.
#[allow(clippy::too_many_arguments)]
fn draw_menu(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    wall: &[u32],
    r: Rect,
    sw: u32,
    sh: u32,
    now: u64,
    menu_tick: u64,
    search: &[u8],
    mx: u32,
    my: u32,
) {
    let (mx0, my0, mw, mh) = menu_rect(sw, sh);
    let p = anim_p(now, menu_tick);
    // Acrylic card: soft shadow + hairline border + wallpaper-blended
    // interior whose opacity ramps with the open animation.
    window_shadow(back, wall, r, (mx0, my0, mw, mh), p);
    fill_round_clip(back, r, (mx0, my0, mw, mh), RAD_PANEL, CARD_BORDER);
    acrylic_round_clip(
        back,
        wall,
        r,
        (mx0 + 1, my0 + 1, mw - 2, mh - 2),
        RAD_PANEL - 1,
        CARD,
        CARD_A * p / 256,
    );
    // Header: accent disc with the mark + time-of-day greeting.
    if r_inter(r, (mx0, my0 + 16, mw, 44)) {
        fill_circle(back, mx0 + 40, my0 + 38, 14, ACCENT);
        // Near-white G; reads clean over the accent disc and stays under
        // the all-channel-239 cursor predicate (red channel 232 < 239).
        draw_text_blended(back, font, mx0 + 34, my0 + 28, "G", 20.0, Rgb(232, 238, 246));
        draw_text_blended(
            back,
            font,
            mx0 + 64,
            my0 + 30,
            greeting_text(crate::drivers::rtc::now_minute(now / 100)),
            FONT_PX,
            MENU_FG,
        );
    }
    // "Pinned" label + launcher hint.
    if r_inter(r, (mx0, my0 + 68, mw, 24)) {
        draw_text_blended(back, font, mx0 + 24, my0 + 70, "Pinned", 13.0, MENU_FG);
        draw_text_blended(back, font, mx0 + 92, my0 + 71, ">", FONT_PX, SUB_FG);
    }
    // Pinned tiles (filtered by the search query, contiguous indices).
    // Same order as the click handler's tile_rect walk.
    let ta = (p * 2).min(256); // tiles fade in slightly ahead of the card
    let mut tile = 0u32;
    for (name, app) in APPS {
        if !name_matches(name, search) {
            continue;
        }
        let t = tile_rect(tile, sw, sh);
        tile += 1;
        if !r_inter(r, t) {
            continue;
        }
        let hover = point_in(mx, my, t);
        let tbg = if hover { HOVER } else { MENU_BG2 };
        acrylic_round_clip(back, wall, r, t, RAD_TILE, tbg, ta);
        if ta >= 256 {
            draw_app_icon(back, font, app, t.0 + (t.2 - 40) / 2, t.1 + 12);
            let nw: u32 = name.chars().map(|c| font.char_advance(c, 13.0)).sum();
            draw_text_blended(
                back,
                font,
                t.0 + (t.2 - nw) / 2,
                t.1 + 62,
                name,
                13.0,
                MENU_FG,
            );
        }
    }
    // Search pill at the very bottom (full-width rounded, W11 style).
    let sb = search_rect(sw, sh);
    if r_inter(r, sb) {
        acrylic_round_clip(back, wall, r, sb, sb.3 / 2, MENU_BG2, ta);
        draw_magnifier(back, sb.0 + 18, sb.1 + 20, Rgb(160, 170, 190), MENU_BG2);
        if search.is_empty() {
            draw_text_blended(
                back,
                font,
                sb.0 + 36,
                sb.1 + 12,
                "Type here to search",
                FONT_PX,
                SUB_FG,
            );
        } else if let Ok(q) = core::str::from_utf8(search) {
            // Fuzz-injected control bytes (e.g. the kernel fuzzer's keys
            // while the menu is open) have no glyphs — fall back to the
            // placeholder instead of a row of tofu boxes.
            if q.chars().all(|c| c >= ' ' && c != '\u{7f}') {
                draw_text_blended(back, font, sb.0 + 36, sb.1 + 12, q, FONT_PX, MENU_FG);
            } else {
                draw_text_blended(
                    back,
                    font,
                    sb.0 + 36,
                    sb.1 + 12,
                    "Type here to search",
                    FONT_PX,
                    SUB_FG,
                );
            }
        }
    }
}

/// "Good Morning/Afternoon/Evening/Night, User!" from the wall-clock
/// minute of day (RTC boot time + uptime). Name comes from the concept
/// (the user's account); GreenLime ships as a single-user desktop.
fn greeting_text(minute_of_day: u32) -> &'static str {
    match minute_of_day / 60 {
        5..=11 => "Good Morning, User!",
        12..=16 => "Good Afternoon, User!",
        17..=21 => "Good Evening, User!",
        _ => "Good Night, User!",
    }
}

/// Quick settings flyout (right-anchored acrylic panel): header, 2x2
/// accent/DW toggle tiles with drawn glyphs, two sliders with knobs and
/// live level readouts, and an "All settings" footer that swaps the
/// flyout for the Settings dialog. Every control's hit rect lives beside
/// its draw helper (quick_*_rect) so clicks and pixels never drift.
#[allow(clippy::too_many_arguments)]
fn draw_quick(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    wall: &[u32],
    r: Rect,
    sw: u32,
    sh: u32,
    born: u64,
    data: ([bool; 4], u32, u32),
) {
    let now = idt::ticks();
    let p = anim_p(now, born);
    let (px, py, _, _) = popup_rect(Popup::Quick, sw, sh);
    window_shadow(back, wall, r, (px, py, QS_W, QS_H), p);
    fill_round_clip(back, r, (px, py, QS_W, QS_H), RAD_PANEL, CARD_BORDER);
    acrylic_round_clip(
        back,
        wall,
        r,
        (px + 1, py + 1, QS_W - 2, QS_H - 2),
        RAD_PANEL - 1,
        CARD,
        CARD_A * p / 256,
    );
    let (states, volume, bright) = data;
    // Header.
    draw_text_blended(back, font, px + 20, py + 12, "Quick settings", FONT_PX, MENU_FG);
    // 2x2 toggle tiles (accent when on, dark ink over accent).
    let labels = ["Wi-Fi", "Bluetooth", "Night", "Saver"];
    for i in 0..4u32 {
        let t = quick_tile_rect(i, sw, sh);
        if !r_inter(r, t) {
            continue;
        }
        let on = states[i as usize];
        let tbg = if on { ACCENT } else { MENU_BG2 };
        fill_round_clip(back, r, t, RAD_TILE, tbg);
        let ink = if on { Rgb(26, 30, 36) } else { SUB_FG };
        match i {
            0 => {
                let cx = t.0 + 18;
                let cy = t.1 + 40;
                fill_circle(back, cx, cy, 2, ink);
                fill_arc_top(back, cx, cy, 11, 8, ink);
                fill_arc_top(back, cx, cy, 6, 3, ink);
            }
            1 => {
                let cx = (t.0 + 18) as i32;
                let cy = (t.1 + 28) as i32;
                fill_tri_up(back, cx, cy - 9, 9, 4, ink);
                fill_tri_down(back, cx, cy, 9, 4, ink);
            }
            2 => {
                let cx = t.0 + 20;
                let cy = t.1 + 28;
                fill_circle(back, cx, cy, 10, ink);
                fill_circle(back, cx + 5, cy - 4, 9, tbg);
            }
            _ => {
                let (bx, by) = (t.0 + 10, t.1 + 23);
                back.fill_rect(bx, by, 18, 2, ink);
                back.fill_rect(bx, by + 8, 18, 2, ink);
                back.fill_rect(bx, by + 2, 2, 6, ink);
                back.fill_rect(bx + 16, by + 2, 2, 6, ink);
                back.fill_rect(bx + 18, by + 4, 2, 4, ink);
                back.fill_rect(bx + 3, by + 3, 9, 4, ink);
            }
        }
        draw_text_blended(
            back,
            font,
            t.0 + 34,
            t.1 + 21,
            labels[i as usize],
            11.0,
            if on { ink } else { MENU_FG },
        );
    }
    // Sliders: label + rounded track + accent fill + knob + readout.
    let sil = ["Volume", "Brightness"];
    let levels = [volume, bright];
    for i in 0..2u32 {
        let s = quick_slider_rect(i, sw, sh);
        if !r_inter(r, s) {
            continue;
        }
        draw_text_blended(back, font, s.0, s.1 - 4, sil[i as usize], 11.0, SUB_FG);
        let track = (s.0, s.1 + 12, 176, 14);
        fill_round_clip(back, r, track, track.3 / 2, MENU_BG2);
        let v = levels[i as usize].min(100);
        if v > 0 {
            let fw = (track.2 * v / 100).max(9);
            fill_round_clip(back, r, (track.0, track.1, fw, track.3), track.3 / 2, ACCENT);
        }
        let kx = track.0 + (track.2 * v / 100).clamp(7, track.2.saturating_sub(17));
        let ky = track.1 + track.3 / 2;
        fill_circle(back, kx, ky, 8, ACCENT);
        fill_circle(back, kx, ky, 4, Rgb(236, 240, 246));
        draw_text_blended(
            back,
            font,
            s.0 + 184,
            s.1 + 12,
            &alloc::format!("{}", v),
            FONT_PX,
            MENU_FG,
        );
    }
    // Footer: gear + "All settings" (swaps to the Settings dialog).
    let f = quick_footer_rect(sw, sh);
    if r_inter(r, f) {
        fill_circle(back, f.0 + 11, f.1 + 11, 7, SUB_FG);
        fill_circle(back, f.0 + 11, f.1 + 11, 3, CARD);
        draw_text_blended(back, font, f.0 + 26, f.1 + 4, "All settings", 12.0, MENU_FG);
    }
}

/// Centered dialogs (About / Settings), plus the right-anchored quick
/// settings flyout. Acrylic bodies with hairline borders; About/Settings
/// keep the solid title strip the screen tests exact-count. The quick
/// flyout is interactive (toggles + sliders); dialogs dismiss on click.
#[allow(clippy::too_many_arguments)]
fn draw_popup(
    back: &mut Framebuffer,
    font: &mut FontSystem,
    wall: &[u32],
    r: Rect,
    sw: u32,
    sh: u32,
    kind: Popup,
    born: u64,
    nwin: usize,
    toggles: [bool; 4],
    volume: u32,
    bright: u32,
) {
    let now = idt::ticks();
    let p = anim_p(now, born);
    let (px, py, pw, ph) = popup_rect(kind, sw, sh);
    if kind == Popup::Quick {
        draw_quick(back, font, wall, r, sw, sh, born, (toggles, volume, bright));
        return;
    }
    // Acrylic dialog body.
    window_shadow(back, wall, r, (px, py, pw, ph), p);
    fill_round_clip(back, r, (px, py, pw, ph), RAD_PANEL, CARD_BORDER);
    acrylic_round_clip(
        back,
        wall,
        r,
        (px + 1, py + 1, pw - 2, ph - 2),
        RAD_PANEL - 1,
        CARD,
        CARD_A * p / 256,
    );
    // Title strip: solid (the desktop test exact-counts this color).
    if r_inter(r, (px + 1, py + 1, pw - 2, TITLE_H)) {
        fill_round_top_clip(back, r, (px + 1, py + 1, pw - 2, TITLE_H), RAD_PANEL - 1, TITLE_BG);
        let title = match kind {
            Popup::About => "About GreenLime OS",
            Popup::Settings => "Settings",
            Popup::Quick => unreachable!(),
        };
        draw_text(back, font, px + 12, py + 7, title, FONT_PX, TITLE_FG, TITLE_BG);
        // X mark hints dismissal.
        let xb = (px + pw - 32, py + 7, 22, 16);
        fill_round_clip(back, r, xb, 5, CLOSE_BG);
        draw_text(back, font, xb.0 + 6, xb.1 + 1, "X", 13.0, TITLE_FG, CLOSE_BG);
    }
    // Body text — blended over the acrylic card.
    let body = (px + 2, py + 2 + TITLE_H, pw - 4, ph - 4 - TITLE_H);
    if r_inter(r, body) {
        match kind {
            Popup::About => {
                let lines = [
                    "GreenLime OS v1.1",
                    "modern hybrid kernel, Rust from scratch",
                    "click anywhere to dismiss",
                ];
                for (i, line) in lines.iter().enumerate() {
                    let fg = match i {
                        0 => MENU_FG,
                        1 => FG,
                        _ => SUB_FG,
                    };
                    draw_text_blended(
                        back,
                        font,
                        px + 20,
                        py + 14 + TITLE_H + i as u32 * 28,
                        line,
                        FONT_PX,
                        fg,
                    );
                }
            }
            Popup::Settings => {
                let secs = now / 100;
                let lines = [
                    alloc::format!("Display      {} x {}", sw, sh),
                    alloc::format!(
                        "Uptime       {:02}:{:02}:{:02}",
                        secs / 3600,
                        (secs % 3600) / 60,
                        secs % 60
                    ),
                    alloc::format!("Windows      {} open", nwin),
                    alloc::format!("Taskbar      dock pill, {}px strip", TASKBAR_H),
                    String::from("GreenLime OS v1.1"),
                ];
                for (i, line) in lines.iter().enumerate() {
                    let fg = if i < 3 { FG } else { SUB_FG };
                    draw_text_blended(
                        back,
                        font,
                        px + 20,
                        py + 14 + TITLE_H + i as u32 * 26,
                        line,
                        FONT_PX,
                        fg,
                    );
                }
            }
            Popup::Quick => {}
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
            srv.tick_anim(); // keeps fades running lap to lap
            srv.present(); // compose damage into back buffer + blit
            srv.draw_cursor(); // cursor back on top (fresh save-under)
        }
        // Cooperative yield every lap: keeps latency low, never wedges.
        sched::yield_now();
    }
}

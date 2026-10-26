//! Window server protocol (PORT_WINSRV). Multi-window desktop.
//! Terminal ops (1/2) go through the DEV server as `term0`; client-window
//! ops (3..6) are sent to PORT_WINSRV directly by windowed clients.
//! Payloads are UTF-8 text; cells, pixels, and drag events come later.

/// Append UTF-8 text to terminal 0. Fire-and-forget (server ACKs nothing).
/// Console rules: text continues the current line, `'\n'` starts a new one.
pub const WIN_WRITE: u32 = 1;
/// Canonical (line-buffered) read of terminal 0 stdin. The server holds the
/// request until a full line is typed, then replies with the bytes
/// (`'\n'`-terminated; empty payload = EOF / no framebuffer). The DEV server
/// relays this as `OP_READ` on a `term0` handle and blocks until the reply,
/// so clients just do a normal blocking read on their console fd.
pub const WIN_READ: u32 = 2;
/// Create a client window: payload = title (UTF-8, may be empty). Replies
/// `REPLY_OK` with `arg0` = window id, or `REPLY_ERR` (no framebuffer /
/// window table full). New windows do not steal keyboard focus — the
/// user clicks (or the taskbar button) to focus one.
pub const WIN_CREATE: u32 = 3;
/// Append UTF-8 text to client window `arg0`. Fire-and-forget; console
/// rules as WIN_WRITE. The window is redrawn via the compositor.
pub const WIN_CLIENT_WRITE: u32 = 4;
/// Raw key stream from the focused window: held until a key lands, then
/// replies with the bytes typed so far (`REPLY_ERR` = window closed /
/// no framebuffer). Keyboard focus follows clicks; keys go to whichever
/// window is focused.
pub const WIN_CLIENT_READ: u32 = 5;
/// Close window `arg0` (server: `REPLY_OK`) — same path as the titlebar
/// X button, which also EOFs any pending WIN_CLIENT_READ.
pub const WIN_DESTROY: u32 = 6;

/// Wallpaper service handshake (ring-3 wallpaper task, task 8; v1.1 GUI):
/// `arg0` = decoded source width, `arg1` = decoded source height (pixels).
/// Replies `REPLY_OK` with `arg0` = target width | (target height << 32)
/// — the framebuffer dims the task scales to — or `REPLY_ERR` (no wall
/// permission). Requires CAP_WALL; winsrv checks the stamped sender's
/// caps at the gate like every other dangerous surface.
pub const WIN_WALL_SETUP: u32 = 7;
/// Wallpaper pixels: `arg0` = FIRST pixel index of this run (row-major,
/// 0 .. src_w*src_h); payload = consecutive RGB triples (3 B/px) for that
/// run, already scaled to the target dims. Fire-and-forget; blocks only
/// when the port queue is full (natural flow control).
pub const WIN_WALL_ROWS: u32 = 8;
/// Last wallpaper row landed (no args): winsrv prints its proof line.
pub const WIN_WALL_DONE: u32 = 9;

// ---- M15 app graphics API: per-client canvases (window content = pixels)
// A windowed client may switch its OWN window's content area from text
// mode into graphics mode: WIN_BLIT_SETUP allocates a canvas sized
// w x h (clamped to the content viewport), WIN_BLIT streams RGB pixel
// runs into it row-major, WIN_BLIT_END frees it and returns the window
// to text mode. Owner-checked like every client op (kernel-stamped
// sender must be the WIN_CREATE owner) — canvases are per-window, not
// shared. Pixels are opaque XRGB once landed; the client keeps every
// channel < 239 the same as every painted UI color (the wallpaper task
// clamps its own pixels too — the cursor-uniqueness predicate).

/// Enable graphics mode on window `arg0`: `arg1` = canvas w | (h << 32).
/// Replies `REPLY_OK` with the CLAMPED dims packed the same way (canvas
/// may end up smaller than requested — the content viewport bounds it),
/// or `REPLY_ERR` (not owner / no fb / canvas budget exhausted). The
/// canvas starts filled with the window body color.
pub const WIN_BLIT_SETUP: u32 = 10;
/// Pixels into window `arg0`'s canvas: `arg1` = FIRST pixel index of this
/// run (row-major, 0 .. cw*ch), payload = consecutive RGB triples (3 B/px)
/// — the same run shape as WIN_WALL_ROWS. Fire-and-forget (no reply;
/// send blocks on a full port queue = the flow control). Out-of-bounds or
/// non-owner runs are dropped silently.
pub const WIN_BLIT: u32 = 11;
/// Disable graphics mode on window `arg0` (frees the canvas, repaints the
/// content area in text mode, reply `REPLY_OK` — the client needs the
/// sync point so the console phase starts after the pixels really left).
/// Not-owner replies `REPLY_ERR`; no canvas = still `REPLY_OK`.
pub const WIN_BLIT_END: u32 = 12;

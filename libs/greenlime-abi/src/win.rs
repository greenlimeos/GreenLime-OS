//! Window server protocol (PORT_WINSRV). v0.8: multi-window desktop.
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

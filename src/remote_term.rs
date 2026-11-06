//! Remote terminal: GUI-side mirror of a daemon-owned PTY session.
//!
//! Holds the latest [`GridSnapshot`] and forwards input/resize/scroll to the
//! daemon via [`GuiClient`]. [`TerminalView`]-compatible surface for rendering.

use std::sync::{Arc, Mutex};

use gpui::{Context, EventEmitter};

use crate::gui_client::GuiClient;
use crate::runtime::snapshot::{GhostSnap, GridSnapshot};
use crate::term_shared::{Ghost, TerminalEvent};

/// Debounce PTY resizes so 1px layout jitter can't thrash the daemon.
struct ResizeGate {
    /// Last size we asked the daemon for. Only our own requests write it.
    sent: (u16, u16),
    /// Last size measured in layout.
    seen: (u16, u16),
    /// Consecutive frames the measurement has been stable.
    stable: u8,
    /// Live PTY dims off the last snapshot. Input only — never a stand-in for
    /// `sent`. Adopting another client's dims here made two windows on one
    /// circle bounce a pane at framerate: each took the other's size as its
    /// own, re-measured, re-asserted. Geometry ownership is by activation
    /// (`forget_sent_size`), not by whose frame landed last.
    pty: (u16, u16),
}

impl ResizeGate {
    /// Whether a freshly measured grid size has to go to the daemon.
    ///
    /// Large reflows (pane kill auto-close, sash, window resize — more than 1
    /// col/row) go on the first measure; waiting for a second stable frame
    /// left siblings stuck for seconds when nothing else was painting. ±1 cell
    /// still needs two matching frames so float cell-width noise can't thrash
    /// 120↔121 forever.
    fn measure(&mut self, want: (u16, u16)) -> bool {
        if self.sent == want {
            self.seen = want;
            self.stable = 2;
            return false;
        }
        // Nothing asserted yet (fresh pane, post-activation, post-resync) and
        // the PTY already is this size. A redundant Resize costs every client
        // a full grid — the engine drops the damage base on any resize.
        if self.sent == (0, 0) && self.pty == want {
            self.sent = want;
            self.seen = want;
            self.stable = 2;
            return false;
        }
        if self.seen != want {
            self.seen = want;
            let big = want.0.abs_diff(self.sent.0) > 1 || want.1.abs_diff(self.sent.1) > 1;
            if big || self.sent == (0, 0) {
                // Real reflow or first layout — don't wait for another paint.
                self.sent = want;
                self.stable = 2;
                return true;
            }
            self.stable = 1;
            return false;
        }
        self.stable = self.stable.saturating_add(1);
        if self.stable >= 2 {
            self.sent = want;
            return true;
        }
        false
    }
}

pub struct RemoteTerminal {
    pub slug: String,
    /// Arc so the paint path can hold the grid without cloning thousands of cells.
    pub snapshot: Arc<GridSnapshot>,
    pub ghost: Option<Ghost>,
    /// Who last wrote stdin — drives the causal left-gutter tint.
    pub last_input_origin: Option<String>,
    client: Arc<GuiClient>,
    rev: u64,
    resize: Mutex<ResizeGate>,
}

impl RemoteTerminal {
    pub fn new(slug: String, client: Arc<GuiClient>) -> Self {
        Self {
            snapshot: Arc::new(GridSnapshot::empty(&slug)),
            slug,
            ghost: None,
            last_input_origin: None,
            client,
            rev: 0,
            resize: Mutex::new(ResizeGate {
                sent: (0, 0),
                seen: (0, 0),
                stable: 0,
                pty: (0, 0),
            }),
        }
    }

    pub fn set_input_origin(&mut self, origin: String, cx: &mut Context<Self>) {
        self.last_input_origin = Some(origin);
        cx.notify();
    }

    /// Accept the next frame at any rev without blanking the last paint.
    /// Workspace switch: daemon will FULL-flush; without this, a same-rev
    /// frame is dropped and the pane can stick on a stale/empty grid.
    pub fn open_rev_gate(&mut self) {
        self.rev = 0;
    }

    /// Clear the painted grid and accept the next frame at any rev.
    /// Used when a damage decode fails — without resetting `rev`, a full
    /// reattach frame at the same rev is dropped as "stale" and the pane
    /// stays blank until something bumps rev (e.g. window resize).
    pub fn clear_for_resync(&mut self, cx: &mut Context<Self>) {
        let pane = self.slug.clone();
        self.rev = 0;
        self.snapshot = Arc::new(GridSnapshot::empty(&pane));
        self.ghost = None;
        {
            let mut g = self.resize.lock().unwrap();
            // Force the next layout pass to re-send size (hysteresis was
            // "stable" on the old geometry, and a size mismatch is one of the
            // ways a decode fails — our idea of the PTY dims may be wrong).
            g.sent = (0, 0);
            g.seen = (0, 0);
            g.stable = 0;
            g.pty = (0, 0);
        }
        cx.notify();
    }

    pub fn apply_snapshot(&mut self, snap: GridSnapshot, cx: &mut Context<Self>) {
        // Drop stale/duplicate frames (throttle + out-of-order socket).
        if snap.rev != 0 && snap.rev <= self.rev {
            return;
        }
        self.rev = snap.rev;
        // Prefer explicit origin on the snap; never wipe a known origin with None.
        if let Some(o) = &snap.last_input_origin {
            self.last_input_origin = Some(o.clone());
        }
        if let Some(g) = &snap.ghost {
            self.ghost = Some(Ghost {
                id: g.id.clone(),
                text: g.text.clone(),
                from: g.from.clone(),
                reason: g.reason.clone(),
            });
        } else {
            self.ghost = None;
        }
        // Live dims only — see ResizeGate::pty for why this may not touch
        // `sent`.
        self.resize.lock().unwrap().pty = (snap.cols, snap.rows);
        if let Some(t0) = crate::latency_probe::transfer("g_key", "g_paint", &self.slug) {
            crate::latency_probe::record("gui key→grid-apply", t0.elapsed().as_micros() as u64);
        }
        // Universal notify→paint gauge: every applied grid marks; the pane's
        // next canvas paint completes ("gui apply→paint"). Dense samples of
        // frame-scheduling latency without needing keystrokes.
        crate::latency_probe::mark("g_frame", &self.slug);
        self.snapshot = Arc::new(snap);
        cx.emit(TerminalEvent::Wakeup);
        // All visible panes paint live. Visibility is enforced upstream
        // (daemon skips other workspaces; GUI drops their grids). The old
        // unfocused ~2–5fps cap was a crisis throttle for pre-batch paint.
        cx.notify();
    }

    pub fn set_ghost(&mut self, ghost: Option<GhostSnap>, cx: &mut Context<Self>) {
        self.ghost = ghost.map(|g| Ghost {
            id: g.id,
            text: g.text,
            from: g.from,
            reason: g.reason,
        });
        // Rebuild snapshot with updated ghost (rare path).
        let mut snap = (*self.snapshot).clone();
        snap.ghost = self.ghost.as_ref().map(|g| GhostSnap {
            id: g.id.clone(),
            text: g.text.clone(),
            from: g.from.clone(),
            reason: g.reason.clone(),
        });
        self.snapshot = Arc::new(snap);
        cx.notify();
    }

    pub fn is_running(&self) -> bool {
        self.snapshot.running
    }

    pub fn title(&self) -> Option<String> {
        self.snapshot.title.clone()
    }

    pub fn write_bytes(&self, bytes: Vec<u8>) {
        crate::term_shared::touch_typing_hot();
        crate::latency_probe::mark("g_key", &self.slug);
        let _ = self.client.input(&self.slug, &bytes);
    }

    /// Paste clipboard text into the PTY. Daemon-side inject uses bracketed
    /// paste when the app requested it (same path as `ctl send --no-submit`).
    pub fn paste(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let _ = self.client.inject(&self.slug, text, false);
    }

    /// Optimistic local echo for a printable ASCII key so typing feels instant
    /// even when a neighbor TUI is burning paint budget. Authoritative grid
    /// from the daemon (higher `rev`) overwrites this on the next push.
    pub fn local_echo_char(&mut self, ch: char, cx: &mut Context<Self>) {
        if ch.is_control() || ch == '\u{7f}' {
            return;
        }
        let mut s = (*self.snapshot).clone();
        if s.cols == 0 || s.rows == 0 || s.cells.is_empty() {
            return;
        }
        let cols = s.cols as usize;
        let row = s.cursor_row as usize;
        let col = s.cursor_col as usize;
        if row >= s.rows as usize {
            return;
        }
        let idx = row * cols + col;
        if idx >= s.cells.len() {
            return;
        }
        // Don't invent colors — keep whatever was under the cursor.
        s.cells[idx].c = ch;
        s.cells[idx].bold = false;
        s.cells[idx].dim = false;
        s.cells[idx].italic = false;
        s.cells[idx].underline = false;
        s.cells[idx].inverse = false;
        if col + 1 < cols {
            s.cursor_col = (col + 1) as u16;
        }
        // Leave `rev` unchanged so the next real frame always wins.
        self.snapshot = Arc::new(s);
        cx.notify();
    }

    pub fn inject(&self, text: String, submit: bool) {
        let _ = self.client.inject(&self.slug, &text, submit);
    }

    /// Forget the size we last told the daemon, so the next layout pass
    /// re-states it even though our own geometry never changed.
    ///
    /// PTY dims are last-writer-wins in the engine and every client only
    /// speaks up when its *own* measured grid moves. So once the web client
    /// reshapes a pane to fit a phone, this window would never mention its
    /// geometry again — the pane stays phone-sized inside a desktop window
    /// until something happens to jog a resize. Called on window activation:
    /// the client the human just switched to re-takes the dims, and an
    /// inactive one stays quiet instead of fighting over them.
    pub fn forget_sent_size(&self) {
        if let Ok(mut g) = self.resize.lock() {
            g.sent = (0, 0);
            g.seen = (0, 0);
            g.stable = 0;
        }
    }

    pub fn resize_cells(&self, cols: u16, rows: u16) {
        let want = (cols.max(2), rows.max(2));
        let should_send = self.resize.lock().unwrap().measure(want);
        if should_send {
            let _ = self.client.resize(&self.slug, want.0, want.1);
        }
    }

    pub fn scroll_lines(&self, delta: i32) {
        let _ = self.client.scroll(&self.slug, delta);
    }

    pub fn scroll_to_bottom(&self) {
        let _ = self.client.scroll_bottom(&self.slug);
    }

    pub fn ghost_accept(&self) {
        let _ = self.client.ghost_accept(&self.slug);
    }

    pub fn ghost_reject(&self) {
        let _ = self.client.ghost_reject(&self.slug);
    }
}

impl EventEmitter<TerminalEvent> for RemoteTerminal {}

#[cfg(test)]
mod tests {
    use super::ResizeGate;

    fn gate(sent: (u16, u16), pty: (u16, u16)) -> ResizeGate {
        ResizeGate {
            sent,
            seen: sent,
            stable: 2,
            pty,
        }
    }

    /// THE two-client bug: a window whose own geometry never moved must stay
    /// quiet no matter what dims the daemon broadcasts. The gate used to adopt
    /// the incoming size as `sent`, so the next paint re-asserted — forever.
    #[test]
    fn foreign_dims_never_provoke_a_reassert() {
        let mut g = gate((120, 40), (80, 24));
        for _ in 0..10 {
            assert!(!g.measure((120, 40)));
        }
    }

    /// Two windows on one circle, neither activated: the pane settles where
    /// the last activation left it and nobody sends anything.
    #[test]
    fn two_windows_settle_without_a_single_resize() {
        let (a_dims, b_dims) = ((120, 40), (80, 24));
        let mut pty = a_dims;
        let mut a = gate(a_dims, pty);
        let mut b = gate(b_dims, pty);
        let mut sends = 0;
        for _ in 0..10 {
            a.pty = pty;
            if a.measure(a_dims) {
                pty = a_dims;
                sends += 1;
            }
            b.pty = pty;
            if b.measure(b_dims) {
                pty = b_dims;
                sends += 1;
            }
        }
        assert_eq!(sends, 0);
        assert_eq!(pty, a_dims);
    }

    /// A window that never asserted takes the dims once, then both settle.
    #[test]
    fn fresh_window_asserts_once() {
        let (a_dims, b_dims) = ((120, 40), (80, 24));
        let mut pty = a_dims;
        let mut a = gate(a_dims, pty);
        let mut b = gate((0, 0), pty);
        b.seen = (0, 0);
        let mut sends = 0;
        for _ in 0..10 {
            a.pty = pty;
            if a.measure(a_dims) {
                pty = a_dims;
                sends += 1;
            }
            b.pty = pty;
            if b.measure(b_dims) {
                pty = b_dims;
                sends += 1;
            }
        }
        assert_eq!(sends, 1);
        assert_eq!(pty, b_dims);
    }

    #[test]
    fn activation_retakes_the_dims() {
        let mut g = gate((0, 0), (80, 24));
        g.seen = (0, 0);
        assert!(g.measure((120, 40)));
        assert_eq!(g.sent, (120, 40));
    }

    /// Activation on a pane the PTY already fits: latch, no round trip.
    #[test]
    fn nothing_to_say_when_the_pty_already_fits() {
        let mut g = gate((0, 0), (120, 40));
        g.seen = (0, 0);
        assert!(!g.measure((120, 40)));
        assert_eq!(g.sent, (120, 40));
    }

    #[test]
    fn big_reflow_goes_on_the_first_measure() {
        let mut g = gate((120, 40), (120, 40));
        assert!(g.measure((80, 24)));
    }

    #[test]
    fn one_cell_jitter_waits_for_a_second_frame() {
        let mut g = gate((120, 40), (120, 40));
        assert!(!g.measure((121, 40)));
        assert!(g.measure((121, 40)));
    }
}

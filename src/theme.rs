//! seance theme + branding module: the active color scheme's chrome.
//!
//! Twelve schemes live in `seance_core::schemes`; the daemon picks one and
//! tells every window (`GuiEvent::Theme` -> [`set_scheme`]). Candlelit, the
//! default and the one described below, is authored here in HSL.
//!
//! Candlelit: a dark room lit by one candle. Deep warm charcoal grounds,
//! amber candle-glow accents, muted violet highlights. Elegant, understated —
//! not gothic kitsch.
//!
//! ── palette (hex, for human reference — computed from the HSL below) ────────
//!   bg           #131111   deepest warm charcoal (window/pane ground)
//!   bg_elevated  #1C1718   slightly lifted charcoal (panels, popovers)
//!   surface      #211C1D   interactive surface (buttons, inputs, tabs)
//!   border       #352C2E   hairline separators, warm-tinted
//!   text         #EBE3DB   warm off-white (primary text)
//!   text_dim     #A69A91   muted parchment (secondary text)
//!   text_faint   #69605D   faint (disabled / tertiary)
//!   flame        #E9A03A   amber candle-glow accent (primary)
//!   flame_dim    #A97328   dimmed ember (hover/active amber)
//!   violet       #A790D5   muted violet highlight (secondary accent)
//!   violet_dim   #6C5B95   dimmed violet
//!   success      #86B67C   soft sage (calm, not neon)
//!   danger       #D0675D   warm muted red (does not scream)
//!   status_exited  #69605D gray — the circle closed
//! ───────────────────────────────────────────────────────────────────────────
//!
//! All colors are `gpui::Hsla`. Candlelit is authored with `gpui_component::hsl`
//! (h: 0..360, s: 0..100, l: 0..100) — this is the crate's own helper
//! (crates/ui/src/theme/color.rs:16) so degrees/percent match the table above.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use gpui::{App, Hsla};
use gpui_component::{hsl, Colorize as _, Theme, ThemeColor, ThemeMode, ThemeTokens};
use seance_core::schemes::{self, Scheme};

/// Index into `schemes::SCHEMES` of the scheme this GUI paints with. The
/// daemon owns the choice; [`set_scheme`] follows it.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn active() -> &'static Scheme {
    &schemes::SCHEMES[ACTIVE.load(Ordering::Relaxed)]
}

/// Light ground (ouija, riso, parlor, herbarium): widgets run in gpui-component's light mode.
pub fn is_light() -> bool {
    active().light
}

/// Hover emphasis: lighter on a dark ground, darker on a light one, so a
/// hover never fades into the background.
pub fn lift(c: Hsla, amount: f32) -> Hsla {
    if is_light() {
        c.darken(amount)
    } else {
        c.lighten(amount)
    }
}

/// Default terminal fg/bg when the scheme brings its own terminal colors;
/// `None` means `terminal.conf`'s, as before schemes.
pub fn term_fg_bg() -> Option<(u32, u32)> {
    active().term.map(|t| (t.fg, t.bg))
}

/// One scheme's chrome as `Hsla`, converted once per process.
struct Chrome {
    bg: Hsla,
    bg_elevated: Hsla,
    surface: Hsla,
    border: Hsla,
    text: Hsla,
    text_dim: Hsla,
    text_faint: Hsla,
    flame: Hsla,
    flame_dim: Hsla,
    violet: Hsla,
    violet_dim: Hsla,
    success: Hsla,
    danger: Hsla,
}

fn chrome() -> &'static Chrome {
    static ALL: OnceLock<Vec<Chrome>> = OnceLock::new();
    let all = ALL.get_or_init(|| {
        schemes::SCHEMES
            .iter()
            .map(|s| {
                if s.id == schemes::DEFAULT {
                    candlelit()
                } else {
                    from_hex(&s.chrome)
                }
            })
            .collect()
    });
    &all[ACTIVE.load(Ordering::Relaxed)]
}

/// Candlelit stays in HSL, the source of truth for docs/THEME.md. The hex in
/// `seance_core::schemes` is only its rounded twin for other clients.
fn candlelit() -> Chrome {
    Chrome {
        bg: hsl(345., 6., 7.),           // #131111
        bg_elevated: hsl(345., 8., 10.), // #1C1718
        surface: hsl(345., 8., 12.),     // #211C1D
        border: hsl(345., 10., 19.),     // #352C2E
        text: hsl(30., 27., 89.),        // #EBE3DB
        text_dim: hsl(25., 11., 61.),    // #A69A91
        text_faint: hsl(15., 6., 39.),   // #69605D
        flame: hsl(35., 80., 57.),       // #E9A03A
        flame_dim: hsl(35., 62., 41.),   // #A97328
        violet: hsl(260., 45., 70.),     // #A790D5
        violet_dim: hsl(258., 24., 47.), // #6C5B95
        success: hsl(110., 28., 60.),    // #86B67C
        danger: hsl(5., 55., 59.),       // #D0675D
    }
}

fn from_hex(c: &schemes::Chrome) -> Chrome {
    let h = |v: u32| -> Hsla { gpui::rgb(v).into() };
    Chrome {
        bg: h(c.bg),
        bg_elevated: h(c.bg_elevated),
        surface: h(c.surface),
        border: h(c.border),
        text: h(c.text),
        text_dim: h(c.text_dim),
        text_faint: h(c.text_faint),
        flame: h(c.flame),
        flame_dim: h(c.flame_dim),
        violet: h(c.violet),
        violet_dim: h(c.violet_dim),
        success: h(c.success),
        danger: h(c.danger),
    }
}

/// The active scheme's palette. Each associated fn returns a `gpui::Hsla`.
///
/// These are the load-bearing colors the seance app references directly for
/// its own chrome (pane borders, status dots, the summon glow). gpui-component
/// widgets get the same palette applied to their `Theme` in [`init`].
/// Names are candlelit's (`flame` = primary accent, `violet` = second); other
/// schemes fill the same roles with their own hues.
pub struct SeancePalette;

impl SeancePalette {
    // ── grounds ────────────────────────────────────────────────────────────
    /// The room. Window + terminal pane background.
    #[inline]
    pub fn bg() -> Hsla {
        chrome().bg
    }
    /// Lifted ground: panels, popovers, the layer above the room.
    #[inline]
    pub fn bg_elevated() -> Hsla {
        chrome().bg_elevated
    }
    /// Interactive surface: button/input/tab fills sitting on the ground.
    #[inline]
    pub fn surface() -> Hsla {
        chrome().surface
    }
    /// Hairline border between surfaces.
    #[inline]
    pub fn border() -> Hsla {
        chrome().border
    }

    // ── text ───────────────────────────────────────────────────────────────
    /// Primary text.
    #[inline]
    pub fn text() -> Hsla {
        chrome().text
    }
    /// Secondary text.
    #[inline]
    pub fn text_dim() -> Hsla {
        chrome().text_dim
    }
    /// Tertiary / disabled text.
    #[inline]
    pub fn text_faint() -> Hsla {
        chrome().text_faint
    }

    // ── accents ──────────────────────────────────────────────────────────────
    /// The primary accent (candlelit: amber candle-glow). The flame.
    #[inline]
    pub fn flame() -> Hsla {
        chrome().flame
    }
    /// Dimmed primary: hover / pressed / low-emphasis accent.
    #[inline]
    pub fn flame_dim() -> Hsla {
        chrome().flame_dim
    }
    /// The secondary highlight, the cool counterpoint to the flame.
    #[inline]
    pub fn violet() -> Hsla {
        chrome().violet
    }
    /// Dimmed secondary: hover / low-emphasis.
    #[inline]
    pub fn violet_dim() -> Hsla {
        chrome().violet_dim
    }

    // ── semantic ─────────────────────────────────────────────────────────────
    /// Success, calm affirmation.
    #[inline]
    pub fn success() -> Hsla {
        chrome().success
    }
    /// Danger.
    #[inline]
    pub fn danger() -> Hsla {
        chrome().danger
    }

    // ── session status dots ──────────────────────────────────────────────────
    /// The circle closed — gray, spent.
    #[inline]
    pub fn status_exited() -> Hsla {
        Self::text_faint()
    }
}

/// Follow the daemon's scheme choice. Re-applies the widget theme when it
/// changed and returns whether it did; the caller refreshes the windows. An
/// id this build doesn't know (a newer daemon) keeps the current look.
pub fn set_scheme(id: &str, cx: &mut App) -> bool {
    let Some(idx) = schemes::index_of(id) else {
        return false;
    };
    if ACTIVE.swap(idx, Ordering::Relaxed) == idx {
        return false;
    }
    init(cx);
    true
}

/// Activate the active scheme and apply its palette to gpui-component's
/// global `Theme` so its widgets (buttons, inputs, panels, tabs, scrollbars)
/// match seance's chrome. Re-run by [`set_scheme`] on every switch.
///
/// Must be called AFTER `gpui_component::init(cx)` (which registers the
/// `Theme`/`ThemeRegistry` globals), and before opening the window.
///
/// Mechanism: `gpui_component`'s `Theme` derefs to `ThemeColor`
/// (crates/ui/src/theme/mod.rs:97-103), so we mutate its color fields in place,
/// then regenerate `theme.tokens` — widgets paint from `theme.tokens`
/// (`ThemeTokens`, crates/ui/src/theme/theme_color.rs:347-368), which is derived
/// from `colors` and does NOT auto-update when we poke individual fields.
pub fn init(cx: &mut App) {
    // 1. Switch gpui-component into dark (or light) mode. `Theme::change`
    //    lazily creates the global if missing and calls `apply_config`,
    //    seeding every ThemeColor field from the built-in Default Dark/Light
    //    before we override the ones we care about. It also resets fonts,
    //    which is why step 4 runs again on every switch.
    //    (crates/ui/src/theme/mod.rs:163-183)
    let mode = if is_light() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, None, cx);

    let p = palette();
    let installed_fonts: std::collections::HashSet<String> =
        cx.text_system().all_font_names().into_iter().collect();
    let theme = Theme::global_mut(cx);

    // 2. Overwrite ThemeColor fields with the scheme's palette. `theme` derefs
    //    to `&mut ThemeColor`, so `theme.background` etc. address the color
    //    fields directly (crates/ui/src/theme/mod.rs:105-109).
    apply_palette(&mut theme.colors, &p);

    // 3. Regenerate the resolved tokens from the mutated colors. Widgets read
    //    `theme.tokens.<field>` (a ThemeToken = solid color + paint bg), so
    //    without this step the overrides would be invisible to components.
    //    `ThemeTokens: From<&ThemeColor>` (theme_color.rs:364-368).
    theme.tokens = ThemeTokens::from(&theme.colors);

    // 4. Font hygiene (typing-lag part 5). gpui-component's stock chrome font
    //    is ".SystemUIFont" — a macOS pseudo-family that never resolves on
    //    Linux — and its Linux mono default ("DejaVu Sans Mono") isn't
    //    installed on this box either. gpui caches the failed lookup, but
    //    RE-MATERIALIZES the error on every hit (`anyhow!("{err}")`), and
    //    with RUST_BACKTRACE=1 (which the GUI sets for crash logs) each of
    //    those captures a fresh backtrace — thousands of times per frame
    //    inside taffy text measurement. Measured: ~60ms of every 66ms
    //    Window::draw was font_id on missing families. Pick the first
    //    actually-installed candidate so the hot path is a cached Ok.
    if cfg!(not(target_os = "macos")) {
        let installed: std::collections::HashSet<String> = installed_fonts;
        let pick = |cands: &[&str]| {
            cands
                .iter()
                .find(|c| installed.contains(**c))
                .map(|c| gpui::SharedString::from(c.to_string()))
        };
        if let Some(f) = pick(&[
            "Liberation Sans",
            "Noto Sans",
            "DejaVu Sans",
            "Cantarell",
            "Ubuntu",
        ]) {
            theme.font_family = f;
        }
        if let Some(f) = pick(&[
            "JetBrainsMono Nerd Font",
            "JetBrains Mono",
            "Liberation Mono",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
        ]) {
            theme.mono_font_family = f;
        }
    }
}

// ── internal wiring ─────────────────────────────────────────────────────────

/// Snapshot of every palette color, computed once per `init`.
struct Palette {
    light: bool,
    bg: Hsla,
    bg_elevated: Hsla,
    surface: Hsla,
    border: Hsla,
    text: Hsla,
    text_dim: Hsla,
    flame: Hsla,
    flame_dim: Hsla,
    violet: Hsla,
    violet_dim: Hsla,
    success: Hsla,
    danger: Hsla,
}

fn palette() -> Palette {
    Palette {
        light: is_light(),
        bg: SeancePalette::bg(),
        bg_elevated: SeancePalette::bg_elevated(),
        surface: SeancePalette::surface(),
        border: SeancePalette::border(),
        text: SeancePalette::text(),
        text_dim: SeancePalette::text_dim(),
        flame: SeancePalette::flame(),
        flame_dim: SeancePalette::flame_dim(),
        violet: SeancePalette::violet(),
        violet_dim: SeancePalette::violet_dim(),
        success: SeancePalette::success(),
        danger: SeancePalette::danger(),
    }
}

/// Map the active palette onto every relevant `ThemeColor` field.
///
/// Fields we intentionally leave at the Default-Dark/Light seed: the chart_* /
/// base-color swatches (`red`, `green`, `blue`, `yellow`, `magenta`, `cyan`,
/// and their `_light` variants) — those are only used by data-viz widgets
/// seance doesn't ship, and the ANSI palette covers terminal color separately.
#[allow(clippy::field_reassign_with_default)]
fn apply_palette(c: &mut ThemeColor, p: &Palette) {
    // Grounds & text.
    c.background = p.bg;
    c.foreground = p.text;
    c.border = p.border;
    c.muted = p.surface;
    c.muted_foreground = p.text_dim;

    // Accent (hover backgrounds on menu/list items) → warm surface + amber text.
    c.accent = p.surface;
    c.accent_foreground = p.flame;

    // Focus ring / caret / selection — the candle picks out what has focus.
    c.ring = p.flame;
    c.caret = p.flame;
    c.selection = p.violet_dim.opacity(0.45);

    // Primary = the flame. Amber buttons, primary emphasis.
    c.primary = p.flame;
    c.primary_hover = lift(p.flame, 0.06);
    c.primary_active = p.flame_dim;
    c.primary_foreground = p.bg; // dark text on amber for contrast

    // Secondary = warm surface, understated.
    c.secondary = p.surface;
    c.secondary_hover = lift(p.surface, 0.04);
    c.secondary_active = p.surface.darken(0.03);
    c.secondary_foreground = p.text;

    // Default button chrome (neutral).
    c.button = p.surface;
    c.button_foreground = p.text;
    c.button_hover = lift(p.surface, 0.04);
    c.button_active = p.surface.darken(0.03);

    // Inputs — border-driven on dark (Theme::input_background mixes this with
    // transparent). Keep the input border warm; focus recolors via `ring`.
    c.input = p.border;

    // Links — violet, the cool counterpoint.
    c.link = p.violet;
    c.link_hover = lift(p.violet, 0.06);
    c.link_active = p.violet_dim;

    // Semantic states.
    c.success = p.success;
    c.success_foreground = p.bg;
    c.success_hover = lift(p.success, 0.06);
    c.success_active = p.success.darken(0.06);

    c.danger = p.danger;
    // Light text on red reads on a dark ground; on a light one the red is
    // the darker side, so the ground color goes on top instead.
    c.danger_foreground = if p.light { p.bg } else { p.text };
    c.danger_hover = lift(p.danger, 0.06);
    c.danger_active = p.danger.darken(0.06);

    // Info = violet family; warning = ember (keeps the two-accent discipline).
    c.info = p.violet;
    c.info_foreground = p.bg;
    c.info_hover = lift(p.violet, 0.06);
    c.info_active = p.violet_dim;

    c.warning = p.flame;
    c.warning_foreground = p.bg;
    c.warning_hover = lift(p.flame, 0.06);
    c.warning_active = p.flame_dim;

    // Popover / list / tab surfaces — the lifted layer.
    c.popover = p.bg_elevated;
    c.popover_foreground = p.text;

    c.list = p.bg;
    c.list_active = p.surface;
    c.list_active_border = p.flame_dim;
    c.list_hover = p.surface.opacity(0.6);
    c.list_head = p.bg_elevated;
    c.list_even = p.bg_elevated.opacity(0.4);

    c.tab_bar = p.bg;
    c.tab = p.bg;
    c.tab_active = p.surface;
    c.tab_foreground = p.text_dim;
    c.tab_active_foreground = p.text;
    c.tab_bar_segmented = p.bg_elevated;

    // Sidebar — the séance ledger; slightly lifted, amber-picked active items.
    c.sidebar = p.bg_elevated;
    c.sidebar_foreground = p.text_dim;
    c.sidebar_border = p.border;
    c.sidebar_accent = p.surface;
    c.sidebar_accent_foreground = p.flame;
    c.sidebar_primary = p.flame;
    c.sidebar_primary_foreground = p.bg;

    // Window chrome bars.
    c.title_bar = p.bg;
    c.title_bar_border = p.border;
    c.status_bar = p.bg_elevated;
    c.status_bar_border = p.border;
    c.window_border = p.border;

    // Scrollbars — barely-there on the ground, amber-warm thumb.
    c.scrollbar = p.bg;
    c.scrollbar_thumb = p.border;
    c.scrollbar_thumb_hover = p.flame_dim;

    // Overlays / drag / skeleton.
    c.overlay = p.bg.opacity(0.6);
    c.drop_target = p.flame.opacity(0.12);
    c.drag_border = p.flame;
    c.skeleton = p.surface;

    // Progress / sliders / switches — flame-driven.
    c.progress_bar = p.flame;
    c.slider_bar = p.flame;
    c.slider_thumb = p.text;
    c.switch = p.surface;
    c.switch_thumb = p.text;

    // Tiles (dock-able pane grid) — the ground.
    c.tiles = p.bg;
    c.table = p.bg;
    c.table_head = p.bg_elevated;
    c.table_head_foreground = p.text_dim;
    c.table_row_border = p.border;
    c.table_hover = p.surface.opacity(0.6);
    c.table_active = p.surface;
    c.table_active_border = p.flame_dim;
}

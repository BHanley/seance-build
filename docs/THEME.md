# seance — theme & branding

seance ships twelve color schemes (see [Schemes](#schemes)); the default, and
the one this page's palette tables describe, is **candlelit**.

Aesthetic: **candlelit**. A dark room lit by one candle — deep warm charcoal
grounds, amber candle-glow accents, muted violet highlights. Elegant and
understated, not gothic kitsch. The séance metaphor (a circle of agents and a
human working in one shared space) is carried by two accents only: the
**flame** (amber — presence / human attention) and the **violet** counterpoint
(agent affordances: ghost propose, notes, history).

All colors are `gpui::Hsla`. Candlelit's source of truth is the HSL in `src/theme.rs`;
hex below is computed from that HSL (sRGB round). Authored with
`gpui_component::hsl(h: 0..360, s: 0..100, l: 0..100)`.

## Core palette

| token            | HSL              | hex       | role |
|------------------|------------------|-----------|------|
| `bg`             | 345°  6%  7%     | `#131111` | the room — window + terminal pane ground |
| `bg_elevated`    | 345°  8% 10%     | `#1C1718` | lifted layer — panels, popovers, sidebar |
| `surface`        | 345°  8% 12%     | `#211C1D` | interactive fills — buttons, inputs, tabs |
| `border`         | 345° 10% 19%     | `#352C2E` | warm hairline separators |
| `text`           |  30° 27% 89%     | `#EBE3DB` | primary text — warm off-white |
| `text_dim`       |  25° 11% 61%     | `#A69A91` | secondary text — muted parchment |
| `text_faint`     |  15°  6% 39%     | `#69605D` | tertiary / disabled |
| `flame`          |  35° 80% 57%     | `#E9A03A` | **primary accent** — amber candle-glow |
| `flame_dim`      |  35° 62% 41%     | `#A97328` | dimmed ember — amber hover/active |
| `violet`         | 260° 45% 70%     | `#A790D5` | **secondary accent** — muted violet |
| `violet_dim`     | 258° 24% 47%     | `#6C5B95` | dimmed violet |
| `success`        | 110° 28% 60%     | `#86B67C` | soft sage — calm affirmation |
| `danger`         |   5° 55% 59%     | `#D0675D` | warm muted red — doesn't scream |

### Session status dots

| token             | maps to     | meaning |
|-------------------|-------------|---------|
| `status_exited`   | `text_faint`| process left the circle |

> `status_running` / `status_idle` were removed 2026-07-22 (dead code); live
> "working" state is drawn from observed TUI title spinners, not theme tokens.

## Schemes

Every scheme defines the same chrome tokens as the table above (`flame` is
its primary accent, `violet` its second) plus, except candlelit, a terminal
palette. The table lives in `crates/seance-core/src/schemes.rs` (packed
`0xRRGGBB`, wasm-safe, so the web client can read it later);
`SeancePalette::*()` returns the active scheme's color, so call sites never
name a scheme.

| id | label | ground | accent | second |
|----|-------|--------|--------|--------|
| `candlelit` | Candlelit (default) | `#131111` | `#E9A03A` | `#A790D5` |
| `abyssal` | Abyssal | `#0A1018` | `#3FE0D0` | `#FF8A6B` |
| `ectoplasm` | Ectoplasm | `#0B0F0C` | `#7CFF9E` | `#D6B8FF` |
| `ouija` | Ouija (light) | `#F3EBDD` | `#8E2A1F` | `#2F3F7A` |
| `monokai` | Monokai Seance | `#19181A` | `#FFD866` | `#AB9DF2` |
| `tarot` | Tarot | `#120D1C` | `#E4B94C` | `#E0457B` |
| `mojave` | Mojave Dusk | `#1E1416` | `#FF8C42` | `#3FC1B0` |
| `riso` | Riso (light) | `#F7F4EC` | `#E6007E` | `#00838A` |
| `synth` | Synth Seance | `#0D0B1F` | `#FF3DCB` | `#2DE2FF` |
| `fog` | Fog | `#3B414D` | `#F2B56B` | `#B7A8F0` |
| `parlor` | Parlor (light) | `#CDC3B2` | `#8A2419` | `#24527A` |
| `herbarium` | Herbarium (light) | `#BDC5B3` | `#8E3B14` | `#43407F` |

**Who owns the choice.** The daemon. It saves it in the state dir
(`theme.json`, `{"scheme": ID}`), sends it to each window on attach and
broadcasts `GuiEvent::Theme` on every switch, so the Mac thin client and the
daemon host's GUI change together. At boot the saved choice wins, then `seance-theme`
in `terminal.conf`, then candlelit; an unknown id falls back to candlelit.

**Switching.** The ✦ menu at the top of the rail lists the twelve, dark ones then
light ones under their own headings, active one marked; `seance ctl theme` lists them and `seance ctl theme ID` switches (an
unknown ID is refused with the list). A window that hears the switch sets
the active scheme, re-runs `theme::init` and refreshes every window.

**Terminal colors.** Cells are resolved daemon-side, so on a switch the
daemon swaps its palette (`pty_session::set_term_colors`), recolors each live
pane's seeded palette (slots a program set itself via OSC 4/10/11 are kept)
and re-sends every pane as a full frame, idle panes included. OSC color
queries answer from the active palette. The 16 ANSI slots come from each
scheme's named colors: 1 to 6 and 9 to 14 are its red, green, yellow, blue,
magenta, cyan; 8 is its gray; on dark schemes 0 = `bg_elevated`, 7 =
`text_dim`, 15 = `text`, on light schemes 0 = `text`, 7 = `surface`, 15 =
`bg_elevated`. Cursor = accent. Candlelit keeps `terminal.conf` exactly.

**Light schemes** (ouija, riso, parlor, herbarium) put gpui-component in `ThemeMode::Light`
before the palette is applied, use the light code-highlight theme in
markdown panes, and put the ground color on top of filled red (danger). Hovers use
`theme::lift`, which darkens instead of lightening on a light ground.

**Limits.** Sleeping panes are recolored by swapping exact old palette
colors in their frozen frame, so a program's own truecolor stays as it was.
A program that asked for the background once at startup (claude does, via
OSC 11, to pick its own dark or light look) keeps that pick until it
restarts. The web client's chrome stays candlelit for now; its cells follow
the scheme because the daemon resolves them.

## ANSI terminal palette (16-color)

**Not this module** (for candlelit; other schemes bring their own, see
above). Agent CLIs and shells render inside the embedded terminal
against a 16-color palette resolved daemon-side from `src/term_config.rs`.
The default is the ghostty base16 set (`#181818` bg / `#d8d8d8` fg,
`#ab4642 #a1b56c #f7ca88 #7cafc2 #ba8baf #86c1b9` …);
`~/.config/seance/terminal.conf` (Ghostty syntax: `theme`, `palette`,
`background`, `foreground`, `cursor-color`) replaces it, and the program can
still override it via OSC 4/10/11. The seance-tinted ANSI table that used to live here
described `theme.rs::ansi_palette()`, which was removed 2026-07-22 with the
dead local-PTY path — nothing read it. Change terminal colors in
`terminal.conf`; change chrome colors here.

## Public API (`src/theme.rs`)

```rust
pub struct SeancePalette;                 // color accessors, each -> gpui::Hsla
impl SeancePalette {
    pub fn bg() -> Hsla;            pub fn bg_elevated() -> Hsla;
    pub fn surface() -> Hsla;       pub fn border() -> Hsla;
    pub fn text() -> Hsla;          pub fn text_dim() -> Hsla;
    pub fn text_faint() -> Hsla;
    pub fn flame() -> Hsla;         pub fn flame_dim() -> Hsla;
    pub fn violet() -> Hsla;        pub fn violet_dim() -> Hsla;
    pub fn success() -> Hsla;       pub fn danger() -> Hsla;
    pub fn status_exited() -> Hsla;
}

pub fn init(cx: &mut gpui::App);             // apply the active scheme (dark or light) + overrides
pub fn set_scheme(id: &str, cx: &mut gpui::App) -> bool; // follow the daemon; re-inits on change
pub fn active() -> &'static Scheme;          // seance_core::schemes entry in use
pub fn is_light() -> bool;
pub fn term_fg_bg() -> Option<(u32, u32)>;   // scheme terminal fg/bg, None = terminal.conf
```

Colors are associated **functions** (not consts) because they follow the
active scheme. Each scheme's chrome is converted to `Hsla` once per process;
a call is an atomic load and an index.

## How `init()` applies the theme

Call order in `main`: `gpui_component::init(cx)` → **`theme::init(cx)`** →
open window.

`init()` does four steps (see `src/theme.rs::init`); step 4 is font-family
hygiene on non-macOS — pick the first installed sans/mono rather than letting
gpui probe, which is load-bearing for typing latency:

1. **`Theme::change(ThemeMode::Dark, None, cx)`** (`Light` for a light
   scheme): activates the mode.
   `Theme::change` lazily creates the `Theme` global if absent and calls
   `apply_config(dark_theme)`, seeding every `ThemeColor` field from the
   built-in *Default Dark* before we override.
   — `crates/ui/src/theme/mod.rs:163-183`
2. **Mutate `Theme::global_mut(cx).colors`** — `Theme` derefs to `ThemeColor`
   and `colors` is a public field, so we overwrite ~70 relevant fields with the
   candlelit palette (`apply_palette`).
   — `crates/ui/src/theme/mod.rs:97-109` (Deref/DerefMut), field list at
   `crates/ui/src/theme/theme_color.rs:57-345`
3. **`theme.tokens = ThemeTokens::from(&theme.colors)`** — widgets paint from
   the resolved `ThemeTokens` (color + paint background), which does **not**
   auto-recompute when we poke individual `colors` fields. This regeneration is
   the load-bearing step — skip it and the overrides are invisible to
   components.
   — `ThemeTokens` def `theme_color.rs:347-368`; `From<&ThemeColor>`
   `theme_color.rs:364-368`; same pattern used internally at `mod.rs:234` and
   `schema.rs:939`.

Color transforms (`.opacity/.lighten/.darken`) come from `gpui_component`'s
`Colorize` trait (`crates/ui/src/theme/color.rs:19-61`).

### Field-mapping decisions

- **`primary` = flame**; `warning` also = flame (keeps the app to two accents).
- **`info` / `link` = violet**; violet is the only cool accent.
- `primary_foreground` / `success_foreground` / `warning_foreground` = `bg`
  (dark text on the bright amber/sage for contrast).
- `ring` / `caret` = flame — the candle picks out what has focus.
- `selection` = `violet_dim` @ 45% alpha.
- Inputs are border-driven on dark: gpui-component's `Theme::input_background()`
  mixes `input` with transparent, so we set `input = border` and let focus
  recolor via `ring` (`mod.rs:189-196`).

## Uncertainty flags for the integrator

1. **hex is computed, HSL is canonical.** The table hex is a Python
   `colorsys` sRGB round-trip of the HSL in `theme.rs`; gpui's own HSL→sRGB
   conversion may differ by ±1 per channel. Trust the on-screen render / the
   HSL, not the hex.
2. **`apply_palette` covers the fields seance uses.** The `chart_*` swatches and
   the base `red/green/blue/yellow/magenta/cyan(+_light)` `ThemeColor` fields are
   left at the Default-Dark seed — they only feed data-viz widgets seance
   doesn't ship, and terminal color is resolved daemon-side
   (ghostty palette in `runtime/pty_session.rs`), not by the theme module.
   If a component you add reads those, extend `apply_palette`.
   (`ansi_palette()` was removed 2026-07-22 with the dead local-PTY path.)
3. **No `window_border` on non-Linux**, per gpui-component docs
   (`theme_color.rs:314-319`). seance is Linux-only, so this is fine; we set it
   to `border`.
4. **`Theme::change` re-seeds from Default Dark, then we override.** If a future
   gpui-component rev renames/removes a `ThemeColor` field this file sets, that
   line won't compile — the field list is pinned to rev
   `b5eef62336f88bb6c1ee45bf32f73c9895d49f8d`. Verified against the local
   checkout, not training data.
5. **The web client re-declares this palette by hand.** `crates/seance-web`
   has no access to `theme.rs`: the same values appear as GL constants
   (`renderer.rs`) and as hex literals in the chrome CSS (`ui.rs`, `help.rs`,
   `activity.rs`, `menus.rs`, `probe.rs`). Values currently match; there is no
   compile-time link, so a change here must be mirrored there by hand.
```

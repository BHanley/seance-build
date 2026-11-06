//! A tiny pixel creature per circle, drawn on the circle's lead pane (its
//! title strip and Stage's lead card). Generated from the circle slug, so a
//! circle keeps its creature on every machine; `seance-lead-sprite.<slug> = SEED`
//! in terminal.conf swaps in another.
//!
//! 10x10 and mirror-symmetric like an identicon: a body silhouette, two dark
//! eyes inside it, legs on the bottom rows and sometimes ears or an antenna.

use gpui::{div, prelude::*, px, rgb, AnyElement};

pub(super) const SIZE: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cell {
    Empty,
    Body,
    Eye,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Sprite {
    pub cells: [[Cell; SIZE]; SIZE],
    /// Index into [`BODY`].
    pub color: usize,
    /// Where this creature is in its dance, so circles don't move in step.
    pub phase: u64,
}

/// Muted body colors that sit with the candlelit palette: sage, dusty blue,
/// mauve, sand, clay, teal, rose, slate.
const BODY: [u32; 8] = [
    0x8fa67a, 0x7a95b0, 0xa38bb0, 0xb8a27a, 0xb0846e, 0x6fa3a0, 0xb07f8a, 0x8a92a3,
];
const INK: u32 = 0x1b1c18;

/// FNV-1a: stable across runs and machines, unlike std's hasher.
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// xorshift64, seeded from the hash.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// 0..n
    fn below(&mut self, n: u64) -> usize {
        (self.next() % n) as usize
    }
}

/// The creature for `seed` (a circle slug, or its override).
pub(super) fn generate(seed: &str) -> Sprite {
    let mut rng = Rng(fnv1a(seed) | 1);
    let mut cells = [[Cell::Empty; SIZE]; SIZE];
    // Paint the left half (columns 0..5, centre at 4) and mirror it.
    let mut set = |row: usize, half: usize, cell: Cell| {
        cells[row][4 - half] = cell;
        cells[row][5 + half] = cell;
    };

    // Body: rows 2..8, a half-width of 2..=4 that drifts a step per row,
    // at least 3 on the eye rows so the eyes sit inside it.
    let mut widths = [0usize; SIZE];
    let mut w = 2 + rng.below(3);
    for (row, slot) in widths.iter_mut().enumerate().take(8).skip(2) {
        *slot = if matches!(row, 3 | 4) { w.max(3) } else { w };
        for half in 0..*slot {
            set(row, half, Cell::Body);
        }
        w = (w as i64 + rng.below(3) as i64 - 1).clamp(2, 4) as usize;
    }

    // Eyes on row 3 or 4, with body on both sides of each.
    let eye_row = 3 + rng.below(2);
    let eye = 1 + rng.below(widths[eye_row] as u64 - 2);
    set(eye_row, eye, Cell::Eye);

    // Top: nothing, ears at the top row's edge, or an antenna in the middle.
    match rng.below(3) {
        1 => {
            set(1, widths[2] - 1, Cell::Body);
            if rng.below(2) == 0 {
                set(0, widths[2] - 1, Cell::Body);
            }
        }
        2 => {
            set(1, 0, Cell::Body);
            set(0, rng.below(2), Cell::Body);
        }
        _ => {}
    }

    // Legs under the body, with feet that sometimes turn out.
    let leg = 1 + rng.below(widths[7] as u64 - 1);
    set(8, leg, Cell::Body);
    set(9, leg, Cell::Body);
    if rng.below(2) == 0 {
        set(9, leg + 1, Cell::Body);
    }

    Sprite {
        cells,
        color: rng.below(BODY.len() as u64),
        phase: fnv1a(seed),
    }
}

/// Dance moves while the circle works, as (x, y) cell offsets per beat. The
/// first is the basic step (rest, hop left, rest, hop right); every few bars
/// a creature throws in one of the others at random (a user's ask).
const MOVES: [[(f32, f32); 4]; 5] = [
    [(0., 0.), (-1., -1.), (0., 0.), (1., -1.)],
    // jump
    [(0., 0.), (0., -1.), (0., -2.), (0., -1.)],
    // shimmy
    [(-1., 0.), (1., 0.), (-1., 0.), (1., 0.)],
    // slide left and back
    [(-1., 0.), (-2., 0.), (-1., 0.), (0., 0.)],
    // bounce
    [(0., -1.), (0., 0.), (0., -1.), (0., 0.)],
];

/// This creature's offset at beat `beat`: two-beat bars of the basic step,
/// with a random move in about one bar in four.
fn dance_step(phase: u64, beat: u64) -> (f32, f32) {
    let beat = beat.wrapping_add(phase);
    let bar = beat / 4;
    let roll = (bar ^ phase).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 60;
    let mv = if roll < 4 {
        1 + (roll as usize) % (MOVES.len() - 1)
    } else {
        0
    };
    MOVES[mv][(beat % 4) as usize]
}

/// Poking the creature (a click on it in Chat's header): one dance move and a
/// line each time, and five pokes within [`POKE_WINDOW_MS`] throw a party.
pub(super) const DANCE_MS: u64 = 960;
pub(super) const LINE_MS: u64 = 2400;
pub(super) const PARTY_MS: u64 = 7000;
const POKE_WINDOW_MS: u64 = 3000;
const PARTY_POKES: usize = 5;
pub(super) const PARTY_LINE: &str = "A seance party! The spirits are out.";
const LINES: [&str; 10] = [
    "Who summoned me?",
    "The spirits say: ship it.",
    "Knock once for yes.",
    "I saw nothing.",
    "Boo.",
    "The lamp is warm tonight.",
    "Your tokens are in another castle.",
    "I only dance when you're not looking.",
    "Have you tried /compact?",
    "The veil is thin. So is the server's RAM.",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Poke {
    /// Play `MOVES[mv]` and say `line`.
    Dance { mv: usize, line: &'static str },
    /// A party scene over Chat for [`PARTY_MS`], and every creature dances.
    Party,
}

/// Recent pokes (unix ms).
#[derive(Default)]
pub(super) struct Pokes(Vec<u64>);

impl Pokes {
    pub(super) fn poke(&mut self, now: u64) -> Poke {
        self.0.retain(|t| now.saturating_sub(*t) <= POKE_WINDOW_MS);
        self.0.push(now);
        if self.0.len() >= PARTY_POKES {
            self.0.clear();
            return Poke::Party;
        }
        let mut rng = Rng(now.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        Poke::Dance {
            mv: rng.below(MOVES.len() as u64),
            line: LINES[rng.below(LINES.len() as u64)],
        }
    }
}

/// The creature's own body color.
pub(super) fn body_color(sprite: &Sprite) -> gpui::Hsla {
    rgb(BODY[sprite.color]).into()
}

/// When the party ends (unix ms); every creature dances until then.
static PARTY_UNTIL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn start_party(now: u64) {
    PARTY_UNTIL.store(now + PARTY_MS, std::sync::atomic::Ordering::Relaxed);
}

/// A click ended the party early.
pub(super) fn stop_party() {
    PARTY_UNTIL.store(0, std::sync::atomic::Ordering::Relaxed);
}

fn partying(now: u64) -> bool {
    now < PARTY_UNTIL.load(std::sync::atomic::Ordering::Relaxed)
}

/// The sprite as a grid of `cell`-point squares. Whole device pixels keep it
/// crisp: 1.5 and 2.5 points land on 3 and 5 pixels on a 2x screen.
/// `dancing` steps it through [`MOVES`] on the wall clock; the sidebar spinner's
/// 240ms tick already re-renders while any circle works, so it costs no frames.
pub(super) fn render(sprite: &Sprite, cell: f32, dancing: bool) -> AnyElement {
    render_move(sprite, cell, dancing, None)
}

/// [`render`], and with `poked` (a move and when it began) the creature plays
/// that move once, on any circle, working or not. A party dances them all.
pub(super) fn render_move(
    sprite: &Sprite,
    cell: f32,
    dancing: bool,
    poked: Option<(usize, u64)>,
) -> AnyElement {
    let body = rgb(BODY[sprite.color]);
    let ink = rgb(INK);
    let now = super::util::now_ms();
    let (dx, dy) = match poked {
        Some((mv, start)) if now.saturating_sub(start) < DANCE_MS => {
            MOVES[mv][(now.saturating_sub(start) / 240 % 4) as usize]
        }
        _ if dancing || partying(now) => dance_step(sprite.phase, now / 240),
        _ => (0., 0.),
    };
    div()
        .flex_none()
        .relative()
        .left(px(dx * cell))
        .top(px(dy * cell))
        .flex()
        .flex_col()
        .children(sprite.cells.iter().map(|row| {
            div().flex().children(row.iter().map(|c| {
                let d = div().flex_none().size(px(cell));
                match c {
                    Cell::Empty => d,
                    Cell::Body => d.bg(body),
                    Cell::Eye => d.bg(ink),
                }
            }))
        }))
        .into_any_element()
}

/// Two or more panes working dance as a crew (a user's ask): one paper cutout
/// per pane, the same creature in its own shade, running through a random
/// string of moves. gpui has no 3D, so this fakes the web Chat's CSS ring with
/// its numbers. Cutout cells are 2 points to its 2px, so its px read as points
/// here.
const CREW_CELL: f32 = 2.;
const CREW_MAX: usize = 6;
/// A step toward white (positive) or black per crew index, wrapping past
/// six, so a dancer keeps its shade wherever it is in the crew. 0 is the
/// circle's own color.
const CREW_SHADES: [f32; CREW_MAX] = [0., 0.3, -0.3, 0.55, -0.5, 0.75];
const SHEET_MS: u64 = 1700;
/// Each sheet runs this far ahead of the one before it.
const SHEET_STAGGER_MS: u64 = 450;
const SWAY_DEG: f32 = 40.;
const HOP: f32 = 3.;
const PERSPECTIVE: f32 = 170.;
/// The party ring sits farther back on the web (`#party { perspective: 500px }`).
const PARTY_PERSPECTIVE: f32 = 500.;
/// Seconds to walk from one move to the next.
const CREW_BLEND: f32 = 0.9;
/// The first move change comes this long after the crew forms, the rest
/// [`MOVE_SECS`] apart.
const FIRST_MOVE_SECS: f32 = 4.;
const MOVE_SECS: std::ops::RangeInclusive<f32> = 3.5..=6.5;
const PARTY_MOVE_SECS: f32 = 1.5;

/// The crew's moves, the web Chat's `CREW_MOVES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CrewMove {
    Circle,
    Eight,
    Conga,
    Kickline,
    Totem,
    Dosado,
    Leapfrog,
    Burst,
    Cartwheel,
    Swing,
    Whirl,
    Flip,
}

const CREW_MOVES: [CrewMove; 12] = [
    CrewMove::Circle,
    CrewMove::Eight,
    CrewMove::Conga,
    CrewMove::Kickline,
    CrewMove::Totem,
    CrewMove::Dosado,
    CrewMove::Leapfrog,
    CrewMove::Burst,
    CrewMove::Cartwheel,
    CrewMove::Swing,
    CrewMove::Whirl,
    CrewMove::Flip,
];

/// Where a move puts one sheet: x right, y down, z toward the viewer, in
/// points from the ring's centre; ry and rz in degrees.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Place {
    x: f32,
    y: f32,
    z: f32,
    ry: f32,
    rz: f32,
}

/// Sheet `i` of `n` in `mv` at `t` seconds into the dance, on a ring of
/// radius `r`.
fn crew_place(mv: CrewMove, i: usize, n: usize, t: f32, r: f32) -> Place {
    use std::f32::consts::{PI, TAU};
    let (fi, fne) = (i as f32, n as f32);
    // -1 to 1 along a line.
    let spread = if n > 1 { 2. * fi / (fne - 1.) - 1. } else { 0. };
    let p = |x, y, z| Place {
        x,
        y,
        z,
        ..Place::default()
    };
    match mv {
        CrewMove::Circle => {
            let a = t * 1.25 + TAU * fi / fne;
            p(r * a.sin(), r * 0.34 * a.cos(), r * a.cos())
        }
        CrewMove::Eight => {
            let a = t * 1.4 + TAU * fi / fne;
            p(r * a.sin(), 7. * (2. * a).sin(), r * 0.6 * a.cos())
        }
        CrewMove::Conga => {
            let a = t * 1.2 - fi * 0.7;
            p(r * a.sin(), 5. * (t * 3. - fi * 1.2).sin(), 12. * a.cos())
        }
        CrewMove::Kickline => {
            let a = t * 6. - fi * 0.9;
            Place {
                rz: 12. * a.sin(),
                ..p(spread * r, 3. - 12. * a.sin().max(0.), 0.)
            }
        }
        CrewMove::Totem => Place {
            rz: 6. * (t * 3. + fi).sin(),
            ..p(
                4. * (t * 3. + fi).sin(),
                (fi - (fne - 1.) / 2.) * (34. / fne).min(9.),
                fi * 7.,
            )
        },
        CrewMove::Dosado => {
            let pairs = n.div_ceil(2);
            let c = if pairs > 1 {
                ((i / 2) as f32 / (pairs - 1) as f32 * 2. - 1.) * r * 0.7
            } else {
                0.
            };
            let a = t * 2.4 + if i % 2 == 1 { PI } else { 0. };
            p(c + 11. * a.cos(), 2. * a.sin(), 12. * a.sin())
        }
        CrewMove::Leapfrog => {
            let at = (t * 0.55 + fi / fne) % 1.;
            if at < 0.7 {
                return p((at / 0.7 * 2. - 1.) * r, 6., 0.);
            }
            let q = (at - 0.7) / 0.3;
            let up = (PI * q).sin();
            Place {
                rz: -360. * q,
                ..p((1. - q * 2.) * r, 6. - 24. * up, -10. * up)
            }
        }
        CrewMove::Burst => {
            let a = TAU * fi / fne;
            let k = 0.3 + 0.7 * (t * 2.).sin().abs();
            Place {
                rz: 15. * (t * 2.).sin(),
                ..p(r * k * a.sin(), -12. * k * a.cos(), r * k * a.cos() * 0.5)
            }
        }
        CrewMove::Cartwheel => {
            let x = r * (t * 1.2 + spread).sin();
            Place {
                rz: x * 16.,
                ..p(x, 2., spread * 14.)
            }
        }
        CrewMove::Swing => {
            let a = 1.1 * (t * 2.4 + fi * 0.35).sin();
            Place {
                rz: -a * 40.,
                ..p(r * a.sin() + spread * 6., 9. - 16. * a.cos(), fi * 5.)
            }
        }
        CrewMove::Whirl => {
            let a = t * 2.4 + TAU * fi / fne;
            let k = 0.55 + 0.45 * (t * 1.3).sin();
            Place {
                ry: t * 180. % 360.,
                ..p(r * k * a.sin(), r * k * 0.34 * a.cos(), r * k * a.cos())
            }
        }
        CrewMove::Flip => Place {
            ry: (t * 150. + fi * 70.) % 360.,
            ..p(spread * r, 4. * (t * 3. + fi).sin(), 0.)
        },
    }
}

/// Which move the crew is dancing and when it changes, as the web Chat's
/// `crewTick` keeps it. Starts on the circle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CrewDance {
    /// When the crew formed (unix ms); times below are seconds since then.
    epoch: u64,
    /// The move being left; dropped once the blend is over.
    from: CrewMove,
    to: CrewMove,
    began: f32,
    next: f32,
    /// The party's: a new move every [`PARTY_MOVE_SECS`].
    fast: bool,
}

impl CrewDance {
    pub(super) fn new(now: u64) -> Self {
        Self {
            epoch: now,
            from: CrewMove::Circle,
            to: CrewMove::Circle,
            began: 0.,
            next: FIRST_MOVE_SECS,
            fast: false,
        }
    }

    /// The party's dance, the web's `data-fast` ring.
    fn fast(now: u64) -> Self {
        Self {
            next: PARTY_MOVE_SECS,
            fast: true,
            ..Self::new(now)
        }
    }

    fn secs(&self, now: u64) -> f32 {
        now.saturating_sub(self.epoch) as f32 / 1000.
    }

    /// Moves on to a new move, never the one just danced, once this one's
    /// time is up.
    pub(super) fn advance(&mut self, now: u64, rng: &mut impl rand::Rng) {
        let t = self.secs(now);
        if t >= self.next {
            self.from = self.to;
            self.began = t;
            self.next = t + if self.fast {
                PARTY_MOVE_SECS
            } else {
                rng.gen_range(MOVE_SECS)
            };
            let at = CREW_MOVES.iter().position(|m| *m == self.to).unwrap_or(0);
            let k = rng.gen_range(0..CREW_MOVES.len() - 1);
            self.to = CREW_MOVES[if k >= at { k + 1 } else { k }];
        } else if self.blend(t) >= 1. {
            self.from = self.to;
        }
    }

    /// How far from the old move to the new at `t`, eased: 0 to exactly 1.
    fn blend(&self, t: f32) -> f32 {
        let b = ((t - self.began) / CREW_BLEND).clamp(0., 1.);
        b * b * (3. - 2. * b)
    }

    /// Sheet `i` of `n` at `now`, between the old move and the new.
    fn place(&self, i: usize, n: usize, now: u64, r: f32) -> Place {
        let t = self.secs(now);
        let c = crew_place(self.to, i, n, t, r);
        let w = self.blend(t);
        if self.from == self.to || w >= 1. {
            return c;
        }
        let a = crew_place(self.from, i, n, t, r);
        let mix = |a: f32, c: f32| a + (c - a) * w;
        Place {
            x: mix(a.x, c.x),
            y: mix(a.y, c.y),
            z: mix(a.z, c.z),
            ry: mix(a.ry, c.ry),
            rz: mix(a.rz, c.rz),
        }
    }
}

/// One cutout of the crew at a moment.
#[derive(Debug, PartialEq)]
pub(super) struct Cutout {
    /// Its crew index: a pane's place in the circle, or a subagent's number
    /// after the panes.
    pub pane: u32,
    /// Its centre from the ring's on screen, in points (y down), perspective
    /// and hop included.
    pub x: f32,
    pub y: f32,
    /// Toward the viewer, in points: what draws over what.
    pub z: f32,
    /// 1 at the front of the ring, -1 at the back.
    pub near: f32,
    /// Perspective: above 1 in front, below behind.
    pub scale: f32,
    /// How far the sheet is turned on its own, in radians; it squeezes by
    /// the cosine.
    pub sway: f32,
    /// The move's turn in the sheet's plane and about its upright, radians.
    pub rz: f32,
    pub ry: f32,
    /// Its [`CREW_SHADES`] step.
    pub shade: f32,
}

impl Cutout {
    /// On screen, one cell's step along the sheet's rows and down its
    /// columns: the sway squeezes it, rz turns it, ry squeezes it again
    /// (past a quarter turn, mirrored), as the web's transforms nest.
    fn axes(&self) -> ((f32, f32), (f32, f32)) {
        let k = CREW_CELL * self.scale;
        let (sin, cos) = self.rz.sin_cos();
        let (squeeze, sway) = (self.ry.cos(), self.sway.cos());
        (
            (k * sway * cos * squeeze, k * sway * sin),
            (-k * sin * squeeze, k * cos),
        )
    }
}

fn crew_radius(n: usize) -> f32 {
    (4.2 * n as f32).max(15.)
}

/// Whether these dancers (busy panes, then running subagents) make a crew:
/// two or more.
pub(super) fn is_crew(crew: &[u32]) -> bool {
    crew.len() >= 2
}

/// One dance per circle whose crew is two or more, as the web Chat keeps one
/// per list row: made when the crew forms, kept while it lasts, dropped when
/// it falls under two or the circle goes.
pub(super) fn keep_crew_dances<'a>(
    dances: &mut std::collections::HashMap<String, CrewDance>,
    crews: impl IntoIterator<Item = (&'a str, &'a [u32])>,
    now: u64,
) {
    let mut kept = std::collections::HashMap::new();
    for (ws, crew) in crews {
        if is_crew(crew) {
            let dance = dances.remove(ws).unwrap_or_else(|| CrewDance::new(now));
            kept.insert(ws.to_string(), dance);
        }
    }
    *dances = kept;
}

/// The first [`CREW_MAX`] of `crew` (crew indexes) dancing `dance` at `now`
/// (unix ms), back to front so nearer cutouts are drawn over farther ones.
/// Empty under two.
pub(super) fn crew_at(crew: &[u32], dance: &CrewDance, now: u64) -> Vec<Cutout> {
    if !is_crew(crew) {
        return Vec::new();
    }
    let crew: Vec<(u32, f32)> = crew
        .iter()
        .take(CREW_MAX)
        .map(|&pane| (pane, CREW_SHADES[pane as usize % CREW_SHADES.len()]))
        .collect();
    place_cuts(&crew, dance, now, crew_radius(crew.len()), PERSPECTIVE)
}

/// Each (crew index, shade) of `crew` dancing `dance` at `now` on a ring of
/// radius `r`, back to front.
fn place_cuts(
    crew: &[(u32, f32)],
    dance: &CrewDance,
    now: u64,
    r: f32,
    perspective: f32,
) -> Vec<Cutout> {
    let n = crew.len();
    let tau = std::f32::consts::TAU;
    let ms = now.saturating_sub(dance.epoch);
    let mut cuts: Vec<Cutout> = crew
        .iter()
        .enumerate()
        .map(|(i, &(pane, shade))| {
            let at = dance.place(i, n, now, r);
            let scale = perspective / (perspective - at.z);
            // Sway side to side, hopping at each upright pass.
            let u = ((ms + i as u64 * SHEET_STAGGER_MS) % SHEET_MS) as f32 / SHEET_MS as f32;
            let sway = -SWAY_DEG.to_radians() * (tau * u).cos();
            let hop = -HOP * (tau * u).sin().powi(2);
            let mut cut = Cutout {
                pane,
                x: at.x * scale,
                y: at.y * scale,
                z: at.z,
                near: (at.z / r).clamp(-1., 1.),
                scale,
                sway,
                rz: at.rz.to_radians(),
                ry: at.ry.to_radians(),
                shade,
            };
            // The hop is up the sheet, wherever the move has turned it.
            let (_, down) = cut.axes();
            cut.x += down.0 * hop / CREW_CELL;
            cut.y += down.1 * hop / CREW_CELL;
            cut
        })
        .collect();
    cuts.sort_by(|a, b| a.z.total_cmp(&b.z));
    cuts
}

/// `color` a `step` of the way to white (positive) or black (negative).
fn shade_rgb(color: u32, step: f32) -> u32 {
    let to = if step > 0. { 255. } else { 0. };
    [16, 8, 0].iter().fold(0, |out, shift| {
        let v = ((color >> shift) & 0xff) as f32;
        out << 8 | (v + (to - v) * step.abs()).round() as u32
    })
}

/// A crew from [`crew_at`]'s `cuts`, `zoom` times the header's size. `poked`
/// moves the whole ring through that move, as [`render_move`] does one
/// creature.
pub(super) fn render_crew(
    sprite: &Sprite,
    cuts: &[Cutout],
    poked: Option<(usize, u64)>,
    zoom: f32,
) -> AnyElement {
    // As tall as the lone creature at the header's size, so it keeps its
    // height.
    let height = SIZE as f32 * 2.5 * zoom;
    let width = (2. * crew_radius(cuts.len()) + SIZE as f32 * CREW_CELL) * zoom;
    let now = super::util::now_ms();
    let (dx, dy) = match poked {
        Some((mv, start)) if now.saturating_sub(start) < DANCE_MS => {
            MOVES[mv][(now.saturating_sub(start) / 240 % 4) as usize]
        }
        _ => (0., 0.),
    };
    crew_box(std::slice::from_ref(sprite), cuts, width, height, zoom)
        .left(px(dx * 2.5 * zoom))
        .top(px(dy * 2.5 * zoom))
        .into_any_element()
}

/// `cuts` drawn in a `width` by `height` box, each from `sprites` by its
/// crew index (wrapping).
fn crew_box(sprites: &[Sprite], cuts: &[Cutout], width: f32, height: f32, zoom: f32) -> gpui::Div {
    // Half points are whole device pixels on a 2x screen, so cells meet
    // without seams however the sheet is squeezed.
    let snap = |v: f32| (v * 2.).round() / 2.;
    let ink = gpui::Hsla::from(rgb(INK));
    let mut cells = Vec::new();
    for cut in cuts {
        let sprite = &sprites[cut.pane as usize % sprites.len()];
        let body = gpui::Hsla::from(rgb(shade_rgb(BODY[sprite.color], cut.shade)));
        // Farther cutouts a little darker, for depth.
        let body = gpui::Hsla {
            l: body.l * (0.9 + 0.1 * cut.near),
            ..body
        };
        // gpui cannot turn a div, so each cell stays upright and only its
        // centre turns with the sheet; it covers its turned square's bounds,
        // which leaves no gaps when tilted.
        let ((ax, ay), (bx, by)) = cut.axes();
        let (ax, ay, bx, by) = (ax * zoom, ay * zoom, bx * zoom, by * zoom);
        let (cw, ch) = (ax.abs() + bx.abs(), ay.abs() + by.abs());
        // Unturned in its plane, a row's cells share a line, so a run of one
        // fill is one div; turned, each cell is placed on its own.
        let merge = cut.rz == 0.;
        // The sheet's thin dark edge, right of each run of cells (left once
        // ry has mirrored the sheet).
        let edge = snap(ax.abs() * 0.4).max(0.5);
        let half = SIZE as f32 / 2.;
        let centre = |x: usize, y: usize| {
            let (u, v) = (x as f32 + 0.5 - half, y as f32 + 0.5 - half);
            (
                width / 2. + cut.x * zoom + ax * u + bx * v,
                height / 2. + cut.y * zoom + ay * u + by * v,
            )
        };
        for (y, line) in sprite.cells.iter().enumerate() {
            let mut start = 0;
            for (x, c) in line.iter().enumerate() {
                if x == 0 || line[x - 1] != *c {
                    start = x;
                }
                let fill = match c {
                    Cell::Empty => continue,
                    Cell::Body => body,
                    Cell::Eye => ink,
                };
                // Drawn at its run's last cell.
                if merge && line.get(x + 1) == Some(c) {
                    continue;
                }
                let (from, _) = centre(if merge { start } else { x }, y);
                let (cx, cy) = centre(x, y);
                let (top, bottom) = (snap(cy - ch / 2.), snap(cy + ch / 2.));
                let at = |l: f32, r: f32| {
                    div()
                        .absolute()
                        .left(px(l))
                        .top(px(top))
                        .w(px(r - l))
                        .h(px(bottom - top))
                };
                // A mirrored sheet runs right to left.
                let (l, r) = (from.min(cx) - cw / 2., from.max(cx) + cw / 2.);
                cells.push(at(snap(l), snap(r)).bg(fill));
                if line.get(x + 1).is_none_or(|n| *n == Cell::Empty) {
                    let side = snap(cx + ax / 2.);
                    let (l, r) = if ax < 0. {
                        (side - edge, side)
                    } else {
                        (side, side + edge)
                    };
                    cells.push(at(l, r).bg(gpui::black().opacity(0.5)));
                }
            }
        }
    }
    div()
        .flex_none()
        .relative()
        .w(px(width))
        .h(px(height))
        .children(cells)
}

/// A circle's creature, honoring its `seance-lead-sprite.<slug>` override.
pub(super) fn for_circle(slug: &str) -> Sprite {
    let conf = crate::term_config::get();
    generate(conf.lead_sprites.get(slug).map_or(slug, String::as_str))
}

/// How many creatures dance at the party, under how many pieces of
/// confetti.
const PARTY_CREW: usize = 8;
pub(super) const CONFETTI: usize = 140;
/// The ring's radius before it is scaled up, at most.
const PARTY_R: f32 = 60.;
/// Spinning in, and out again at the end.
const PARTY_SPIN_MS: u64 = PARTY_MS * 12 / 100;
/// The scene fades in this long, and out over the last [`PARTY_FADE_OUT_MS`].
const PARTY_FADE_IN_MS: u64 = PARTY_MS * 8 / 100;
const PARTY_FADE_OUT_MS: u64 = PARTY_MS / 10;
/// Lamp gold and genie purple, between the creatures' own colors.
const LAMP: u32 = 0xe9a03a;
pub(super) const GENIE: u32 = 0x7c5cff;

/// The five-click party over Chat, the web Chat's `eggParty`: the circles'
/// creatures spin in big, run through the crew's moves with a new one every
/// [`PARTY_MOVE_SECS`] under falling confetti, then spin away. A click ends
/// it.
pub(super) struct Party {
    /// When it began (unix ms).
    pub at: u64,
    sprites: Vec<Sprite>,
    shades: Vec<f32>,
    dance: CrewDance,
}

impl Party {
    /// `home` (this circle's creature) and `others` (the other circles').
    pub(super) fn new(
        home: Sprite,
        others: impl IntoIterator<Item = Sprite>,
        now: u64,
        rng: &mut impl rand::Rng,
    ) -> Self {
        let (sprites, shades) = party_slots(home, others, rng).into_iter().unzip();
        Self {
            at: now,
            sprites,
            shades,
            dance: CrewDance::fast(now),
        }
    }

    pub(super) fn on(&self, now: u64) -> bool {
        now.saturating_sub(self.at) < PARTY_MS
    }

    pub(super) fn advance(&mut self, now: u64, rng: &mut impl rand::Rng) {
        self.dance.advance(now, rng);
    }

    /// Confetti piece `i`'s color: lamp gold, genie purple or a creature's.
    pub(super) fn confetti_color(&self, i: usize) -> gpui::Hsla {
        match i % 4 {
            0 => rgb(LAMP).into(),
            1 => rgb(GENIE).into(),
            _ => body_color(&self.sprites[i % self.sprites.len()]),
        }
    }

    /// The crew at `now` on a ring of radius `r`, spun and scaled as the
    /// scene is.
    fn cuts(&self, now: u64, r: f32) -> Vec<Cutout> {
        let Some(scene) = party_scene(now.saturating_sub(self.at)) else {
            return Vec::new();
        };
        let crew: Vec<(u32, f32)> = (0..).zip(self.shades.iter().copied()).collect();
        let mut cuts = place_cuts(&crew, &self.dance, now, r, PARTY_PERSPECTIVE);
        // gpui cannot turn the ring, so each cutout goes round its centre
        // and turns in its plane by as much; close enough for a spin.
        let (sin, cos) = scene.turn.sin_cos();
        for c in &mut cuts {
            let (x, y) = (c.x, c.y);
            c.x = scene.grow * (x * cos - y * sin);
            c.y = scene.grow * (x * sin + y * cos);
            c.scale *= scene.grow;
            c.rz += scene.turn;
        }
        cuts
    }

    /// The crew, sized for a view `view_w` points wide as the web sizes it.
    pub(super) fn render(&self, now: u64, view_w: f32) -> AnyElement {
        let zoom = (view_w / 150.).min(2.6);
        let r = PARTY_R.min(view_w * 0.32 / zoom);
        let side = SIZE as f32 * CREW_CELL;
        crew_box(
            &self.sprites,
            &self.cuts(now, r),
            (2. * r + side) * zoom,
            side * zoom,
            zoom,
        )
        .into_any_element()
    }
}

/// The party's crew, slot by slot: `home` first, then the other circles'
/// distinct creatures shuffled, up to [`PARTY_CREW`], then `home` again
/// until it is full. Home wears [`CREW_SHADES`] by slot, the others their own
/// color.
fn party_slots(
    home: Sprite,
    others: impl IntoIterator<Item = Sprite>,
    rng: &mut impl rand::Rng,
) -> Vec<(Sprite, f32)> {
    use rand::seq::SliceRandom;
    let mut rest: Vec<Sprite> = Vec::new();
    for s in others {
        if s != home && !rest.contains(&s) {
            rest.push(s);
        }
    }
    rest.shuffle(rng);
    rest.truncate(PARTY_CREW - 1);
    let fill = PARTY_CREW - 1 - rest.len();
    std::iter::once(home.clone())
        .chain(rest)
        .chain(std::iter::repeat_n(home.clone(), fill))
        .enumerate()
        .map(|(i, s)| {
            let shade = if s == home {
                CREW_SHADES[i % CREW_SHADES.len()]
            } else {
                0.
            };
            (s, shade)
        })
        .collect()
}

/// The scene `t` ms into the party: how opaque it is, and how grown (0 to 1)
/// and turned (radians) the crew's ring is.
#[derive(Debug, PartialEq)]
pub(super) struct Scene {
    pub fade: f32,
    pub grow: f32,
    pub turn: f32,
}

/// None once the party is over.
pub(super) fn party_scene(t: u64) -> Option<Scene> {
    if t >= PARTY_MS {
        return None;
    }
    let fade = if t < PARTY_FADE_IN_MS {
        t as f32 / PARTY_FADE_IN_MS as f32
    } else {
        ((PARTY_MS - t) as f32 / PARTY_FADE_OUT_MS as f32).min(1.)
    };
    let ease = |b: f32| b * b * (3. - 2. * b);
    let spin = 200f32.to_radians();
    let (grow, turn) = if t < PARTY_SPIN_MS {
        let e = ease(t as f32 / PARTY_SPIN_MS as f32);
        (e, -spin * (1. - e))
    } else if t >= PARTY_MS - PARTY_SPIN_MS {
        let e = ease((t - (PARTY_MS - PARTY_SPIN_MS)) as f32 / PARTY_SPIN_MS as f32);
        (1. - e, spin * e)
    } else {
        (1., 0.)
    };
    Some(Scene { fade, grow, turn })
}

/// Confetti piece `i` of [`CONFETTI`], `t` ms into the party `seed` began,
/// over a view `w` by `h` points: its top left, or None before it starts
/// falling, after it has fallen past the bottom, and once the party is over.
/// Each starts in its own 0 to 4.2 s and falls for 1.6 to 3.2 s, drifting up
/// to 60 points either way, as the web's `partyFall`.
pub(super) fn confetti_at(seed: u64, i: usize, t: u64, w: f32, h: f32) -> Option<(f32, f32)> {
    if t >= PARTY_MS {
        return None;
    }
    let mut rng = Rng((seed ^ (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)) | 1);
    let left = rng.below(1001) as f32 / 1000. * w;
    let drift = rng.below(121) as f32 - 60.;
    let fall = 1600 + rng.below(1601) as u64;
    let delay = rng.below(4201) as u64;
    let age = t.checked_sub(delay).filter(|a| *a < fall)?;
    let p = age as f32 / fall as f32;
    Some((left + drift * p, -12. + p * 1.1 * h))
}

/// A click on the party. On the header's creature it is one more poke and
/// ignored, as those are while a party runs; anywhere else it ends the
/// party. True if that party was still on, so ended early.
pub(super) fn click_party(party: &mut Option<Party>, now: u64, on_creature: bool) -> bool {
    if on_creature && party.as_ref().is_some_and(|p| p.on(now)) {
        return false;
    }
    party.take().is_some_and(|p| p.on(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dance_mixes_in_moves_and_circles_differ() {
        // Start on a bar line, so chunks of four are whole bars.
        let steps = |phase: u64| {
            let start = (4 - phase % 4) % 4;
            (start..start + 400)
                .map(|b| dance_step(phase, b))
                .collect::<Vec<_>>()
        };
        let a = steps(generate("alpha-circle").phase);
        let b = steps(generate("seance-demo").phase);
        assert_ne!(a, b);
        // Mostly the basic step, with some other moves thrown in.
        let basic = a.chunks(4).filter(|bar| bar == &MOVES[0]).count();
        assert!(basic > 40 && basic < 100, "basic bars: {basic}");
        // Never further than two cells from home.
        assert!(a.iter().all(|(x, y)| x.abs() <= 2. && y.abs() <= 2.));
    }

    #[test]
    fn same_slug_same_creature() {
        assert_eq!(generate("alpha-circle"), generate("alpha-circle"));
    }

    #[test]
    fn different_slugs_differ() {
        let slugs = [
            "main",
            "alpha-circle",
            "circle-2",
            "profile",
            "3d-maps",
            "personal",
        ];
        let sprites: Vec<_> = slugs.iter().map(|s| generate(s)).collect();
        for (i, a) in sprites.iter().enumerate() {
            for b in &sprites[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn symmetric_with_two_eyes_inside_the_body() {
        for slug in [
            "main",
            "alpha-circle",
            "circle-2",
            "a",
            "zz-top",
            "x".repeat(40).as_str(),
        ] {
            let s = generate(slug);
            let mut eyes = 0;
            for (r, row) in s.cells.iter().enumerate() {
                for c in 0..SIZE {
                    assert_eq!(row[c], row[SIZE - 1 - c], "{slug} row {r}");
                    if row[c] == Cell::Eye {
                        eyes += 1;
                        assert_eq!(row[c - 1], Cell::Body, "{slug}: eye has body beside it");
                    }
                }
            }
            assert_eq!(eyes, 2, "{slug}");
            assert!(s.cells[9].contains(&Cell::Body), "{slug}: has legs");
        }
    }

    #[test]
    fn five_pokes_in_three_seconds_throw_a_party() {
        let mut p = Pokes::default();
        for i in 0..4 {
            assert!(matches!(p.poke(1_000 + i * 500), Poke::Dance { mv, .. } if mv < MOVES.len()));
        }
        assert_eq!(p.poke(3_000), Poke::Party);
        // The count starts over after a party.
        assert!(matches!(p.poke(3_100), Poke::Dance { .. }));
    }

    #[test]
    fn slow_pokes_never_party() {
        let mut p = Pokes::default();
        for i in 0..20 {
            assert!(matches!(p.poke(i * 1_000), Poke::Dance { .. }));
        }
    }

    #[test]
    fn every_line_comes_up_and_the_party_runs_seven_seconds() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..400 {
            if let Poke::Dance { line, .. } = Pokes::default().poke(i * 7_919) {
                seen.insert(line);
            }
        }
        assert_eq!(seen.len(), LINES.len());
        start_party(10_000);
        assert!(partying(10_000 + PARTY_MS - 1));
        assert!(!partying(10_000 + PARTY_MS));
        start_party(10_000);
        stop_party();
        assert!(!partying(10_001));
    }

    #[test]
    fn crew_needs_two_and_stops_at_six() {
        let d = CrewDance::new(0);
        assert!(crew_at(&[], &d, 0).is_empty());
        assert!(crew_at(&[4], &d, 0).is_empty());
        assert_eq!(crew_at(&[0, 1], &d, 0).len(), 2);
        let big: Vec<u32> = (0..9).collect();
        let mut panes: Vec<u32> = crew_at(&big, &d, 0).iter().map(|c| c.pane).collect();
        panes.sort();
        assert_eq!(panes, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn subagents_past_the_panes_keep_a_wrapping_shade() {
        // Subagents are numbered after every pane, so indexes run past six.
        let d = CrewDance::new(0);
        let cuts = crew_at(&[2, 9, 14], &d, 0);
        let mut panes: Vec<u32> = cuts.iter().map(|c| c.pane).collect();
        panes.sort();
        assert_eq!(panes, [2, 9, 14]);
        // 2 and 14 share a shade, as on the web.
        for c in &cuts {
            assert_eq!(c.shade, CREW_SHADES[c.pane as usize % 6]);
        }
        // The six taken are the first six listed, not the six lowest.
        let mut panes: Vec<u32> = crew_at(&[25, 3, 7, 8, 12, 20, 0], &d, 0)
            .iter()
            .map(|c| c.pane)
            .collect();
        panes.sort();
        assert_eq!(panes, [3, 7, 8, 12, 20, 25]);
    }

    #[test]
    fn each_circle_keeps_its_own_dance_while_its_crew_lasts() {
        let mut dances = std::collections::HashMap::new();
        keep_crew_dances(
            &mut dances,
            [("a", &[0, 1][..]), ("b", &[3][..]), ("c", &[][..])],
            100,
        );
        assert_eq!(dances.keys().collect::<Vec<_>>(), ["a"]);
        // A later round keeps a's dance as it was and starts c's.
        keep_crew_dances(&mut dances, [("a", &[0, 7][..]), ("c", &[1, 2][..])], 900);
        assert_eq!(dances["a"], CrewDance::new(100));
        assert_eq!(dances["c"], CrewDance::new(900));
        // Under two, or gone from the list: dropped.
        keep_crew_dances(&mut dances, [("a", &[0][..])], 1000);
        assert!(dances.is_empty());
    }

    #[test]
    fn crew_draws_back_to_front_as_it_turns() {
        // Before its first change of move the crew circles.
        let d = CrewDance::new(0);
        for t in (0..4000).step_by(137) {
            let cuts = crew_at(&[0, 1, 2, 3, 4], &d, t);
            assert!(cuts.windows(2).all(|w| w[0].z <= w[1].z), "t={t}");
            // Nearer is bigger and lower, as seen from above the ring.
            let (back, front) = (&cuts[0], &cuts[4]);
            assert!(front.scale > back.scale && front.y > back.y);
        }
        // Each one comes round to the front in turn.
        let front = |t| crew_at(&[0, 1, 2], &d, t).last().unwrap().pane;
        let fronts: std::collections::HashSet<u32> = (0..4000).step_by(100).map(front).collect();
        assert_eq!(fronts.len(), 3);
    }

    #[test]
    fn crew_never_dances_a_move_twice_running() {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let mut d = CrewDance::new(0);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2000 {
            let was = d.to;
            d.advance((d.next * 1000.).ceil() as u64 + 1, &mut rng);
            assert_ne!(d.to, was);
            assert_eq!(d.from, was);
            // Each move lasts 3.5 to 6.5 s.
            assert!(
                MOVE_SECS.contains(&(d.next - d.began)),
                "{}",
                d.next - d.began
            );
            seen.insert(d.to);
        }
        assert_eq!(seen.len(), CREW_MOVES.len());
    }

    #[test]
    fn a_new_move_blends_in_then_the_old_is_dropped() {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let mut d = CrewDance::new(0);
        d.advance(4000, &mut rng);
        assert_eq!((d.from, d.began), (CrewMove::Circle, 4.));
        let (from, to) = (d.from, d.to);
        // The first frame is all the old move, the last all the new.
        assert_eq!(d.blend(4.), 0.);
        assert_eq!(d.place(1, 3, 4000, 15.), crew_place(from, 1, 3, 4., 15.));
        assert!(d.blend(4.45) > 0. && d.blend(4.45) < 1.);
        assert_eq!(d.blend(4.9), 1.);
        assert_eq!(d.place(1, 3, 4900, 15.), crew_place(to, 1, 3, 4.9, 15.));
        d.advance(4900, &mut rng);
        assert_eq!((d.from, d.to), (to, to));
    }

    #[test]
    fn leapfrog_walks_then_jumps_over() {
        let at = |t| crew_place(CrewMove::Leapfrog, 0, 3, t, 15.);
        // Most of the way it walks left to right on the ground.
        let (start, walk) = (at(0.), at(0.35 / 0.55));
        assert_eq!((start.x, start.y, start.rz), (-15., 6., 0.));
        assert!(walk.x > start.x && walk.y == 6. && walk.rz == 0.);
        // Then it leaps back over the others, turning a full circle.
        let top = at(0.85 / 0.55);
        assert!(
            top.x.abs() < 0.01 && top.y < -17.9 && top.z < -9.9,
            "{top:?}"
        );
        assert!((top.rz + 180.).abs() < 0.1, "{top:?}");
    }

    #[test]
    fn totem_and_dosado_place_an_odd_crew() {
        for n in [3, 5] {
            for t in [0., 1.3, 7.7] {
                let all = |mv| (0..n).map(move |i| crew_place(mv, i, n, t, crew_radius(n)));
                assert!(all(CrewMove::Totem)
                    .chain(all(CrewMove::Dosado))
                    .all(|p| [p.x, p.y, p.z].iter().all(|v| v.is_finite())));
                // The totem stacks them bottom to top, nearest on top.
                let totem: Vec<_> = all(CrewMove::Totem).collect();
                assert!(totem.windows(2).all(|w| w[0].y < w[1].y && w[0].z < w[1].z));
            }
            // The odd one out of the do-si-do dances on the right end.
            let last = crew_place(CrewMove::Dosado, n - 1, n, 0., crew_radius(n));
            assert_eq!(last.x, crew_radius(n) * 0.7 + 11.);
        }
        let mut d = CrewDance::new(0);
        d.to = CrewMove::Dosado;
        d.from = CrewMove::Dosado;
        assert_eq!(crew_at(&[0, 1, 2, 3, 4], &d, 2000).len(), 5);
    }

    #[test]
    fn a_turned_sheet_squeezes_and_mirrors_past_a_quarter() {
        let d = CrewDance::new(0);
        let mut cut = crew_at(&[0, 1], &d, 0).remove(0);
        cut.sway = 0.;
        cut.scale = 1.;
        let ((ax, ay), (bx, by)) = cut.axes();
        assert_eq!((ax, ay, bx, by), (CREW_CELL, 0., 0., CREW_CELL));
        cut.ry = 120f32.to_radians();
        let ((ax, _), _) = cut.axes();
        assert!((ax + CREW_CELL / 2.).abs() < 1e-5, "{ax}");
        cut.ry = 0.;
        cut.rz = 90f32.to_radians();
        let ((ax, ay), (bx, by)) = cut.axes();
        assert!(ax.abs() < 1e-5 && (ay - CREW_CELL).abs() < 1e-5);
        assert!((bx + CREW_CELL).abs() < 1e-5 && by.abs() < 1e-5);
    }

    #[test]
    fn a_pane_keeps_its_shade_wherever_it_is_in_the_crew() {
        let shade = |crew: &[u32], pane| {
            crew_at(crew, &CrewDance::new(0), 1234)
                .into_iter()
                .find(|c| c.pane == pane)
                .unwrap()
                .shade
        };
        assert_eq!(shade(&[3, 1], 3), shade(&[1, 3], 3));
        assert_eq!(shade(&[0, 2, 3], 3), shade(&[3, 7], 3));
        assert_ne!(shade(&[1, 3], 1), shade(&[1, 3], 3));
        // Pane 0 wears the circle's own color.
        assert_eq!(shade(&[0, 1], 0), 0.);
        assert_eq!(shade_rgb(0x8fa67a, 0.), 0x8fa67a);
        assert_eq!(shade_rgb(0x808080, 1.), 0xffffff);
        assert_eq!(shade_rgb(0x808080, -1.), 0x000000);
    }

    #[test]
    fn the_party_spins_in_holds_then_spins_out() {
        let at = |t| party_scene(t).unwrap();
        // From nothing, turned back, and faded out.
        assert_eq!((at(0).grow, at(0).fade), (0., 0.));
        assert!((at(0).turn + 200f32.to_radians()).abs() < 1e-5);
        let mid = at(PARTY_SPIN_MS / 2);
        assert!(mid.grow > 0. && mid.grow < 1. && mid.turn < 0.);
        // Steady and fully shown between the spins.
        for t in (PARTY_SPIN_MS..PARTY_MS - PARTY_SPIN_MS).step_by(250) {
            assert_eq!((at(t).grow, at(t).turn), (1., 0.), "t={t}");
        }
        assert_eq!(at(PARTY_MS / 2).fade, 1.);
        // Spinning away the other way, and gone at seven seconds.
        let out = at(PARTY_MS - PARTY_SPIN_MS / 2);
        assert!(out.grow > 0. && out.grow < 1. && out.turn > 0.);
        assert!(at(PARTY_MS - 1).grow < 0.01 && at(PARTY_MS - 1).fade < 0.01);
        assert_eq!(PARTY_MS, 7000);
        assert!(party_scene(PARTY_MS).is_none() && party_scene(PARTY_MS + 5000).is_none());
    }

    #[test]
    fn the_party_crew_changes_move_every_second_and_a_half() {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(3);
        let mut d = CrewDance::fast(1_000);
        let mut changes = Vec::new();
        for now in (1_000..1_000 + PARTY_MS).step_by(16) {
            let was = d.to;
            d.advance(now, &mut rng);
            if d.to != was {
                changes.push(now - 1_000);
            }
        }
        // At 1.5, 3, 4.5 and 6 s, to the nearest frame.
        assert_eq!(changes.len(), 4, "{changes:?}");
        for (k, at) in changes.iter().enumerate() {
            let due = 1_500 * (k as u64 + 1);
            assert!(*at >= due && *at < due + 16 * (k as u64 + 1), "{changes:?}");
        }
        // Never the same move twice running.
        let mut d = CrewDance::fast(0);
        for _ in 0..500 {
            let was = d.to;
            d.advance((d.next * 1000.).ceil() as u64 + 1, &mut rng);
            assert_ne!(d.to, was);
            assert!((d.next - d.began - PARTY_MOVE_SECS).abs() < 1e-3);
        }
    }

    #[test]
    fn confetti_falls_in_view_for_the_party_only() {
        let (w, h) = (900., 600.);
        for i in 0..CONFETTI {
            assert_eq!(
                confetti_at(42, i, 2_000, w, h),
                confetti_at(42, i, 2_000, w, h)
            );
            // Every piece falls at some point, and none after the party.
            let seen: Vec<_> = (0..PARTY_MS)
                .step_by(10)
                .filter_map(|t| confetti_at(42, i, t, w, h))
                .collect();
            assert!(!seen.is_empty(), "piece {i}");
            for (x, y) in seen {
                assert!((-60. ..=w + 60.).contains(&x), "{x}");
                assert!((-12. ..=1.1 * h - 12.).contains(&y), "{y}");
            }
            assert!(confetti_at(42, i, PARTY_MS, w, h).is_none());
        }
    }

    #[test]
    fn the_party_crew_is_eight_with_home_first_and_shaded() {
        use rand::SeedableRng;
        let home = generate("home");
        let others = ["a", "b", "c"].map(generate);
        let mut rng = rand::rngs::StdRng::seed_from_u64(9);
        // Repeats and home itself among the others are dropped, then home
        // fills the rest, shaded by slot.
        let mut given = others.to_vec();
        given.extend([others[0].clone(), home.clone()]);
        let slots = party_slots(home.clone(), given, &mut rng);
        assert_eq!(slots.len(), PARTY_CREW);
        assert_eq!(slots[0], (home.clone(), 0.));
        for o in &others {
            assert_eq!(slots.iter().filter(|(s, _)| s == o).count(), 1);
        }
        for (i, (s, shade)) in slots.iter().enumerate() {
            let want = if *s == home {
                CREW_SHADES[i % CREW_SHADES.len()]
            } else {
                0.
            };
            assert_eq!(*shade, want, "slot {i}");
        }
        assert!(slots[4..].iter().all(|(s, _)| *s == home));
        // More circles than slots: no repeats, and the rng picks the order.
        let many: Vec<Sprite> = (0..12).map(|i| generate(&format!("c{i}"))).collect();
        let pick = |seed| {
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            party_slots(home.clone(), many.clone(), &mut rng)
        };
        let a = pick(1);
        assert_eq!(a.len(), PARTY_CREW);
        assert_eq!(a.iter().filter(|(s, _)| *s == home).count(), 1);
        assert_eq!(a, pick(1));
        assert!((2..20).any(|seed| pick(seed) != a));
    }

    #[test]
    fn a_click_ends_the_party_early() {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(0);
        let start = |rng: &mut rand::rngs::StdRng| {
            Some(Party::new(generate("home"), [generate("a")], 10_000, rng))
        };
        let mut party = start(&mut rng);
        // A click on the header's creature is an ignored poke.
        assert!(!click_party(&mut party, 11_000, true));
        assert!(party.is_some());
        assert!(click_party(&mut party, 12_000, false));
        assert!(party.is_none());
        // Over already, or none: nothing ended early.
        let mut party = start(&mut rng);
        assert!(!click_party(&mut party, 10_000 + PARTY_MS, false));
        assert!(party.is_none());
        assert!(!click_party(&mut None, 12_000, false));
        // The crew is gone once the party is over.
        let party = start(&mut rng).unwrap();
        assert_eq!(party.cuts(12_000, PARTY_R).len(), PARTY_CREW);
        assert!(party.cuts(10_000 + PARTY_MS, PARTY_R).is_empty());
    }
}

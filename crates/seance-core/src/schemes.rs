//! Color schemes: the whole look of seance (chrome and terminal content) in
//! one switchable table.
//!
//! Pure data, so every client can read it: the native GUI turns `chrome` into
//! its `SeancePalette`, the daemon resolves terminal cells from `term`, and
//! the web client can mirror both later. The daemon owns which scheme is
//! active and tells every attached window (`GuiEvent::Theme`).
//!
//! Colors are packed `0xRRGGBB`.

/// The scheme a fresh install starts on (today's look).
pub const DEFAULT: &str = "candlelit";

/// Chrome tokens, named as `SeancePalette` names them: `flame` is the
/// primary accent and `violet` the second, whatever hue a scheme gives them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    pub bg: u32,
    pub bg_elevated: u32,
    pub surface: u32,
    pub border: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_faint: u32,
    pub flame: u32,
    pub flame_dim: u32,
    pub violet: u32,
    pub violet_dim: u32,
    pub success: u32,
    pub danger: u32,
}

/// Terminal content colors: the 16 ANSI slots plus default fg/bg and cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermColors {
    pub ansi: [u32; 16],
    pub fg: u32,
    pub bg: u32,
    pub cursor: u32,
}

#[derive(Debug)]
pub struct Scheme {
    pub id: &'static str,
    pub label: &'static str,
    /// Light ground: widgets switch to their light mode underneath.
    pub light: bool,
    pub chrome: Chrome,
    /// `None` keeps the user's own terminal colors (`terminal.conf`).
    pub term: Option<TermColors>,
}

/// Terminal source colors in the order the scheme specs list them.
struct TermSpec {
    bg: u32,
    fg: u32,
    green: u32,
    yellow: u32,
    blue: u32,
    magenta: u32,
    red: u32,
    cyan: u32,
    gray: u32,
}

/// Fill the 16 slots from the scheme's named colors. Brights repeat the base
/// hues; black and white come from the chrome so the ends of the ramp sit on
/// the scheme's own grounds. Light schemes flip which end is which.
const fn term(light: bool, c: &Chrome, t: TermSpec) -> TermColors {
    let (black, white, bright_white) = if light {
        (c.text, c.surface, c.bg_elevated)
    } else {
        (c.bg_elevated, c.text_dim, c.text)
    };
    TermColors {
        ansi: [
            black,
            t.red,
            t.green,
            t.yellow,
            t.blue,
            t.magenta,
            t.cyan,
            white,
            t.gray,
            t.red,
            t.green,
            t.yellow,
            t.blue,
            t.magenta,
            t.cyan,
            bright_white,
        ],
        fg: t.fg,
        bg: t.bg,
        cursor: c.flame,
    }
}

const fn chrome(c: [u32; 13]) -> Chrome {
    Chrome {
        bg: c[0],
        bg_elevated: c[1],
        surface: c[2],
        border: c[3],
        text: c[4],
        text_dim: c[5],
        text_faint: c[6],
        flame: c[7],
        flame_dim: c[8],
        violet: c[9],
        violet_dim: c[10],
        success: c[11],
        danger: c[12],
    }
}

const fn spec(t: [u32; 9]) -> TermSpec {
    TermSpec {
        bg: t[0],
        fg: t[1],
        green: t[2],
        yellow: t[3],
        blue: t[4],
        magenta: t[5],
        red: t[6],
        cyan: t[7],
        gray: t[8],
    }
}

// Hex of the HSL in the native `theme.rs`; the native app paints candlelit
// from the HSL itself.
const CANDLELIT: Chrome = chrome([
    0x131111, 0x1C1718, 0x211C1D, 0x352C2E, 0xEBE3DB, 0xA69A91, 0x69605D, 0xE9A03A, 0xA97328,
    0xA790D5, 0x6C5B95, 0x86B67C, 0xD0675D,
]);
const ABYSSAL: Chrome = chrome([
    0x0A1018, 0x0F1722, 0x16233A, 0x22354D, 0xD8E6F0, 0x93AABF, 0x6E86A0, 0x3FE0D0, 0x1F8F86,
    0xFF8A6B, 0xA65544, 0x7BD88F, 0xFF5F6D,
]);
const ECTOPLASM: Chrome = chrome([
    0x0B0F0C, 0x101611, 0x17221A, 0x24332A, 0xD6F5DD, 0x98BFA3, 0x6A8873, 0x7CFF9E, 0x3E9E5A,
    0xD6B8FF, 0x7F6A9E, 0xC6F07A, 0xFF6B6B,
]);
const OUIJA: Chrome = chrome([
    0xF3EBDD, 0xEADFCC, 0xE0D0B5, 0xCDBB9C, 0x2A1E16, 0x5E4A3A, 0x735E4B, 0x8E2A1F, 0xB5574A,
    0x2F3F7A, 0x6B78A8, 0x3D6B2E, 0xC2410C,
]);
const MONOKAI: Chrome = chrome([
    0x19181A, 0x221F22, 0x2D2A2E, 0x403E41, 0xFCFCFA, 0xC1C0C0, 0x8C8A8C, 0xFFD866, 0xB39847,
    0xAB9DF2, 0x6E6599, 0xA9DC76, 0xFF6188,
]);
const TAROT: Chrome = chrome([
    0x120D1C, 0x1A1328, 0x251B3A, 0x362850, 0xEDE6F7, 0xB3A6CC, 0x8A7CA6, 0xE4B94C, 0x9E7F2E,
    0xE0457B, 0x94304F, 0x8FD19E, 0xFF5C5C,
]);
const MOJAVE: Chrome = chrome([
    0x1E1416, 0x281A1C, 0x352225, 0x4A3033, 0xF6E7DC, 0xCDAE9E, 0x9E8073, 0xFF8C42, 0xB05E2A,
    0x3FC1B0, 0x2A7F75, 0xB5D17A, 0xFF5A5F,
]);
const RISO: Chrome = chrome([
    0xF7F4EC, 0xEFEBDF, 0xE4DFCF, 0xCFC8B3, 0x1A1A1A, 0x4A4A48, 0x6B6A64, 0xE6007E, 0xF075B5,
    0x00838A, 0x5FB3B8, 0x2E7D32, 0xD32F2F,
]);
const SYNTH: Chrome = chrome([
    0x0D0B1F, 0x141130, 0x1D1844, 0x2D2660, 0xF2EEFF, 0xB9B0E6, 0x8A80C2, 0xFF3DCB, 0xA62A86,
    0x2DE2FF, 0x1C8EA1, 0x5CFFB0, 0xFF7A3D,
]);
// Fog, Parlor and Herbarium sit between the dark and light ends: a lifted
// slate dark, and two dimmed (not white) light grounds. Fog's terminal sits
// a step darker than its chrome so dim and gray text stay readable; the light
// two keep danger crimson so it doesn't read as their brick-red flame.
const FOG: Chrome = chrome([
    0x3B414D, 0x434A57, 0x4C5462, 0x545B69, 0xECEFF4, 0xC5CBD6, 0x9AA3B2, 0xF2B56B, 0xB0824A,
    0xB7A8F0, 0x7D72AE, 0xA6D38E, 0xFF8A93,
]);
const PARLOR: Chrome = chrome([
    0xCDC3B2, 0xC3B8A6, 0xB8AC98, 0xAEA28D, 0x231C17, 0x4A3E33, 0x5E5145, 0x8A2419, 0xA8564A,
    0x24527A, 0x5E7FA0, 0x2F5E22, 0x9E1449,
]);
const HERBARIUM: Chrome = chrome([
    0xBDC5B3, 0xB2BBA7, 0xA7B09B, 0x9DA690, 0x1C251E, 0x3A463C, 0x4D5A4F, 0x8E3B14, 0xAE6A48,
    0x43407F, 0x7471A6, 0x2E5A1E, 0x9A1040,
]);

/// Every scheme, in menu order.
pub static SCHEMES: [Scheme; 12] = [
    Scheme {
        id: "candlelit",
        label: "Candlelit",
        light: false,
        chrome: CANDLELIT,
        term: None,
    },
    Scheme {
        id: "abyssal",
        label: "Abyssal",
        light: false,
        chrome: ABYSSAL,
        term: Some(term(
            false,
            &ABYSSAL,
            spec([
                0x0C131D, 0xC9D7E3, 0x7BD88F, 0xF2C572, 0x6AB0FF, 0xC792EA, 0xFF5F6D, 0x3FE0D0,
                0x6E86A0,
            ]),
        )),
    },
    Scheme {
        id: "ectoplasm",
        label: "Ectoplasm",
        light: false,
        chrome: ECTOPLASM,
        term: Some(term(
            false,
            &ECTOPLASM,
            spec([
                0x070A08, 0xB8F2C6, 0x7CFF9E, 0xE6F27A, 0x7FD4FF, 0xD6B8FF, 0xFF6B6B, 0x6FF2E0,
                0x6A8873,
            ]),
        )),
    },
    Scheme {
        id: "ouija",
        label: "Ouija",
        light: true,
        chrome: OUIJA,
        term: Some(term(
            true,
            &OUIJA,
            spec([
                0xF8F2E7, 0x2A1E16, 0x3D6B2E, 0x8A6100, 0x2F3F7A, 0x7A2E6E, 0xA3241A, 0x1F6B6B,
                0x735E4B,
            ]),
        )),
    },
    Scheme {
        id: "monokai",
        label: "Monokai Seance",
        light: false,
        chrome: MONOKAI,
        term: Some(term(
            false,
            &MONOKAI,
            spec([
                0x2D2A2E, 0xFCFCFA, 0xA9DC76, 0xFFD866, 0x78DCE8, 0xAB9DF2, 0xFF6188, 0x78DCE8,
                0x8C8A8C,
            ]),
        )),
    },
    Scheme {
        id: "tarot",
        label: "Tarot",
        light: false,
        chrome: TAROT,
        term: Some(term(
            false,
            &TAROT,
            spec([
                0x150F21, 0xE3DAF2, 0x8FD19E, 0xE4B94C, 0x8AA8FF, 0xC68BFF, 0xFF5C7A, 0x6FD6E0,
                0x8A7CA6,
            ]),
        )),
    },
    Scheme {
        id: "mojave",
        label: "Mojave Dusk",
        light: false,
        chrome: MOJAVE,
        term: Some(term(
            false,
            &MOJAVE,
            spec([
                0x22171A, 0xF2DFD3, 0xB5D17A, 0xFFC66B, 0x7FB8E6, 0xE38FB8, 0xFF5A5F, 0x3FC1B0,
                0x9E8073,
            ]),
        )),
    },
    Scheme {
        id: "riso",
        label: "Riso",
        light: true,
        chrome: RISO,
        term: Some(term(
            true,
            &RISO,
            spec([
                0xFBF9F3, 0x1A1A1A, 0x2E7D32, 0x8A6A00, 0x2B4FBF, 0xC2006A, 0xC62828, 0x00838A,
                0x6B6A64,
            ]),
        )),
    },
    Scheme {
        id: "synth",
        label: "Synth Seance",
        light: false,
        chrome: SYNTH,
        term: Some(term(
            false,
            &SYNTH,
            spec([
                0x0F0C26, 0xE6E0FF, 0x5CFFB0, 0xFFE15C, 0x6E8BFF, 0xFF3DCB, 0xFF4D6D, 0x2DE2FF,
                0x8A80C2,
            ]),
        )),
    },
    Scheme {
        id: "fog",
        label: "Fog",
        light: false,
        chrome: FOG,
        term: Some(term(
            false,
            &FOG,
            spec([
                0x30353F, 0xE3E7EE, 0xA6D38E, 0xF2D07A, 0x8CC4FF, 0xD3A8F0, 0xFF8A93, 0x7FDCD3,
                0x9AA3B2,
            ]),
        )),
    },
    Scheme {
        id: "parlor",
        label: "Parlor",
        light: true,
        chrome: PARLOR,
        term: Some(term(
            true,
            &PARLOR,
            spec([
                0xD2C9B9, 0x231C17, 0x2F5E22, 0x6E4E00, 0x24427A, 0x6E2462, 0x8A2419, 0x165858,
                0x5E5145,
            ]),
        )),
    },
    Scheme {
        id: "herbarium",
        label: "Herbarium",
        light: true,
        chrome: HERBARIUM,
        term: Some(term(
            true,
            &HERBARIUM,
            spec([
                0xC2CAB8, 0x1C251E, 0x2E5A1E, 0x6A5000, 0x2D4A85, 0x6E2E70, 0x8E2418, 0x1A5A5A,
                0x4D5A4F,
            ]),
        )),
    },
];

/// Look a scheme up by id. Ids are lowercase; input is trimmed and
/// lowercased so `Ouija` from a config file still matches.
pub fn find(id: &str) -> Option<&'static Scheme> {
    let id = id.trim().to_ascii_lowercase();
    SCHEMES.iter().find(|s| s.id == id)
}

/// Position in [`SCHEMES`], for clients that keep the active scheme as an index.
pub fn index_of(id: &str) -> Option<usize> {
    let s = find(id)?;
    SCHEMES.iter().position(|x| x.id == s.id)
}

/// `candlelit, abyssal, ...` for error messages and help.
pub fn id_list() -> String {
    SCHEMES.iter().map(|s| s.id).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_first_and_keeps_the_users_terminal() {
        assert_eq!(SCHEMES[0].id, DEFAULT);
        assert!(find(DEFAULT).unwrap().term.is_none());
    }

    #[test]
    fn lookup_is_case_and_space_tolerant() {
        assert_eq!(find(" Ouija ").map(|s| s.id), Some("ouija"));
        assert_eq!(index_of("synth"), Some(8));
        assert!(find("nope").is_none());
    }

    #[test]
    fn ids_are_unique_lowercase_and_listed() {
        for (i, s) in SCHEMES.iter().enumerate() {
            assert_eq!(s.id, s.id.to_ascii_lowercase());
            assert!(!s.label.is_empty());
            assert!(SCHEMES[i + 1..].iter().all(|o| o.id != s.id), "{}", s.id);
            assert!(id_list().contains(s.id));
        }
        let light: Vec<_> = SCHEMES.iter().filter(|s| s.light).map(|s| s.id).collect();
        assert_eq!(light, ["ouija", "riso", "parlor", "herbarium"]);
    }

    #[test]
    fn every_token_is_a_24_bit_color() {
        for s in &SCHEMES {
            let c = s.chrome;
            for v in [
                c.bg,
                c.bg_elevated,
                c.surface,
                c.border,
                c.text,
                c.text_dim,
                c.text_faint,
                c.flame,
                c.flame_dim,
                c.violet,
                c.violet_dim,
                c.success,
                c.danger,
            ] {
                assert!(v <= 0xFF_FFFF, "{}", s.id);
            }
            // Text has to differ from the ground it sits on.
            assert_ne!(c.text, c.bg, "{}", s.id);
            if let Some(t) = s.term {
                assert!(t.ansi.iter().all(|v| *v <= 0xFF_FFFF), "{}", s.id);
                assert_ne!(t.fg, t.bg, "{}", s.id);
                assert_eq!(t.cursor, c.flame, "{}", s.id);
            } else {
                assert_eq!(s.id, DEFAULT);
            }
        }
    }

    #[test]
    fn ansi_ramp_ends_follow_the_ground() {
        let dark = find("abyssal").unwrap();
        let t = dark.term.unwrap();
        assert_eq!(t.ansi[0], dark.chrome.bg_elevated);
        assert_eq!(t.ansi[7], dark.chrome.text_dim);
        assert_eq!(t.ansi[8], 0x6E86A0);
        assert_eq!(t.ansi[9], t.ansi[1]);
        assert_eq!(t.ansi[15], dark.chrome.text);

        let light = find("riso").unwrap();
        let t = light.term.unwrap();
        assert_eq!(t.ansi[0], light.chrome.text);
        assert_eq!(t.ansi[7], light.chrome.surface);
        assert_eq!(t.ansi[15], light.chrome.bg_elevated);
    }
}

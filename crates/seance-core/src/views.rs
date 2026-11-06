//! Window layouts ("looks"): Classic, Stage and Chat (Wall is kept, hidden).
//!
//! Each GUI window picks its own and remembers it locally, so the daemon
//! never stores one; it only relays `ctl layout NAME` to every window
//! (`GuiEvent::View`) and keeps what each window last reported
//! (`GuiRequest::SetView`) so `ctl layout` can list them. Names travel as
//! plain strings so a window can skip one it does not know.

/// A window layout. Classic is today's rail plus pane grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ViewMode {
    #[default]
    Classic,
    Stage,
    /// Circles as chat threads, drawn from Invoke's history.
    Chat,
    /// Hidden since Chat took its place in the switcher; still reachable as
    /// the ctrl+shift+space peek.
    Wall,
}

impl ViewMode {
    /// Toggle and cycle order.
    pub const ALL: [ViewMode; 3] = [ViewMode::Classic, ViewMode::Stage, ViewMode::Chat];

    /// Wire and config name.
    pub fn id(self) -> &'static str {
        match self {
            ViewMode::Classic => "classic",
            ViewMode::Stage => "stage",
            ViewMode::Chat => "chat",
            ViewMode::Wall => "wall",
        }
    }

    /// Case-insensitive; `None` for anything else. A saved or relayed
    /// "wall" opens as Chat, which took its place.
    pub fn parse(s: &str) -> Option<ViewMode> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("wall") {
            return Some(ViewMode::Chat);
        }
        Self::ALL
            .into_iter()
            .find(|m| m.id().eq_ignore_ascii_case(s))
    }

    /// The next one in toggle order, wrapping.
    pub fn next(self) -> ViewMode {
        if self == ViewMode::Wall {
            return ViewMode::Classic;
        }
        let i = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

impl std::fmt::Display for ViewMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_parse_back_and_display_matches() {
        for m in ViewMode::ALL {
            assert_eq!(ViewMode::parse(m.id()), Some(m));
            assert_eq!(m.to_string(), m.id());
        }
        assert_eq!(ViewMode::parse(" Chat "), Some(ViewMode::Chat));
        assert_eq!(ViewMode::parse("STAGE"), Some(ViewMode::Stage));
    }

    #[test]
    fn a_saved_wall_opens_as_chat() {
        assert_eq!(ViewMode::parse("wall"), Some(ViewMode::Chat));
        assert_eq!(ViewMode::parse(" WALL "), Some(ViewMode::Chat));
    }

    #[test]
    fn unknown_names_are_none() {
        for bad in ["", "grid", "classic2", "overview"] {
            assert_eq!(ViewMode::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn next_cycles_in_toggle_order() {
        assert_eq!(ViewMode::Classic.next(), ViewMode::Stage);
        assert_eq!(ViewMode::Stage.next(), ViewMode::Chat);
        assert_eq!(ViewMode::Chat.next(), ViewMode::Classic);
        assert_eq!(ViewMode::Wall.next(), ViewMode::Classic);
        assert_eq!(ViewMode::default(), ViewMode::Classic);
    }
}

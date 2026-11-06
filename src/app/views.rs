//! Window layouts: Classic (the rail and pane grid), Stage and Chat; Wall is
//! hidden from the switch but still there as a peek. Each
//! window keeps its own, switched from the toggle, ctrl+shift+t or
//! `ctl layout`, and the last pick is saved on this machine
//! (`~/.config/seance/view.json`), never in the daemon's shared layout. The
//! same file keeps whether Wall shows every space (`wall_all`).
//!
//! Stage (`stage.rs`) and Wall (`wall.rs`) take the rail and grid's place:
//! on Stage the pane on stage is the only place its terminal renders, and
//! on Wall none does. ctrl+shift+space peeks at the Wall.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, SharedString};
pub(crate) use seance_core::views::ViewMode;

use crate::theme::SeancePalette;

use super::util::tip;
use super::SeanceApp;

/// One window's layout, plus where leaving Wall goes back to.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ViewState {
    pub(super) mode: ViewMode,
    before_wall: ViewMode,
    /// Wall's All tab: every circle from every space. Kept while on Stage
    /// or Classic, so the Wall comes back to it.
    pub(super) wall_all: bool,
    /// Off for blank windows, which show nothing on Wall and, like their
    /// rail list, must not overwrite this machine's pick (or snoozes).
    pub(super) persist: bool,
}

impl ViewState {
    /// This machine's last pick; Classic when there is none, and always
    /// for a blank window.
    pub(super) fn restore(empty: bool) -> Self {
        let text = if empty {
            String::new()
        } else {
            std::fs::read_to_string(config_path()).unwrap_or_default()
        };
        ViewState {
            mode: decode(&text),
            before_wall: ViewMode::Classic,
            wall_all: wall_all_of(&text),
            persist: !empty,
        }
    }
}

fn config_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("seance/view.json");
        }
    }
    PathBuf::from(shellexpand::tilde("~/.config/seance/view.json").as_ref())
}

/// `text` (the file as it was) with `key` set to `value`, keeping the rest.
fn put(text: &str, key: &str, value: impl Into<serde_json::Value>) -> String {
    let mut v = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    v[key] = value.into();
    v.to_string()
}

/// Anything unreadable or unknown is Classic, so a bad file never strands a
/// window on a layout it cannot show.
fn decode(text: &str) -> ViewMode {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("layout")?.as_str().and_then(ViewMode::parse))
        .unwrap_or_default()
}

fn wall_all_of(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("wall_all")?.as_bool())
        .unwrap_or(false)
}

fn save_key(key: &str, value: impl Into<serde_json::Value>) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, put(&old, key, value)).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn save(mode: ViewMode) {
    save_key("layout", mode.id());
}

pub(super) fn save_wall_all(on: bool) {
    save_key("wall_all", on);
}

/// Stage's snoozed panes (slug to until, unix ms); empty when none are kept.
fn snoozed_of(text: &str) -> std::collections::HashMap<String, u64> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| serde_json::from_value(v.get("snoozed")?.clone()).ok())
        .unwrap_or_default()
}

pub(super) fn saved_snoozes() -> std::collections::HashMap<String, u64> {
    snoozed_of(&std::fs::read_to_string(config_path()).unwrap_or_default())
}

pub(super) fn save_snoozes(map: &std::collections::HashMap<String, u64>) {
    save_key("snoozed", serde_json::json!(map));
}

impl SeanceApp {
    /// Tell the daemon where this window starts: its layout for `ctl layout`
    /// and, on Wall, the overview stream. Queued until the first Attach.
    pub(super) fn announce_view(&self) {
        let _ = self.client.set_view(self.view.mode.id());
        if self.overview() {
            let _ = self.client.set_overview(true);
        }
    }

    /// Full-window live overview (ctrl+shift+space): on exactly while the
    /// layout is Wall.
    pub(super) fn overview(&self) -> bool {
        self.view.mode == ViewMode::Wall
    }

    /// A deliberate pick (toggle, ctrl+shift+t, `ctl layout`): saved.
    pub(super) fn set_view(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        self.switch_view(mode, true, cx);
    }

    /// ctrl+shift+space is a peek, as the overview always was: it does not
    /// change the layout the next launch opens on.
    pub(super) fn peek_wall(&mut self, cx: &mut Context<Self>) {
        self.switch_view(ViewMode::Wall, false, cx);
    }

    fn switch_view(&mut self, mode: ViewMode, persist: bool, cx: &mut Context<Self>) {
        if mode == self.view.mode {
            return;
        }
        let was_wall = self.overview();
        if mode == ViewMode::Wall {
            self.view.before_wall = self.view.mode;
        } else if self.view.mode == ViewMode::Wall {
            self.wall_leave();
            // Stage and Classic show one space: the selected circle's. An
            // unplaced one already reads as the default's; pinning the rail
            // there would send it to the default when it is placed.
            if self.view.wall_all {
                let spaces = &self.subs_pref.spaces;
                if let Some(ws) = self
                    .selected_workspace
                    .as_deref()
                    .filter(|ws| spaces.of.contains_key(*ws))
                {
                    self.rail_space = Some(spaces.space_of(ws).to_string());
                }
            }
        }
        if mode == ViewMode::Stage {
            self.stage_enter();
        }
        self.view.mode = mode;
        self.tasks_popover = None;
        if mode == ViewMode::Chat {
            self.chat.focus_input = true;
            self.chat.arrive();
        }
        self.stage.peek = None;
        if self.overview() != was_wall {
            let _ = self.client.set_overview(!was_wall);
        }
        if persist && self.view.persist {
            save(mode);
        }
        let _ = self.client.set_view(mode.id());
        cx.notify();
    }

    /// On opens the Wall as a peek; off goes back to the layout it was
    /// opened from.
    pub(super) fn set_overview(&mut self, on: bool, cx: &mut Context<Self>) {
        if on {
            self.peek_wall(cx);
        } else {
            self.leave_wall(cx);
        }
    }

    /// Back to whichever layout Wall was opened from.
    pub(super) fn leave_wall(&mut self, cx: &mut Context<Self>) {
        if self.view.mode == ViewMode::Wall {
            self.set_view(self.view.before_wall, cx);
        }
    }

    /// `ctl layout NAME` reached this window. A name this build does not
    /// know is skipped.
    pub(super) fn apply_view_event(&mut self, view: &str, cx: &mut Context<Self>) {
        if let Some(mode) = ViewMode::parse(view) {
            self.set_view(mode, cx);
        }
    }

    /// Classic · Stage · Chat. Narrow enough for the slimmest rail: the
    /// segments share the width and the labels are one word.
    pub(super) fn render_view_switch(&self, key: &str, cx: &Context<Self>) -> gpui::AnyElement {
        let current = self.view.mode;
        div()
            .id(SharedString::from(format!("view-switch-{key}")))
            .flex_none()
            .flex()
            .flex_row()
            .p(px(2.))
            .gap(px(2.))
            .rounded_md()
            .border_1()
            .border_color(SeancePalette::border())
            .children(ViewMode::ALL.map(|mode| {
                let on = mode == current;
                div()
                    .id(SharedString::from(format!("view-{key}-{}", mode.id())))
                    .flex_1()
                    .min_w_0()
                    .px_1p5()
                    .rounded_sm()
                    .flex()
                    .justify_center()
                    .text_xs()
                    .overflow_hidden()
                    .cursor_pointer()
                    .when(on, |d| {
                        d.bg(SeancePalette::surface())
                            .text_color(SeancePalette::flame())
                    })
                    .when(!on, |d| {
                        d.text_color(SeancePalette::text_dim())
                            .hover(|s| s.text_color(SeancePalette::text()))
                    })
                    .tooltip(tip(match mode {
                        ViewMode::Classic => {
                            "classic: the rail and pane grid (ctrl+shift+t cycles)"
                        }
                        ViewMode::Stage => "stage: one circle's lead up front, crew beside it",
                        ViewMode::Chat => "chat: talk to each circle's lead, no terminal",
                        ViewMode::Wall => "wall: every circle live (ctrl+shift+space)",
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_view(mode, cx);
                    }))
                    .child(mode.id())
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_roundtrips_every_layout() {
        for mode in ViewMode::ALL {
            assert_eq!(decode(&put("", "layout", mode.id())), mode);
        }
        assert_eq!(put("", "layout", "stage"), r#"{"layout":"stage"}"#);
    }

    #[test]
    fn an_old_file_with_a_hud_key_still_loads() {
        // The progress HUD is gone; its key is ignored and kept as it was.
        let old = r#"{"layout":"stage","hud":"ink"}"#;
        assert_eq!(decode(old), ViewMode::Stage);
        let again = put(old, "layout", "chat");
        assert_eq!(decode(&again), ViewMode::Chat);
    }

    #[test]
    fn snoozes_share_the_file() {
        assert!(snoozed_of(r#"{"layout":"stage"}"#).is_empty());
        let map = std::collections::HashMap::from([("w".to_string(), 42u64)]);
        let text = put(r#"{"layout":"stage"}"#, "snoozed", serde_json::json!(map));
        assert_eq!(snoozed_of(&text), map);
        assert_eq!(decode(&text), ViewMode::Stage);
    }

    #[test]
    fn wall_all_shares_the_file() {
        assert!(!wall_all_of(r#"{"layout":"wall"}"#));
        let text = put(r#"{"layout":"wall"}"#, "wall_all", true);
        assert!(wall_all_of(&text));
        assert_eq!(decode(&text), ViewMode::Chat);
        assert!(!wall_all_of(&put(&text, "wall_all", false)));
    }

    #[test]
    fn bad_or_unknown_files_read_as_classic() {
        for text in [
            "",
            "stage",
            "{",
            "{}",
            r#"{"layout":"grid"}"#,
            r#"{"layout":3}"#,
        ] {
            assert_eq!(decode(text), ViewMode::Classic, "{text}");
        }
        // A layout saved as "wall" before Chat took its place opens as Chat.
        assert_eq!(decode(r#"{"layout":"Wall","later":1}"#), ViewMode::Chat);
    }
}

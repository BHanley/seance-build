//! Left rail for the SeanceApp view: the workspace/pane sidebar (circles in
//! your dragged order, pane drag-and-drop, per-row context menus, inline rename) and
//! the host-bridge widget strip (claude accounts) above the summon footer.

use gpui::{div, prelude::*, px, Context, SharedString, Window};
use gpui_component::{input::Input, menu::ContextMenuExt as _, StyledExt as _, WindowExt as _};

use crate::control::ControlRequest;
use crate::pane::PaneBody;
use crate::runtime::protocol::GuiRequest;
use crate::theme::SeancePalette;

/// Rail metrics. One left axis and one right edge: every glyph slot, every
/// name, and every time/count line up regardless of row kind.
const ROW_H: f32 = 28.;
/// Glyph column — fixed so a row with no glyph still starts its name on the
/// same line as one that has a spinner.
const GLYPH_W: f32 = 15.;
/// Pane rows under the selected circle sit a step smaller than circle rows.
const PANE_ROW_H: f32 = 22.;
/// Time / count column, right-aligned.
const TIME_W: f32 = 34.;
/// The view switch's narrowest width with "classic" whole: three equal
/// segments of the label (38.5 px at 12 px) plus padding, gaps and border.
const SWITCH_MIN_W: f32 = 162.;
/// The rail header's row around the switch: its padding and border, the
/// ✦, "seance" and the gaps.
const RAIL_MARK_W: f32 = 112.;

/// The view switch fits beside "✦ seance" on a rail this wide; narrower,
/// it takes its own row under the header.
fn rail_switch_inline(rail_w: f32) -> bool {
    rail_w - RAIL_MARK_W >= SWITCH_MIN_W
}

use super::actions::*;
use super::util::{
    needs_dot_color, pane_menu, selected_row_fill, sidebar_press_no_select, tip, tip_s, ui_debug,
    working_spinner_glyph, DragPill, DraggedCircle, DraggedPane, DraggedSpace,
};
use super::workspaces::{banish_arm_live, RailFilter, WorkspaceAttention};
use super::{RenameTarget, SashDrag, SeanceApp};

impl SeanceApp {
    /// `✦` popover: every GUI window attached to this daemon, with a kill
    /// affordance for the others (`CloseWindow` — the daemon unregisters the
    /// window and the client quits on `Kicked`). Anchored under the brand
    /// header; a transparent full-window backdrop behind it eats the
    /// click-away, same trick as the quicklaunch editor overlay.
    pub(super) fn render_gui_menu(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        if !self.gui_menu_open {
            return None;
        }
        let self_id = self.window_id.clone();
        // Stage's top row and Wall's space row (when it shows) hold the ✦;
        // Chat's ✦ sits at the top of its circle list, Classic's on the rail.
        let mark_row = match self.view.mode {
            super::views::ViewMode::Stage => true,
            super::views::ViewMode::Wall => self.space_tabs_shown(),
            super::views::ViewMode::Classic | super::views::ViewMode::Chat => false,
        };
        let top = if mark_row {
            super::stage::SPACE_ROW_H + 2.
        } else {
            40.
        };
        let rows: Vec<(String, String, usize)> = self
            .windows
            .iter()
            .map(|w| (w.id.clone(), w.label.clone(), w.workspace_count))
            .collect();
        Some(
            div()
                .id("gui-menu-backdrop")
                .absolute()
                .inset_0()
                // Swallow mouse events so a click-away doesn't also land in a
                // terminal underneath (and steal focus).
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.gui_menu_open = false;
                    cx.notify();
                }))
                .child(
                    div()
                        .id("gui-menu-card")
                        .absolute()
                        .left(px(8.))
                        .top(px(top))
                        .w(px(260.))
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .p_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(SeancePalette::flame_dim())
                        .bg(SeancePalette::bg_elevated())
                        // Clicks inside the card must not reach the backdrop.
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .px_1()
                                .text_xs()
                                .text_color(SeancePalette::text_faint())
                                .child("connected guis"),
                        )
                        .children(rows.into_iter().map(|(id, label, count)| {
                            let is_self = self_id.as_deref() == Some(id.as_str());
                            let circles = if count == 1 {
                                "1 circle".to_string()
                            } else {
                                format!("{count} circles")
                            };
                            div()
                                .px_1()
                                .py_0p5()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_color(SeancePalette::text())
                                        .child(SharedString::from(label)),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(SeancePalette::text_faint())
                                        .child(SharedString::from(circles)),
                                )
                                .child(if is_self {
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(SeancePalette::text_faint())
                                        .child("(this window)")
                                        .into_any_element()
                                } else {
                                    div()
                                        .id(SharedString::from(format!("gui-kill-{id}")))
                                        .flex_none()
                                        .px_1p5()
                                        .rounded_md()
                                        .text_xs()
                                        .cursor_pointer()
                                        .text_color(SeancePalette::text_faint())
                                        .hover(|s| {
                                            s.text_color(SeancePalette::danger())
                                                .bg(SeancePalette::surface())
                                        })
                                        .tooltip(tip("close that window"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let _ = this.client.send(GuiRequest::CloseWindow {
                                                window: id.clone(),
                                            });
                                            this.gui_menu_open = false;
                                            cx.notify();
                                        }))
                                        .child("kill")
                                        .into_any_element()
                                })
                        }))
                        .child(div().my_1().h(px(1.)).bg(SeancePalette::border()))
                        .children(self.render_theme_rows(false, cx))
                        .children(self.render_theme_rows(true, cx))
                        .child(div().my_1().h(px(1.)).bg(SeancePalette::border()))
                        .child(
                            div()
                                .px_1()
                                .text_xs()
                                .text_color(SeancePalette::text_faint())
                                .child(concat!("seance ", env!("CARGO_PKG_VERSION"))),
                        )
                        .child(
                            div()
                                .id("gui-menu-help")
                                .px_1()
                                .py_0p5()
                                .rounded_md()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .cursor_pointer()
                                .text_xs()
                                .text_color(SeancePalette::text_dim())
                                .hover(|s| {
                                    s.text_color(SeancePalette::flame())
                                        .bg(SeancePalette::surface())
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.gui_menu_open = false;
                                    this.open_help_window(cx);
                                    cx.notify();
                                }))
                                .child("grimoire")
                                .child("?"),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Host-bridge strip(s) above the summon footer. Empty when no host or poll failed.
    ///
    /// Collapsed (default): only the current/selected account. Click expands
    /// the full list; click an account to select it and collapse. Clicking the
    /// already-selected account collapses without re-running select.
    pub(super) fn render_host_sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
        if self.host.widgets.is_empty() {
            return div().flex_none().into_any_element();
        }
        div()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(SeancePalette::border())
            .children(self.host.widgets.iter().map(|w| {
                let title = if w.title.is_empty() {
                    w.id.clone()
                } else {
                    w.title.clone()
                };
                let widget_id = w.id.clone();
                let expanded = self.host_expanded.contains(&widget_id);
                let caret = if expanded { "▾" } else { "▸" };
                // Prefer explicit selected flag, then host `active`, then first.
                let current_id = w
                    .items
                    .iter()
                    .find(|i| i.selected)
                    .map(|i| i.id.clone())
                    .or_else(|| w.active.clone())
                    .or_else(|| w.items.first().map(|i| i.id.clone()));
                let visible: Vec<_> = if expanded {
                    w.items.iter().collect()
                } else {
                    w.items
                        .iter()
                        .filter(|i| current_id.as_deref() == Some(i.id.as_str()) || i.selected)
                        .collect()
                };
                div()
                    .flex()
                    .flex_col()
                    .py_1p5()
                    .gap_0p5()
                    .child(
                        div()
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .id(SharedString::from(format!("host-title-{widget_id}")))
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .cursor_pointer()
                                    .tooltip(tip(if expanded {
                                        "collapse accounts"
                                    } else {
                                        "expand accounts"
                                    }))
                                    .on_click({
                                        let wid = widget_id.clone();
                                        cx.listener(move |this, _, _, cx| {
                                            if this.host_expanded.contains(&wid) {
                                                this.host_expanded.remove(&wid);
                                            } else {
                                                this.host_expanded.insert(wid.clone());
                                            }
                                            cx.notify();
                                        })
                                    })
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(SeancePalette::text_faint())
                                            .child(format!("{caret} {title}")),
                                    ),
                            )
                            .when_some(w.error.as_ref(), |d, err| {
                                d.child(
                                    div()
                                        .id(SharedString::from(format!("host-err-{}", widget_id)))
                                        .text_xs()
                                        .text_color(SeancePalette::danger())
                                        .tooltip(tip_s(err.clone()))
                                        .child("!"),
                                )
                            }),
                    )
                    .children(visible.into_iter().map(|item| {
                        let wid = widget_id.clone();
                        let iid = item.id.clone();
                        let selected =
                            item.selected || current_id.as_deref() == Some(item.id.as_str());
                        let state = item.state.as_str();
                        let color = match state {
                            "busy" => SeancePalette::danger(),
                            "warm" => SeancePalette::flame(),
                            "auth" => SeancePalette::violet(),
                            _ if selected => SeancePalette::success(),
                            _ => SeancePalette::text_faint(),
                        };
                        let mark = if selected { "●" } else { "○" };
                        let label = item.label.clone();
                        let detail = item.detail.clone();
                        let detail2 = item.detail2.clone();
                        let tip_text = if !expanded {
                            format!("{label} · click to show all accounts")
                        } else if selected {
                            format!("{label} · current · click to collapse")
                        } else {
                            format!("switch to {label}")
                        };
                        // Full-bleed selected row (same fill as workspaces).
                        div()
                            .id(SharedString::from(format!("host-{wid}-{iid}")))
                            .flex()
                            .items_start()
                            .gap_1p5()
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .when(selected, |d| d.bg(selected_row_fill()))
                            .hover(|s| {
                                if selected {
                                    s.bg(crate::theme::lift(selected_row_fill(), 0.04))
                                } else {
                                    s.bg(SeancePalette::surface())
                                }
                            })
                            .tooltip(tip_s(tip_text))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.host_item_click(&wid, &iid, window, cx);
                            }))
                            .child(
                                div()
                                    .flex_none()
                                    .pt(px(1.))
                                    .text_xs()
                                    .text_color(color)
                                    .child(mark),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .font_weight(if selected {
                                                gpui::FontWeight::SEMIBOLD
                                            } else {
                                                gpui::FontWeight::NORMAL
                                            })
                                            .text_color(if selected {
                                                SeancePalette::text()
                                            } else {
                                                SeancePalette::text_dim()
                                            })
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_xs()
                                            .text_color(SeancePalette::text_faint())
                                            .child(detail),
                                    )
                                    .when(!detail2.is_empty(), |d| {
                                        d.child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_xs()
                                                .text_color(SeancePalette::text_faint())
                                                .child(detail2),
                                        )
                                    }),
                            )
                    }))
                    .into_any_element()
            }))
            .into_any_element()
    }

    /// Collapsed → expand. Expanded → select clicked account and collapse.
    /// Already-selected while expanded → collapse only (no re-switch).
    pub(super) fn host_item_click(
        &mut self,
        widget_id: &str,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let expanded = self.host_expanded.contains(widget_id);
        if !expanded {
            self.host_expanded.insert(widget_id.to_string());
            cx.notify();
            return;
        }

        let already = self
            .host
            .widgets
            .iter()
            .find(|w| w.id == widget_id)
            .map(|w| {
                w.items.iter().any(|i| i.id == item_id && i.selected)
                    || w.active.as_deref() == Some(item_id)
            })
            .unwrap_or(false);

        // Always collapse on the second click.
        self.host_expanded.remove(widget_id);
        if already {
            // No-op for selection — already current, don't re-run select_cmd.
            cx.notify();
            return;
        }
        self.host_select(widget_id, item_id, window, cx);
    }

    pub(super) fn host_select(
        &mut self,
        widget_id: &str,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Select runs daemon-side (thin client: the daemon machine's account
        // switches, not this one's) and can take seconds — keep it off the UI
        // thread and notify when it lands.
        let client = self.client.clone();
        let widget = widget_id.to_string();
        let item = item_id.to_string();
        let item_for_msg = item.clone();
        let _ = window; // notification lands via the async update below
        let task = cx
            .background_executor()
            .spawn(async move { client.host_select(&widget, &item) });
        cx.spawn_in(window, async move |this: gpui::WeakEntity<Self>, cx| {
            let result = task.await;
            let _ = cx.update(|window, cx| {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |_, cx| {
                        match result {
                            Ok(raw) => {
                                // Prefer host JSON message when present.
                                let msg = serde_json::from_str::<serde_json::Value>(&raw)
                                    .ok()
                                    .and_then(|v| {
                                        let email = v.get("email").and_then(|e| e.as_str());
                                        let id = v
                                            .get("id")
                                            .and_then(|e| e.as_str())
                                            .unwrap_or(&item_for_msg);
                                        Some(match email {
                                            Some(e) if !e.is_empty() && e != "unknown" => {
                                                format!("claude → {id} ({e})")
                                            }
                                            _ => format!("claude → {id}"),
                                        })
                                    })
                                    .unwrap_or_else(|| format!("claude → {item_for_msg}"));
                                window.push_notification(
                                    gpui_component::notification::Notification::success(msg),
                                    cx,
                                );
                            }
                            Err(e) => {
                                window.push_notification(
                                    gpui_component::notification::Notification::error(format!(
                                        "switch failed: {e}"
                                    )),
                                    cx,
                                );
                            }
                        }
                        cx.notify();
                    });
                }
            });
        })
        .detach();
    }

    /// Filter chips over the rail: all · pins · one per host mode. An empty
    /// chip hides (unless it's the active filter). Right-click a mode chip to
    /// turn the mode off for every circle in it.
    fn render_rail_chips(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let ordered = self.workspaces();
        let mut chips: Vec<(RailFilter, String, String)> = vec![
            (RailFilter::All, "all".into(), String::new()),
            (RailFilter::Starred, "pins".into(), String::new()),
        ];
        for d in &self.circle_mode_defs {
            chips.push((
                RailFilter::Mode(d.id.clone()),
                d.label.clone(),
                d.off_label.clone(),
            ));
        }
        let els: Vec<gpui::AnyElement> = chips
            .into_iter()
            .enumerate()
            .filter_map(|(i, (filter, label, off_label))| {
                let n = ordered
                    .iter()
                    .filter(|w| self.rail_filter_matches(&filter, w))
                    .count();
                let on = self.rail_filter == filter;
                if n == 0 && !on && filter != RailFilter::All {
                    return None;
                }
                let hot = n > 0 && !matches!(filter, RailFilter::All | RailFilter::Starred);
                let click = filter.clone();
                let chip = div()
                    .id(SharedString::from(format!("rail-chip-{i}")))
                    .flex_none()
                    .px_1p5()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .border_1()
                    .border_color(if on {
                        SeancePalette::flame()
                    } else if hot {
                        SeancePalette::violet_dim()
                    } else {
                        SeancePalette::border()
                    })
                    .bg(if on {
                        SeancePalette::flame()
                    } else {
                        SeancePalette::surface()
                    })
                    .text_color(if on {
                        SeancePalette::bg()
                    } else if hot {
                        SeancePalette::violet()
                    } else {
                        SeancePalette::text_dim()
                    })
                    .hover(|s| s.border_color(SeancePalette::flame_dim()))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|_this, _, window, cx| {
                            sidebar_press_no_select(window, cx);
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.rail_filter = if this.rail_filter == click {
                            RailFilter::All
                        } else {
                            click.clone()
                        };
                        cx.notify();
                    }))
                    .child(format!("{label} {n}"));
                Some(match filter {
                    RailFilter::Mode(id) if !off_label.is_empty() && n > 0 => chip
                        .context_menu(move |menu, _, _| {
                            menu.menu(
                                format!("{off_label} — all {n}"),
                                Box::new(ActCircleModeAllOff(id.clone())),
                            )
                        })
                        .into_any_element(),
                    RailFilter::Starred if n > 0 => chip
                        .context_menu(move |menu, _, _| {
                            menu.menu(format!("unpin all {n}"), Box::new(ActUnpinAll(true)))
                        })
                        .into_any_element(),
                    _ => chip.into_any_element(),
                })
            })
            .collect();
        // "all" alone filters nothing; give the rail its row back.
        if els.len() <= 1 {
            return div().into_any_element();
        }
        div()
            .flex_none()
            .px_2()
            .py_1p5()
            .flex()
            .flex_wrap()
            .gap_1()
            .border_b_1()
            .border_color(SeancePalette::border())
            .children(els)
            .into_any_element()
    }

    /// One space in the rail's top tier. Click shows its circles below; drop
    /// a circle's row on it to move the circle there.
    fn render_space_row(
        &self,
        space: &seance_core::spaces::Space,
        current: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let id = space.id.clone();
        if let Some((RenameTarget::Space(r), input)) = &self.renaming {
            if *r == id {
                return div()
                    .px_2()
                    .py_1p5()
                    .child(Input::new(input))
                    .into_any_element();
            }
        }
        let spaces = &self.subs_pref.spaces;
        let is_default = spaces.default_id() == id;
        let count = spaces.circles_in(&id, &self.workspaces()).len();
        let name = space.name.clone();
        let (id_click, id_drop, id_menu) = (id.clone(), id.clone(), id.clone());
        let (id_space_drop, id_drag, id_arm) = (id.clone(), id.clone(), id.clone());
        let name_click = name.clone();
        let drag_name = name.clone();
        let armed = self.space_banish_armed(&id);
        let (glyph, color) = self
            .space_mark(&id)
            .unwrap_or(("", SeancePalette::text_faint()));
        // flex_none + one font weight: a row that shrinks or re-measures when
        // selected makes the whole rail jump on every click.
        div()
            .id(SharedString::from(format!("space-{id}")))
            .flex_none()
            .h(px(ROW_H))
            .pr_2()
            .pl(px(5.))
            .border_l_3()
            .border_color(if current {
                SeancePalette::flame()
            } else {
                gpui::transparent_black()
            })
            .when(current, |d| d.bg(SeancePalette::flame().opacity(0.22)))
            .flex()
            .items_center()
            .gap_1p5()
            .cursor_pointer()
            .hover(move |s| {
                if current {
                    s.bg(SeancePalette::flame().opacity(0.3))
                } else {
                    s.bg(SeancePalette::surface())
                }
            })
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|_this, _, window, cx| {
                    sidebar_press_no_select(window, cx);
                }),
            )
            .drag_over::<DraggedCircle>(|style, _, _, _| {
                style.bg(SeancePalette::flame_dim().opacity(0.35))
            })
            .on_drop(cx.listener(move |this, drag: &DraggedCircle, _, cx| {
                this.move_circle_to_space(&drag.slug, &id_drop);
                cx.notify();
            }))
            // Spaces only move by hand: drop one on another to sit above it.
            .on_drag(DraggedSpace { id: id_drag }, move |_, _, _, cx| {
                let label = format!("▤ {drag_name}");
                cx.new(|_| DragPill { label })
            })
            .drag_over::<DraggedSpace>(|style, _, _, _| style.bg(SeancePalette::violet_dim()))
            .on_drop(cx.listener(move |this, drag: &DraggedSpace, _, cx| {
                if this
                    .subs_pref
                    .spaces
                    .reorder(&drag.id, Some(&id_space_drop))
                {
                    this.save_arrangement();
                }
                cx.notify();
            }))
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                if ev.click_count() == 2 {
                    this.start_rename(
                        RenameTarget::Space(id_click.clone()),
                        &name_click,
                        window,
                        cx,
                    );
                } else {
                    this.show_space(&id_click, window, cx);
                    cx.notify();
                }
            }))
            .context_menu(move |menu, _, _| {
                let m = menu
                    .menu("rename space", Box::new(ActRenameSpace(id_menu.clone())))
                    .menu("new space", Box::new(ActNewSpace(true)))
                    .menu("move up", Box::new(ActSpaceShift(id_menu.clone(), -1)))
                    .menu("move down", Box::new(ActSpaceShift(id_menu.clone(), 1)));
                let m = if is_default {
                    m
                } else {
                    m.menu("make default", Box::new(ActSpaceDefault(id_menu.clone())))
                };
                let m = m.separator();
                let m = if count > 0 {
                    m.menu(
                        format!("banish all {count} circle(s)…"),
                        Box::new(ActBanishSpace(id_menu.clone())),
                    )
                } else {
                    m
                };
                if is_default {
                    m
                } else {
                    m.menu(
                        "delete space (circles go to the default)",
                        Box::new(ActDeleteSpace(id_menu.clone())),
                    )
                }
            })
            .child(
                div()
                    .flex_none()
                    .w(px(GLYPH_W))
                    .text_sm()
                    .text_color(color)
                    .child(glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(if current {
                        SeancePalette::flame()
                    } else {
                        SeancePalette::text_dim()
                    })
                    .child(name),
            )
            .when(is_default && !armed, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(SeancePalette::text_faint())
                        .child("default"),
                )
            })
            .when(armed, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("space-banish-{id}")))
                        .flex_none()
                        .px_1()
                        .rounded_sm()
                        .text_xs()
                        .cursor_pointer()
                        .bg(SeancePalette::danger().opacity(0.2))
                        .text_color(SeancePalette::danger())
                        .hover(|s| s.bg(SeancePalette::danger().opacity(0.35)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.banish_space_click(&id_arm, window, cx);
                            cx.notify();
                        }))
                        .child(format!("banish {count}? click")),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(px(TIME_W))
                    .text_xs()
                    .text_right()
                    .text_color(SeancePalette::text_faint())
                    .child(count.to_string()),
            )
            .into_any_element()
    }

    /// The rail's lower tier: the current space's circles, or every match
    /// for an active filter.
    fn render_circle_list(&self, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
        let circles = self.visible_workspaces();
        if circles.is_empty() {
            let msg = match self.rail_filter {
                RailFilter::All => "empty — drag a circle onto this space",
                _ => "nothing matches",
            };
            return vec![div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(SeancePalette::text_faint())
                .child(msg)
                .into_any_element()];
        }
        circles
            .into_iter()
            .map(|ws| self.render_workspace_row(ws, cx))
            .collect()
    }

    fn render_workspace_row(&self, workspace: String, cx: &Context<Self>) -> gpui::AnyElement {
        let selected = self.selected_workspace.as_deref() == Some(workspace.as_str());
        // Drives the menu verb only — the pinned *section* is composed by
        // `render_sidebar`, so the row itself needs no other special-casing.
        let pinned = self.subs_pref.is_pinned(&workspace);
        let ws_for_click = workspace.clone();
        let ws_for_group_drop = workspace.clone();
        let ws_for_pane_drop = workspace.clone();
        let ws_for_menu = workspace.clone();
        let renaming_this_ws = matches!(
            &self.renaming,
            Some((RenameTarget::Workspace(w), _)) if *w == workspace
        );
        let rename_input = self.renaming.as_ref().map(|(_, i)| i.clone());
        // Sleep verbs: only offered when the daemon says every pane in the
        // circle can be put back exactly (it checks the filesystem, we can't).
        let asleep = self.workspace_asleep(&workspace);
        let sleepable = !asleep && self.workspace_sleepable(&workspace);
        let attention = self.row_attention(&workspace);
        // Host circle modes (AFK…): badges on the row, toggles in its menu.
        let modes_on = self.circle_modes_of(&workspace);
        let mode_defs = self.circle_mode_defs.clone();
        let in_mode = !modes_on.is_empty();
        let here = self.subs_pref.spaces.space_of(&workspace).to_string();
        let move_targets: Vec<(String, String)> = self
            .subs_pref
            .spaces
            .list
            .iter()
            .filter(|s| s.id != here)
            .map(|s| (s.id.clone(), s.name.clone()))
            .collect();
        let drag_slug = workspace.clone();
        let drag_label = self.workspace_label(&workspace);
        let header: gpui::AnyElement = if renaming_this_ws {
            div()
                .px_2()
                .py_1p5()
                .children(rename_input.map(|i| Input::new(&i)))
                .into_any_element()
        } else {
            div()
                .id(SharedString::from(format!("ws-{workspace}")))
                .group(SharedString::from(format!("wsgrp-{workspace}")))
                .h(px(ROW_H))
                .pr_2()
                // 3px of the left inset is the selection anchor, so the text
                // never shifts when a row becomes selected.
                .border_l_3()
                .border_color(if selected {
                    SeancePalette::flame()
                } else if in_mode {
                    // In a host mode (AFK): its own edge colour, so the band
                    // reads as "set aside" at a glance, selected or not.
                    SeancePalette::violet()
                } else {
                    gpui::transparent_black()
                })
                .when(in_mode && !selected, |d| {
                    d.bg(SeancePalette::violet().opacity(0.08))
                })
                .pl(px(5.))
                .flex()
                .items_center()
                .gap_1p5()
                .cursor_pointer()
                .when(asleep && !selected, |d| d.opacity(0.62))
                .when(selected, |d| d.bg(SeancePalette::flame().opacity(0.22)))
                .hover(move |s| {
                    if selected {
                        s.bg(SeancePalette::flame().opacity(0.3))
                    } else {
                        s.bg(SeancePalette::surface())
                    }
                })
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|_this, _, window, cx| {
                        sidebar_press_no_select(window, cx);
                    }),
                )
                .on_drag(DraggedCircle { slug: drag_slug }, move |_, _, _, cx| {
                    let label = format!("◈ {drag_label}");
                    cx.new(|_| DragPill { label })
                })
                // Drop a pane onto the header → move into this circle.
                .drag_over::<DraggedPane>(|style, _, _, _| style.bg(SeancePalette::violet_dim()))
                .on_drop(cx.listener(move |this, drag: &DraggedPane, _, cx| {
                    ui_debug(&format!(
                        "drop pane '{}' on workspace header '{}'",
                        drag.slug, ws_for_pane_drop
                    ));
                    this.reorder_pane(&drag.slug, &ws_for_pane_drop, None, cx);
                }))
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            let label = this.workspace_label(&ws_for_click);
                            this.start_rename(
                                RenameTarget::Workspace(ws_for_click.clone()),
                                &label,
                                window,
                                cx,
                            );
                        } else {
                            this.select_workspace(&ws_for_click, window, cx);
                        }
                    }),
                )
                .context_menu({
                    let ws_m = ws_for_menu.clone();
                    let modes_on = modes_on.clone();
                    move |menu, window, cx| {
                        // Pin leads: it's the one item aimed at the rail
                        // itself (where this row sits), and the one you reach
                        // for often enough to want it under the cursor.
                        let m = if pinned {
                            menu.menu("unpin", Box::new(ActUnpinWorkspace(ws_m.clone())))
                        } else {
                            menu.menu("pin", Box::new(ActPinWorkspace(ws_m.clone())))
                        };
                        let targets = move_targets.clone();
                        let ws_s = ws_m.clone();
                        let m = m.submenu("move to space", window, cx, move |sub, _, _| {
                            targets.iter().fold(sub, |sub, (id, name)| {
                                sub.menu(
                                    name.clone(),
                                    Box::new(ActMoveToSpace(ws_s.clone(), id.clone())),
                                )
                            })
                        });
                        // Host modes (AFK): one toggle each, worded for the
                        // direction it goes. Display-only modes (slack thread)
                        // carry no labels and get no entry.
                        let m = mode_defs.iter().fold(m, |m, d| {
                            let on = modes_on.iter().any(|x| x.id == d.id);
                            let label = if on { &d.off_label } else { &d.on_label };
                            if label.is_empty() {
                                return m;
                            }
                            m.menu(
                                label.clone(),
                                Box::new(ActCircleMode(ws_m.clone(), d.id.clone(), !on)),
                            )
                        });
                        let m = m
                            .menu(
                                "rename workspace",
                                Box::new(ActRenameWorkspace(ws_m.clone())),
                            )
                            .menu("share replay…", Box::new(ActShareReplay(ws_m.clone())));
                        let m = if asleep {
                            m.menu("awaken circle", Box::new(ActWakeWorkspace(ws_m.clone())))
                        } else if sleepable {
                            m.menu("sleep circle", Box::new(ActSleepWorkspace(ws_m.clone())))
                        } else {
                            m
                        };
                        m.separator().menu(
                            "banish workspace (kill all panes)",
                            Box::new(ActKillWorkspace(ws_m.clone())),
                        )
                    }
                })
                .child({
                    // The glyph slot earns its ink or stays empty. A diamond
                    // on every idle row is a dozen identical marks that say
                    // nothing; leaving them off is what makes the two rows
                    // that ARE doing something impossible to miss.
                    let working = matches!(attention, Some(WorkspaceAttention::Working));
                    // Selection is carried by the flame anchor down the left
                    // edge, which frees the glyph to keep saying what the
                    // circle is DOING — the selected row can be working too.
                    let needs = matches!(attention, Some(WorkspaceAttention::NeedsHuman));
                    let (glyph, color) = if needs {
                        ("●", needs_dot_color())
                    } else if working {
                        (working_spinner_glyph(), SeancePalette::flame())
                    } else if selected {
                        ("◆", SeancePalette::flame())
                    } else if asleep {
                        ("☾", SeancePalette::text_faint())
                    } else {
                        ("", SeancePalette::text_faint())
                    };
                    // Fixed width: every name in the rail starts on the same
                    // vertical line whether or not its row has a glyph.
                    div()
                        .flex_none()
                        .w(px(GLYPH_W))
                        .text_sm()
                        .text_color(color)
                        .child(glyph)
                })
                .child({
                    let name_color = if selected {
                        SeancePalette::flame()
                    } else {
                        SeancePalette::text_dim()
                    };
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .text_sm()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(name_color)
                                .child(self.workspace_label(&workspace)),
                        )
                })
                .children(modes_on.iter().map(|d| {
                    div()
                        .flex_none()
                        .px_1()
                        .rounded_sm()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .bg(SeancePalette::violet().opacity(0.18))
                        .text_color(SeancePalette::violet())
                        .child(d.label.clone())
                }))
                .children({
                    // `done` is worth a mark but not a shout.
                    let att = if selected {
                        None
                    } else {
                        attention.filter(|a| {
                            matches!(a, WorkspaceAttention::Done | WorkspaceAttention::Later)
                        })
                    };
                    att.map(|a| {
                        div()
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .bg(a.color().opacity(0.14))
                            .text_color(a.color())
                            .child(a.label())
                    })
                })
                .child({
                    // Banish ×: revealed only while the row is hovered
                    // (group-hover), so idle rows stay quiet. A first click
                    // only arms it — armed, it names the damage ("banish? 3")
                    // and stays lit until a second click or BANISH_ARM.
                    let armed = banish_arm_live(
                        self.banish_armed.as_ref(),
                        &workspace,
                        std::time::Instant::now(),
                    );
                    let panes = self
                        .panes
                        .iter()
                        .filter(|p| p.workspace == workspace)
                        .count();
                    div()
                        .id(SharedString::from(format!("ws-banish-{workspace}")))
                        .flex_none()
                        .px_1()
                        .rounded_sm()
                        .text_xs()
                        .when(!armed, |d| {
                            d.text_color(gpui::transparent_black()).group_hover(
                                SharedString::from(format!("wsgrp-{workspace}")),
                                |s| s.text_color(SeancePalette::text_faint()),
                            )
                        })
                        .when(armed, |d| {
                            d.text_color(SeancePalette::danger())
                                .bg(SeancePalette::surface())
                        })
                        .hover(|s| {
                            s.text_color(SeancePalette::danger())
                                .bg(SeancePalette::surface())
                        })
                        .cursor_pointer()
                        .on_click({
                            let ws = workspace.clone();
                            cx.listener(move |this, _, window, cx| {
                                this.banish_click(&ws, window, cx);
                            })
                        })
                        .tooltip(tip(if armed {
                            "click again to banish — kills every pane in this circle"
                        } else {
                            "banish workspace (kill all panes)"
                        }))
                        .child(if armed {
                            format!("banish? {panes}")
                        } else {
                            "×".to_string()
                        })
                })
                .child({
                    // Fixed width, right-aligned — headers put their counts in
                    // the same column, so the whole rail has one hard right
                    // edge instead of a ragged gutter.
                    let label = self.workspace_activity_label(&workspace);
                    div()
                        .flex_none()
                        .w(px(TIME_W))
                        .text_xs()
                        .text_right()
                        .text_color(if selected {
                            SeancePalette::text_dim()
                        } else {
                            SeancePalette::text_faint()
                        })
                        .child(label.unwrap_or_default())
                })
                .into_any_element()
        };
        div()
            .id(SharedString::from(format!("wsgroup-{workspace}")))
            .flex()
            .flex_col()
            .gap_0p5()
            .mb_0p5()
            .drag_over::<DraggedPane>(|style, _, _, _| style.bg(SeancePalette::surface()))
            .on_drop(cx.listener(move |this, drag: &DraggedPane, _, cx| {
                ui_debug(&format!(
                    "drop pane '{}' on workspace group '{}'",
                    drag.slug, ws_for_group_drop
                ));
                this.reorder_pane(&drag.slug, &ws_for_group_drop, None, cx);
            }))
            .child(header)
            .when(selected, |d| {
                d.children(self.render_pane_rows(&workspace, cx))
            })
            .into_any_element()
    }

    /// The selected circle's panes, one row each under its header: glyph,
    /// name, and a hover × that arms on the first click and kills on the
    /// second. Hovering a row rings that pane's tile. File panes come last,
    /// under a faint "files" label.
    fn render_pane_rows(&self, workspace: &str, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
        let active = self.active_slug.as_deref();
        let (files, others): (Vec<_>, Vec<_>) = self
            .panes
            .iter()
            .filter(|p| p.workspace == workspace)
            .partition(|p| matches!(p.body, PaneBody::File { .. }));
        let first_file = others.len();
        let has_files = !files.is_empty();
        let mut rows: Vec<gpui::AnyElement> = others
            .into_iter()
            .chain(files)
            .map(|pane| {
                let slug = pane.slug.clone();
                let is_active = active == Some(slug.as_str());
                let later = self.subs_pref.later.contains(&slug);
                let grp = SharedString::from(format!("panegrp-{slug}"));
                let armed = banish_arm_live(
                    self.pane_kill_armed.as_ref(),
                    &slug,
                    std::time::Instant::now(),
                );
                let (glyph, color) = match self.pane_attention(&slug) {
                    Some(WorkspaceAttention::NeedsHuman) => ("●", needs_dot_color()),
                    Some(WorkspaceAttention::Working) => {
                        (working_spinner_glyph(), SeancePalette::flame())
                    }
                    _ if self.done_unseen.contains(&slug) => {
                        ("✓", WorkspaceAttention::Done.color())
                    }
                    _ if later => ("●", WorkspaceAttention::Later.color()),
                    _ => ("", SeancePalette::text_faint()),
                };
                div()
                    .id(SharedString::from(format!("rail-pane-{slug}")))
                    .group(grp.clone())
                    .h(px(PANE_ROW_H))
                    .pr_2()
                    // Glyph lines up under the circle's name.
                    .pl(px(3. + 5. + GLYPH_W + 6.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .hover(|s| s.bg(SeancePalette::surface()))
                    .on_hover(cx.listener({
                        let slug = slug.clone();
                        move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                if this.rail_hover_pane.as_deref() != Some(slug.as_str()) {
                                    this.rail_hover_pane = Some(slug.clone());
                                    cx.notify();
                                }
                            } else if this.rail_hover_pane.as_deref() == Some(slug.as_str()) {
                                this.rail_hover_pane = None;
                                cx.notify();
                            }
                        }
                    }))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|_this, _, window, cx| {
                            sidebar_press_no_select(window, cx);
                        }),
                    )
                    .on_click(cx.listener({
                        let slug = slug.clone();
                        move |this, _, window, cx| {
                            this.set_active(&slug, window, cx);
                        }
                    }))
                    .context_menu({
                        let slug = slug.clone();
                        move |menu, _, _| pane_menu(menu, &slug, later, false)
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(px(GLYPH_W))
                            .text_xs()
                            .text_color(color)
                            .child(glyph),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(if is_active {
                                SeancePalette::text()
                            } else {
                                SeancePalette::text_dim()
                            })
                            .child(pane.name.clone()),
                    )
                    .child(
                        // Same × as the circle row's banish: hidden until the
                        // row is hovered, armed red by a first click.
                        div()
                            .id(SharedString::from(format!("rail-pane-kill-{slug}")))
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .text_xs()
                            .when(!armed, |d| {
                                d.text_color(gpui::transparent_black())
                                    .group_hover(grp, |s| s.text_color(SeancePalette::text_faint()))
                            })
                            .when(armed, |d| {
                                d.text_color(SeancePalette::danger())
                                    .bg(SeancePalette::surface())
                            })
                            .hover(|s| {
                                s.text_color(SeancePalette::danger())
                                    .bg(SeancePalette::surface())
                            })
                            .cursor_pointer()
                            .on_click(cx.listener({
                                let slug = slug.clone();
                                move |this, _, window, cx| {
                                    // The row's own click would focus the pane.
                                    cx.stop_propagation();
                                    this.pane_kill_click(&slug, window, cx);
                                }
                            }))
                            .tooltip(tip(if armed {
                                "click again to kill this pane"
                            } else {
                                "kill pane"
                            }))
                            .child(if armed { "kill?" } else { "×" }),
                    )
                    .into_any_element()
            })
            .collect();
        if has_files {
            rows.insert(
                first_file,
                div()
                    .h(px(PANE_ROW_H))
                    .pl(px(3. + 5. + GLYPH_W + 6. + GLYPH_W + 6.))
                    .flex()
                    .items_end()
                    .pb(px(2.))
                    .text_xs()
                    .text_color(SeancePalette::text_faint())
                    .child("files")
                    .into_any_element(),
            );
        }
        rows
    }

    /// The dark or light color schemes in the ✦ menu under their own
    /// heading, the active one marked. A click asks the daemon, which saves
    /// it and repaints every window; the menu stays open so schemes can be
    /// tried one after another.
    fn render_theme_rows(&self, light: bool, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
        let active = crate::theme::active().id;
        let heading = div()
            .px_1()
            .when(light, |d| d.pt_1())
            .text_xs()
            .text_color(SeancePalette::text_faint())
            .child(if light { "light themes" } else { "dark themes" })
            .into_any_element();
        std::iter::once(heading)
            .chain(
                seance_core::schemes::SCHEMES
                    .iter()
                    .filter(|s| s.light == light)
                    .map(|s| {
                        let id = s.id;
                        let is_active = id == active;
                        let c = s.chrome;
                        div()
                            .id(SharedString::from(format!("theme-{id}")))
                            .px_1()
                            .py_0p5()
                            .rounded_md()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(if is_active {
                                SeancePalette::flame()
                            } else {
                                SeancePalette::text_dim()
                            })
                            .hover(|st| {
                                st.text_color(SeancePalette::flame())
                                    .bg(SeancePalette::surface())
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = this.client.send(GuiRequest::Ctl {
                                    req: ControlRequest::Theme {
                                        id: Some(id.to_string()),
                                        scope: None,
                                        from: None,
                                    },
                                });
                                cx.notify();
                            }))
                            .child(div().flex_none().w(px(10.)).child(if is_active {
                                "●"
                            } else {
                                ""
                            }))
                            .child(div().flex_1().min_w_0().child(s.label))
                            // Ground, accent, second: enough to tell them apart.
                            .child(div().flex_none().flex().gap_0p5().children(
                                [c.bg, c.flame, c.violet].map(|v| {
                                    div()
                                        .size(px(9.))
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(SeancePalette::border())
                                        .bg(gpui::rgb(v))
                                }),
                            ))
                            .into_any_element()
                    }),
            )
            .collect()
    }

    pub(super) fn render_sidebar(
        &self,
        window_active: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let current = self.current_space();
        let filtered = self.rail_filter != RailFilter::All;
        let mut rail_rows: Vec<gpui::AnyElement> = self
            .subs_pref
            .spaces
            .list
            .iter()
            .map(|sp| self.render_space_row(sp, !filtered && sp.id == current, cx))
            .collect();
        // The divider doubles as "drop here to move a space to the end".
        rail_rows.push(
            div()
                .id("space-end")
                .flex_none()
                .h(px(9.))
                .w_full()
                .flex()
                .items_center()
                .drag_over::<DraggedSpace>(|style, _, _, _| style.bg(SeancePalette::violet_dim()))
                .on_drop(cx.listener(|this, drag: &DraggedSpace, _, cx| {
                    if this.subs_pref.spaces.reorder(&drag.id, None) {
                        this.save_arrangement();
                    }
                    cx.notify();
                }))
                .child(div().h(px(1.)).w_full().bg(SeancePalette::border()))
                .into_any_element(),
        );
        rail_rows.extend(self.render_circle_list(cx));

        let _ = window_active; // focus chrome reserved for future empty-window dimming
        let switch_inline = rail_switch_inline(self.sidebar_width);

        div()
            .id("sidebar")
            .flex_none()
            .relative()
            .w(px(self.sidebar_width))
            .h_full()
            .flex()
            .flex_col()
            .bg(SeancePalette::bg_elevated())
            .border_r_1()
            .border_color(SeancePalette::border())
            .child(
                // Brand header: the name on top, the new-space/circle buttons
                // tucked under it so a narrow rail never squeezes either.
                div()
                    .flex_none()
                    .px_3()
                    .pt_2()
                    .pb_1p5()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .border_b_1()
                    .border_color(SeancePalette::border())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("gui-census")
                                    .px_1()
                                    .rounded_md()
                                    .text_color(SeancePalette::flame_dim())
                                    .text_lg()
                                    .cursor_pointer()
                                    .hover(|s| {
                                        s.text_color(SeancePalette::flame())
                                            .bg(SeancePalette::surface())
                                    })
                                    .tooltip(tip("connected guis · theme"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.gui_menu_open = !this.gui_menu_open;
                                        cx.notify();
                                    }))
                                    .child("✦"),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(SeancePalette::text())
                                    .text_sm()
                                    .font_semibold()
                                    .child("seance"),
                            )
                            .when(switch_inline, |d| {
                                d.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(self.render_view_switch("rail", cx)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .id("new-space")
                                    .flex_none()
                                    .px_1p5()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(SeancePalette::violet_dim())
                                    .hover(|s| {
                                        s.text_color(SeancePalette::violet())
                                            .bg(SeancePalette::surface())
                                    })
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.create_space(window, cx);
                                        cx.notify();
                                    }))
                                    .tooltip(tip("new space (name it immediately)"))
                                    .child("+ space"),
                            )
                            .child(
                                div()
                                    .id("new-workspace")
                                    .flex_none()
                                    .px_1p5()
                                    .rounded_md()
                                    .text_xs()
                                    .text_color(SeancePalette::violet_dim())
                                    .hover(|s| {
                                        s.text_color(SeancePalette::violet())
                                            .bg(SeancePalette::surface())
                                    })
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.create_workspace(window, cx);
                                    }))
                                    .tooltip(tip(
                                        "new empty circle in this space (name it immediately)",
                                    ))
                                    .child("+ circle"),
                            ),
                    )
                    .when(!switch_inline, |d| {
                        d.child(div().pt_1().child(self.render_view_switch("rail", cx)))
                    }),
            )
            .child(self.render_rail_chips(cx))
            .child({
                // Workspace list only — context menus live on *rows*, not the scroller.
                // Empty-area multi-window menu is a separate flex filler (avoids double menus).
                div()
                    .id("pane-list")
                    .track_scroll(&self.sidebar_scroll)
                    .flex_1()
                    .overflow_y_scroll()
                    // No horizontal pad — selected workspace fill is full-bleed.
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(rail_rows)
                    .child(div().id("sidebar-empty-hit").flex_1().min_h(px(48.)).w_full())
            })
            .child(self.render_quicklaunch(cx))
            .child(self.render_host_sidebar(cx))
            .child(
                // Footer: summon + help.
                div()
                    .flex_none()
                    .p_2()
                    .border_t_1()
                    .border_color(SeancePalette::border())
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id("summon")
                            .flex_1()
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .text_sm()
                            .text_color(SeancePalette::flame())
                            .bg(SeancePalette::surface())
                            .hover(|s| s.bg(SeancePalette::border()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.new_default_session(cx);
                            }))
                            .tooltip(tip(
                                "new shell pane in this workspace (ctrl+shift+n) — name it in the sidebar",
                            ))
                            .child("+ summon"),
                    )
                    .child(
                        div()
                            .id("activity")
                            .flex_none()
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .text_sm()
                            .text_color(SeancePalette::text_dim())
                            .bg(SeancePalette::surface())
                            .hover(|s| s.bg(SeancePalette::border()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.drawer = if matches!(this.drawer, super::Drawer::Activity) {
                                    super::Drawer::Closed
                                } else {
                                    super::Drawer::Activity
                                };
                                cx.notify();
                            }))
                            .tooltip(tip("activity feed — who did what, live"))
                            .child("≋"),
                    )
                    .child(
                        div()
                            .id("help")
                            .flex_none()
                            .px_3()
                            .py_1p5()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .text_sm()
                            .text_color(SeancePalette::violet())
                            .bg(SeancePalette::surface())
                            .hover(|s| s.bg(SeancePalette::border()))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_help_window(cx);
                            }))
                            .tooltip(tip("open the grimoire — full guide to seance"))
                            .child("?"),
                    ),
            )
            .child(
                div()
                    .id("sidebar-resize")
                    .absolute()
                    .right_0()
                    .top_0()
                    .w(px(5.))
                    .h_full()
                    .cursor_col_resize()
                    .hover(|s| s.bg(SeancePalette::flame_dim()))
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, ev: &gpui::MouseDownEvent, window, cx| {
                        sidebar_press_no_select(window, cx);
                        this.sash_drag = Some(SashDrag::Sidebar {
                            start_x: ev.position.x.into(),
                            width: this.sidebar_width,
                        });
                        cx.stop_propagation();
                        cx.notify();
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_joins_the_mark_once_the_rail_fits_it() {
        assert!(!rail_switch_inline(160.));
        assert!(!rail_switch_inline(232.));
        assert!(!rail_switch_inline(273.));
        assert!(rail_switch_inline(274.));
        assert!(rail_switch_inline(520.));
    }
}

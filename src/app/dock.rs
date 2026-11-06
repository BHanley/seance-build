//! Dock panel: file panes (trackers, decisions, todo lists) shown in a
//! right-side panel beside the grid instead of as tiles. One tab per docked
//! file in the selected circle. The pad and activity drawers take the same
//! spot and cover the panel while they're open.
//!
//! A docked pane is untiled on the daemon (so the grid, overview and shelf
//! logic all skip it) and listed in `subs_pref.docked`, which every window
//! shares.

use gpui::{div, prelude::*, px, Context, SharedString, Window};

use crate::pane::{Pane, PaneBody};
use crate::theme::SeancePalette;

use super::util::tip;
use super::{Drawer, SeanceApp};

impl SeanceApp {
    pub(super) fn is_docked(&self, p: &Pane) -> bool {
        !p.tiled
            && p.popped.is_none()
            && matches!(p.body, PaneBody::File { .. })
            && self.subs_pref.docked.contains(&p.slug)
    }

    fn docked_in(&self, ws: &str) -> Vec<&Pane> {
        self.panes
            .iter()
            .filter(|p| p.workspace == ws && self.is_docked(p))
            .collect()
    }

    /// `share` pushes the arrangement to the daemon (a click); a spawn every
    /// window hears saves locally only.
    pub(super) fn dock_pane(&mut self, slug: &str, share: bool, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.iter_mut().find(|p| p.slug == slug) else {
            return;
        };
        if !matches!(pane.body, PaneBody::File { .. }) {
            return;
        }
        pane.tiled = false;
        let ws = pane.workspace.clone();
        let _ = self.client.set_tiled(slug, false);
        self.subs_pref.docked.insert(slug.to_string());
        if share {
            self.save_arrangement();
        } else {
            self.save_arrangement_local();
        }
        self.dock_tab.insert(ws, slug.to_string());
        self.drawer = Drawer::Closed;
        // Its tile is gone: drop a zoom on it and move focus to a grid pane.
        if self.zoomed_slug.as_deref() == Some(slug) {
            self.zoomed_slug = None;
        }
        if self.active_slug.as_deref() == Some(slug) {
            self.active_slug = None;
            self.ensure_active_pane_in_workspace();
        }
        cx.notify();
    }

    pub(super) fn undock_pane(&mut self, slug: &str, cx: &mut Context<Self>) {
        if self.subs_pref.docked.remove(slug) {
            self.save_arrangement();
        }
        if let Some(pane) = self.panes.iter_mut().find(|p| p.slug == slug) {
            pane.tiled = true;
        }
        let _ = self.client.set_tiled(slug, true);
        cx.notify();
    }

    /// Bring a docked pane's tab up (sidebar row click). False when the pane
    /// isn't docked, so the caller focuses it in the grid as usual.
    pub(super) fn show_docked(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ws) = self
            .panes
            .iter()
            .find(|p| p.slug == slug && self.is_docked(p))
            .map(|p| p.workspace.clone())
        else {
            return false;
        };
        // Guarded: select_workspace can land back here when a circle holds
        // only docked panes.
        if self.selected_workspace.as_deref() != Some(ws.as_str()) {
            self.select_workspace(&ws, window, cx);
        }
        self.dock_tab.insert(ws, slug.to_string());
        self.drawer = Drawer::Closed;
        cx.notify();
        true
    }

    pub(super) fn render_dock_panel(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let ws = self.selected_workspace.as_deref()?;
        let docked = self.docked_in(ws);
        let open = self
            .dock_tab
            .get(ws)
            .and_then(|s| docked.iter().find(|p| &p.slug == s))
            .or(docked.first())?;
        let open_slug = open.slug.clone();
        let tabs = docked.iter().map(|p| {
            let slug = p.slug.clone();
            let is_open = slug == open_slug;
            let ws = ws.to_string();
            div()
                .id(SharedString::from(format!("dock-tab-{slug}")))
                .flex_none()
                .h_full()
                .px_2()
                .flex()
                .items_center()
                .text_xs()
                .border_b_2()
                .border_color(if is_open {
                    SeancePalette::flame()
                } else {
                    gpui::transparent_black()
                })
                .text_color(if is_open {
                    SeancePalette::text()
                } else {
                    SeancePalette::text_faint()
                })
                .hover(|s| s.text_color(SeancePalette::text()))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.dock_tab.insert(ws.clone(), slug.clone());
                    cx.notify();
                }))
                .child(p.name.clone())
        });
        Some(
            div()
                .flex_none()
                .w(px(420.))
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(SeancePalette::border())
                .bg(SeancePalette::bg_elevated())
                .child(
                    div()
                        .flex_none()
                        .h(px(30.))
                        .pl_1()
                        .pr_3()
                        .flex()
                        .items_center()
                        .border_b_1()
                        .border_color(SeancePalette::border())
                        .child(
                            div()
                                .id("dock-tabs")
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .flex()
                                .overflow_x_scroll()
                                .children(tabs),
                        )
                        .child(
                            div()
                                .id("dock-undock")
                                .flex_none()
                                .px_1()
                                .text_sm()
                                .text_color(SeancePalette::text_faint())
                                .hover(|s| s.text_color(SeancePalette::flame()))
                                .cursor_pointer()
                                .tooltip(tip("put this file back in the grid"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.undock_pane(&open_slug, cx);
                                }))
                                .child("⇤"),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(open.content_element()),
                )
                .into_any_element(),
        )
    }
}

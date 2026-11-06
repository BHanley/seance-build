//! Pad inspector: scratchpad and task sidecars read through the daemon cache.

use gpui::{div, prelude::*, Context, SharedString};

use crate::theme::SeancePalette;

use super::util::status_color;
use super::{Drawer, SeanceApp};

impl SeanceApp {
    pub(super) fn open_pad_drawer(&mut self, slug: &str, cx: &mut Context<Self>) {
        self.drawer = Drawer::Pad {
            slug: slug.to_string(),
        };
        cx.notify();
    }

    /// Bridge path (tilde expands DAEMON-side) for a scratch sidecar.
    pub(super) fn scratch_path(slug: &str, ext: &str) -> String {
        format!("~/.local/share/seance/scratch/{slug}.{ext}")
    }

    /// Read pad body + task sidecar (daemon files, via cache — render-safe).
    fn load_pad_bundle(&self, slug: &str) -> (String, Option<String>, Option<serde_json::Value>) {
        let pad = self
            .remote_cache
            .get(&Self::scratch_path(slug, "md"))
            .unwrap_or_default();
        let task_id = self
            .remote_cache
            .get(&Self::scratch_path(slug, "taskid"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let task_json = self
            .remote_cache
            .get(&Self::scratch_path(slug, "task.json"))
            .and_then(|s| serde_json::from_str(&s).ok());
        (pad, task_id, task_json)
    }

    pub(super) fn phone_bind_path(slug: &str) -> String {
        Self::scratch_path(slug, "telegram.json")
    }

    pub(super) fn render_pad_drawer(&self, slug: &str, cx: &Context<Self>) -> impl IntoElement {
        // Include tick so GPUI re-renders when pad_refresh_tick advances.
        let _tick = self.pad_refresh_tick;
        let _ = cx;
        let name = self
            .panes
            .iter()
            .find(|p| p.slug == slug)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| slug.to_string());
        let st = self.statuses.get(slug);
        let (pad, task_id, task_json) = self.load_pad_bundle(slug);
        let status_line = match st {
            Some(s) => match &s.note {
                Some(n) if !n.is_empty() => format!("{} · {n}", s.state),
                _ => s.state.clone(),
            },
            None => "—".into(),
        };
        let task_body = task_json
            .as_ref()
            .and_then(|v| v.get("body").and_then(|b| b.as_str()))
            .unwrap_or("")
            .to_string();
        let task_status = task_json
            .as_ref()
            .and_then(|v| v.get("status").and_then(|b| b.as_str()))
            .unwrap_or("-");
        let task_id_s = task_id.unwrap_or_else(|| "—".into());

        let pad_display = if pad.trim().is_empty() {
            "(empty pad)".to_string()
        } else {
            // Show tail so latest finish is visible without scroll thrash.
            let lines: Vec<&str> = pad.lines().collect();
            if lines.len() > 80 {
                let mut s = String::from("…\n");
                s.push_str(&lines[lines.len() - 80..].join("\n"));
                s
            } else {
                pad
            }
        };
        let task_display = if task_body.trim().is_empty() {
            "(no active/recent inject body)".to_string()
        } else {
            if task_body.len() > 2500 {
                let mut end = 2500;
                while end > 0 && !task_body.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}…", &task_body[..end])
            } else {
                task_body
            }
        };

        let slug_flip = slug.to_string();

        div()
            .id(SharedString::from(format!("pad-drawer-{slug}")))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .text_color(SeancePalette::flame())
                            .child(format!("{name}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(SeancePalette::text_faint())
                            .child(format!("`{slug}`")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(status_color(st.map(|s| s.state.as_str()).unwrap_or("-")))
                            .child(status_line),
                    ),
            )
            .child(
                div().flex().gap_2().child(
                    div()
                        .id(SharedString::from(format!("pad-flip-{slug}")))
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .text_xs()
                        .text_color(SeancePalette::flame())
                        .bg(SeancePalette::surface())
                        .hover(|s| s.bg(SeancePalette::border()))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.flip_notes_for(&slug_flip, window, cx);
                        }))
                        .child("✎ edit notes"),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(SeancePalette::violet())
                    .child(format!("task {task_id_s} · {task_status}")),
            )
            .child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(SeancePalette::bg())
                    .border_1()
                    .border_color(SeancePalette::border())
                    .text_xs()
                    .text_color(SeancePalette::text_dim())
                    .child(task_display),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(SeancePalette::violet())
                    .child("pad (tail)"),
            )
            .child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(SeancePalette::bg())
                    .border_1()
                    .border_color(SeancePalette::border())
                    .text_xs()
                    .text_color(SeancePalette::text())
                    .child(pad_display),
            )
            .into_any_element()
    }
}

//! Workspace identity: a lifetime **UUID**, a stable **slug**, and a mutable **label**.
//! Slugs can be reused after deletion; UUIDs are persisted and never reused.
//!
//! Panes have had this split since the beginning — `slug` is the id, `name` is
//! what you read — and it is why renaming a pane costs nothing. Circles did
//! not: the display name *was* the key, so a rename rewrote the identity and
//! every structure keyed by it had to be migrated by hand. Eight of them in
//! the daemon, six more plus the rail prefs in the native GUI, the same
//! again in the web client — and one that no migration can ever reach, the
//! `SEANCE_WORKSPACE` already baked into a running pane's environment. That
//! last one is unfixable under the old model: you cannot write into the
//! environment of a process that is already running.
//!
//! So circles get the split panes always had. The slug is minted once, from
//! the name it was created with, and never rewritten. Rename changes the
//! label and nothing else — there is no longer anything to migrate, which is
//! why this module deletes far more code than it adds.
//!
//! Addressing accepts either form (`resolve_workspace`): an exact slug wins,
//! then an unambiguous label match. Same precedence as pane lookup, for the
//! same reason — a slug is unique, a label is not.

use super::Engine;
use crate::state::{new_identity_uuid, slugify, unique_slug};

impl Engine {
    /// Resolve an explicit lifetime ID without ever falling back to a reused
    /// slug/name. Non-ID addresses retain the caller's existing resolution.
    pub(super) fn pane_slug_from_id(&self, key: &str) -> Result<String, String> {
        match key.strip_prefix("id:") {
            Some(id) => self
                .panes
                .iter()
                .find(|p| p.pane_id == id)
                .map(|p| p.slug.clone())
                .ok_or_else(|| format!("no pane '{key}'")),
            None => Ok(key.to_string()),
        }
    }

    pub(super) fn ensure_workspace_id(&mut self, slug: &str) {
        let id = self.workspace_ids.entry(slug.to_string()).or_default();
        if id.is_empty() {
            *id = new_identity_uuid();
        }
    }

    /// Every known circle slug: pane-bearing, empty-but-created, and ordered.
    pub(super) fn known_workspace_slugs(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in self
            .panes
            .iter()
            .map(|p| p.workspace.clone())
            .chain(self.extra_workspaces.iter().cloned())
            .chain(self.workspace_order.iter().cloned())
        {
            if !out.contains(&s) {
                out.push(s);
            }
        }
        out
    }

    /// The human-facing label for a circle. Falls back to the slug, which is
    /// what every circle reads as until someone renames it.
    pub fn workspace_label(&self, slug: &str) -> String {
        self.workspace_names
            .get(slug)
            .cloned()
            .unwrap_or_else(|| slug.to_string())
    }

    /// Label map as sorted pairs — deterministic on the wire and in state.json.
    pub(super) fn sorted_workspace_names(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .workspace_names
            .iter()
            .map(|(k, n)| (k.clone(), n.clone()))
            .collect();
        v.sort();
        v
    }

    /// Resolve a circle by slug **or** label to its slug.
    ///
    /// Slug first: it is the identity and is unique by construction. Labels are
    /// free text and may collide, so a label match only resolves when exactly
    /// one circle carries it — otherwise the caller gets `None` and an
    /// "ambiguous" error rather than a coin flip. (Panes learned this the hard
    /// way in 0.14.2, where a display name shadowed another pane's slug.)
    pub fn resolve_workspace(&self, key: &str) -> Option<String> {
        let known = self.known_workspace_slugs();
        if known.iter().any(|s| s == key) {
            return Some(key.to_string());
        }
        let mut hits = known
            .iter()
            .filter(|s| self.workspace_names.get(*s).is_some_and(|n| n == key));
        let first = hits.next()?;
        if hits.next().is_some() {
            return None; // ambiguous label — caller reports it
        }
        Some(first.clone())
    }

    /// True when `key` matches more than one circle's label (so a `None` from
    /// [`Self::resolve_workspace`] can say *why*).
    pub fn workspace_key_is_ambiguous(&self, key: &str) -> bool {
        self.known_workspace_slugs()
            .iter()
            .filter(|s| self.workspace_names.get(*s).is_some_and(|n| n == key))
            .count()
            > 1
    }

    /// Mint a slug for a new circle from the name the human typed, unique
    /// against every circle that already exists. Records the label when it
    /// differs from the slug — so "Growth Work" reads as itself while being
    /// keyed by `growth-work`.
    pub fn create_workspace(&mut self, name: &str) -> String {
        let name = name.trim();
        let known = self.known_workspace_slugs();
        let taken: Vec<&str> = known.iter().map(|s| s.as_str()).collect();
        let slug = unique_slug(name, &taken);
        self.ensure_workspace_id(&slug);
        if !name.is_empty() && name != slug {
            self.workspace_names.insert(slug.clone(), name.to_string());
        }
        if !self.extra_workspaces.contains(&slug) && !self.panes.iter().any(|p| p.workspace == slug)
        {
            self.extra_workspaces.push(slug.clone());
        }
        if !self.workspace_order.iter().any(|w| w == &slug) {
            self.workspace_order.push(slug.clone());
        }
        slug
    }

    /// Drop a workspace's row when its last pane dies and it was never
    /// explicitly created (`extra_workspaces`). An empty circle nobody asked
    /// for is dead chrome in both sidebars.
    pub(crate) fn prune_workspace_if_empty(&mut self, workspace: &str) {
        if workspace.is_empty()
            || self.extra_workspaces.iter().any(|w| w == workspace)
            || self.panes.iter().any(|p| p.workspace == workspace)
        {
            return;
        }
        self.forget_workspace(workspace);
    }

    /// Remove every trace of a workspace: order, clocks, label, subscriptions.
    pub(crate) fn forget_workspace(&mut self, workspace: &str) {
        self.workspace_order.retain(|w| w != workspace);
        self.workspace_output.remove(workspace);
        self.workspace_touch_ms.remove(workspace);
        self.forget_workspace_label(workspace);
        if self.selected_workspace.as_deref() == Some(workspace) {
            self.selected_workspace = self.panes.first().map(|p| p.workspace.clone());
        }
        self.drop_workspace_subs(workspace);
    }

    /// Rename = set the label. The slug does not move, so nothing keyed by it
    /// needs migrating: panes, activity clocks,
    /// subscriptions, selections, client rail prefs and every running
    /// pane's `SEANCE_WORKSPACE` all keep pointing at the same circle.
    ///
    /// Returns the resolved slug, or `None` when `key` names nothing.
    pub fn rename_workspace(&mut self, key: &str, new_label: &str) -> Option<String> {
        let slug = self.resolve_workspace(key)?;
        let old = self.workspace_label(&slug);
        let label = new_label.trim();
        if label.is_empty() || label == slug {
            self.workspace_names.remove(&slug);
        } else {
            self.workspace_names.insert(slug.clone(), label.to_string());
        }
        // Not a migration: identity stays on the slug. The old label keeps
        // resolving as an alias, and whoever talks to this circle is told.
        let new = self.workspace_label(&slug);
        self.on_circle_renamed(&slug, &old, &new);
        Some(slug)
    }

    /// Retire a circle's identity and label when the circle itself is gone.
    pub(super) fn forget_workspace_label(&mut self, slug: &str) {
        self.workspace_names.remove(slug);
        self.workspace_ids.remove(slug);
    }

    /// Resolve every `scope` / `workspace` field in a request — control-plane
    /// or GUI — from slug-or-label to the circle's **slug**, before any
    /// handler sees one.
    ///
    /// Done generically over the serialized form rather than per-variant: the
    /// request is internally tagged (`{"op":…}`), so this is a two-key rewrite
    /// on a JSON object and a new variant carrying a `workspace` gets the
    /// behaviour without anyone remembering to add it. Threading it through
    /// ~20 hand-written call sites is exactly how a rule like this rots.
    ///
    /// A key that resolves to nothing is left **verbatim**, so it goes on
    /// matching no circle and the caller still gets "outside your workspace"
    /// rather than being silently promoted to unscoped.
    pub(super) fn normalize_workspace_keys<T>(&self, req: T) -> T
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let Ok(mut v) = serde_json::to_value(&req) else {
            return req;
        };
        let mut touched = false;
        if let Some(obj) = v.as_object_mut() {
            for key in ["scope", "workspace"] {
                let Some(raw) = obj.get(key).and_then(|x| x.as_str()).map(str::to_string) else {
                    continue;
                };
                if let Some(slug) = self.resolve_workspace(&raw) {
                    if slug != raw {
                        obj.insert(key.to_string(), serde_json::Value::String(slug));
                        touched = true;
                    }
                }
            }
        }
        if !touched {
            return req;
        }
        serde_json::from_value(v).unwrap_or(req)
    }

    /// Slug for a *spawn* target: an existing circle addressed by either form,
    /// or a new circle minted from the given name.
    pub(super) fn workspace_slug_for_spawn(&mut self, requested: &str) -> String {
        if let Some(slug) = self.resolve_workspace(requested) {
            return slug;
        }
        let slug = slugify(requested);
        if !requested.is_empty() && requested != slug {
            self.workspace_names
                .insert(slug.clone(), requested.to_string());
        }
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::with_test_state_dir;
    use super::*;
    use std::path::PathBuf;

    fn engine(tag: &str) -> (Engine, PathBuf) {
        let dir = std::env::temp_dir().join(format!("seance-ws-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (eng, _rx) = Engine::bare_for_test(dir.clone());
        (eng, dir)
    }

    #[test]
    fn rename_moves_the_label_and_leaves_the_slug_alone() {
        with_test_state_dir("ws-rename", || {
            let (mut eng, dir) = engine("rename");
            let slug = eng.push_stub_pane("worker", "lab");
            eng.workspace_order.push("lab".into());
            eng.workspace_output.insert("lab".into(), 42);

            assert_eq!(
                eng.rename_workspace("lab", "Growth Work").as_deref(),
                Some("lab")
            );

            // The identity did not move: the pane, the clock, the order entry
            // and any running pane's SEANCE_WORKSPACE all still say `lab`.
            assert_eq!(
                eng.panes.iter().find(|p| p.slug == slug).unwrap().workspace,
                "lab"
            );
            assert_eq!(eng.workspace_output.get("lab"), Some(&42));
            assert!(eng.workspace_order.iter().any(|w| w == "lab"));
            // Only the label changed.
            assert_eq!(eng.workspace_label("lab"), "Growth Work");

            // And it is addressable by either form afterwards.
            assert_eq!(eng.resolve_workspace("lab").as_deref(), Some("lab"));
            assert_eq!(eng.resolve_workspace("Growth Work").as_deref(), Some("lab"));

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn renaming_back_to_the_slug_drops_the_label_entirely() {
        with_test_state_dir("ws-rename-back", || {
            let (mut eng, dir) = engine("rename-back");
            eng.push_stub_pane("worker", "lab");
            eng.rename_workspace("lab", "Growth Work");
            eng.rename_workspace("lab", "lab");
            assert!(eng.workspace_names.is_empty(), "no dead label left behind");
            assert_eq!(eng.workspace_label("lab"), "lab");
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn a_new_circle_keeps_the_typed_name_as_its_label() {
        with_test_state_dir("ws-create", || {
            let (mut eng, dir) = engine("create");
            let slug = eng.create_workspace("Growth Work");
            assert_eq!(slug, "growth-work");
            assert_eq!(eng.workspace_label(&slug), "Growth Work");

            // A second circle with the same typed name gets a distinct slug —
            // labels may collide, identity may not.
            let slug2 = eng.create_workspace("Growth Work");
            assert_ne!(slug, slug2);
            assert_eq!(eng.workspace_label(&slug2), "Growth Work");

            // Which makes that label ambiguous, and addressing says so rather
            // than picking one.
            assert!(eng.resolve_workspace("Growth Work").is_none());
            assert!(eng.workspace_key_is_ambiguous("Growth Work"));
            // The slugs still resolve exactly.
            assert_eq!(eng.resolve_workspace(&slug).as_deref(), Some(slug.as_str()));
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn a_slug_always_beats_another_circles_label() {
        with_test_state_dir("ws-shadow", || {
            let (mut eng, dir) = engine("shadow");
            eng.push_stub_pane("a", "lab");
            eng.push_stub_pane("b", "studio");
            // Give `studio` the label "lab" — now the string "lab" is both a
            // real slug and someone else's label.
            eng.rename_workspace("studio", "lab");
            assert_eq!(eng.resolve_workspace("lab").as_deref(), Some("lab"));
            let _ = std::fs::remove_dir_all(&dir);
        });
    }
}

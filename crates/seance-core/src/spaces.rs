//! Spaces: the rail's top tier. A space is a named bucket of circles the human
//! arranges; every circle sits in exactly one.
//!
//! Lives inside the rail-arrangement blob (`subscriptions.json`, key
//! `spaces`) that the daemon stores opaquely and every client edits — native
//! (`src/subscriptions_pref.rs`) and web (`seance-web/src/subs.rs`) both embed
//! [`Spaces`], so a client that drops the field on save wipes everyone's
//! spaces. The daemon's ctl verbs edit the same field in place.
//!
//! Rules (decided 2026-10-10):
//! - One space is the **default** (`general` on first run). It can be renamed,
//!   never deleted; it catches everything without a better home.
//! - Circles move only by hand. A new circle lands in the space on screen.
//!   Labels never route anything.
//! - No other space is special.
//!
//! Reconcile is deterministic in its inputs because two windows may reconcile
//! the same new circle at once.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub const DEFAULT_ID: &str = "general";
pub const DEFAULT_NAME: &str = "general";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spaces {
    /// Rail order.
    #[serde(default)]
    pub list: Vec<Space>,
    #[serde(default)]
    pub default: String,
    /// Circle slug → space id.
    #[serde(default)]
    pub of: BTreeMap<String, String>,
}

impl Spaces {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Space> {
        self.list.iter().find(|s| s.id == id)
    }

    fn has(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    pub fn default_id(&self) -> &str {
        if self.has(&self.default) {
            &self.default
        } else {
            self.list
                .first()
                .map(|s| s.id.as_str())
                .unwrap_or(DEFAULT_ID)
        }
    }

    /// The space a circle renders in. Unassigned (not reconciled yet) reads as
    /// the default so no circle is ever invisible.
    pub fn space_of(&self, circle: &str) -> &str {
        match self.of.get(circle) {
            Some(id) if self.has(id) => id,
            _ => self.default_id(),
        }
    }

    /// `ordered` filtered to one space, order preserved.
    pub fn circles_in(&self, id: &str, ordered: &[String]) -> Vec<String> {
        ordered
            .iter()
            .filter(|c| self.space_of(c) == id)
            .cloned()
            .collect()
    }

    /// `ordered` grouped by space in list order, each space's circles in
    /// `ordered`'s order: Wall's All tab.
    pub fn all_circles(&self, ordered: &[String]) -> Vec<String> {
        let mut out = ordered.to_vec();
        out.sort_by_key(|c| self.list.iter().position(|s| s.id == self.space_of(c)));
        out
    }

    /// Exact id, else a unique case-insensitive name.
    pub fn resolve(&self, key: &str) -> Option<String> {
        if self.has(key) {
            return Some(key.to_string());
        }
        let mut hits = self
            .list
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(key));
        let first = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        Some(first.id.clone())
    }

    fn fresh_id(&self, base: &str) -> String {
        let base = crate::util::slugify(base);
        if !self.has(&base) {
            return base;
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|id| !self.has(id))
            .expect("unbounded")
    }

    /// Bring the model in line with the live circles. `circles` is
    /// (slug, label); `focused` is the space the human is looking at, where a
    /// brand-new circle lands. Returns whether it changed.
    pub fn reconcile(&mut self, circles: &[(String, String)], focused: Option<&str>) -> bool {
        let before = self.clone();
        if !self.has(&self.default) {
            if !self.has(DEFAULT_ID) {
                self.list.insert(
                    0,
                    Space {
                        id: DEFAULT_ID.into(),
                        name: DEFAULT_NAME.into(),
                    },
                );
            }
            self.default = DEFAULT_ID.into();
        }

        let live: BTreeSet<&str> = circles.iter().map(|(s, _)| s.as_str()).collect();
        self.of.retain(|c, _| live.contains(c.as_str()));

        let landing = focused
            .filter(|f| self.has(f))
            .unwrap_or_else(|| self.default_id())
            .to_string();
        for (slug, _) in circles {
            if !self.of.get(slug).is_some_and(|id| self.has(id)) {
                self.of.insert(slug.clone(), landing.clone());
            }
        }
        *self != before
    }

    pub fn create(&mut self, name: &str) -> String {
        let id = self.fresh_id(name);
        self.list.push(Space {
            id: id.clone(),
            name: name.trim().to_string(),
        });
        id
    }

    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        match self.list.iter_mut().find(|s| s.id == id) {
            Some(s) if !name.is_empty() && s.name != name => {
                s.name = name.to_string();
                true
            }
            _ => false,
        }
    }

    /// The default can't be deleted. Its circles move to the default.
    pub fn delete(&mut self, id: &str) -> bool {
        if id == self.default_id() || !self.has(id) {
            return false;
        }
        let d = self.default_id().to_string();
        for s in self.of.values_mut() {
            if s == id {
                *s = d.clone();
            }
        }
        self.list.retain(|s| s.id != id);
        true
    }

    pub fn move_circle(&mut self, circle: &str, id: &str) -> bool {
        if !self.has(id) {
            return false;
        }
        self.of
            .insert(circle.to_string(), id.to_string())
            .as_deref()
            != Some(id)
    }

    /// Move space `id` to sit before `before` (end when `None`).
    pub fn reorder(&mut self, id: &str, before: Option<&str>) -> bool {
        let Some(from) = self.list.iter().position(|s| s.id == id) else {
            return false;
        };
        if before == Some(id) {
            return false;
        }
        let snapshot = self.list.clone();
        let s = self.list.remove(from);
        let to = before
            .and_then(|b| self.list.iter().position(|x| x.id == b))
            .unwrap_or(self.list.len());
        self.list.insert(to, s);
        self.list != snapshot
    }

    pub fn set_default(&mut self, id: &str) -> bool {
        if !self.has(id) || self.default == id {
            return false;
        }
        self.default = id.to_string();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(items: &[&str]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|s| (s.to_string(), s.to_string()))
            .collect()
    }
    fn ids(sp: &Spaces) -> Vec<&str> {
        sp.list.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn first_run_is_just_the_default() {
        let mut sp = Spaces::default();
        let circles = c(&["paceline-a", "paceline-b", "ui-a", "ui-b", "wander"]);
        assert!(sp.reconcile(&circles, None));
        assert_eq!(ids(&sp), vec!["general"]);
        assert_eq!(sp.space_of("ui-b"), "general", "labels never route");
        assert!(!sp.reconcile(&circles, None), "idempotent");
    }

    #[test]
    fn two_windows_reconciling_in_different_orders_agree() {
        let a = c(&["x-1a", "x-2b", "y", "z-q"]);
        let mut b = a.clone();
        b.reverse();
        let (mut s1, mut s2) = (Spaces::default(), Spaces::default());
        s1.reconcile(&a, None);
        s2.reconcile(&b, None);
        assert_eq!(s1, s2);
    }

    #[test]
    fn a_new_circle_lands_in_the_focused_space() {
        let mut sp = Spaces::default();
        sp.reconcile(&c(&["a"]), None);
        let w = sp.create("work");
        sp.reconcile(&c(&["a", "ui-1x", "ui-2y"]), Some(&w));
        assert_eq!(sp.space_of("ui-1x"), w);
        assert_eq!(sp.space_of("a"), "general", "existing circles stay put");
        assert_eq!(ids(&sp), vec!["general", w.as_str()]);
    }

    #[test]
    fn all_circles_go_by_space_then_circle_order() {
        let names =
            |items: &[&str]| -> Vec<String> { items.iter().map(|s| s.to_string()).collect() };
        let mut sp = Spaces::default();
        sp.reconcile(&c(&["a", "b", "c", "d"]), None);
        let later = sp.create("later");
        let work = sp.create("work");
        sp.move_circle("a", &work);
        sp.move_circle("b", &later);
        sp.move_circle("d", &later);
        let ordered = names(&["a", "b", "c", "d"]);
        assert_eq!(sp.all_circles(&ordered), names(&["c", "b", "d", "a"]));
        assert_eq!(
            sp.all_circles(&names(&["d", "a", "b"])),
            names(&["d", "b", "a"])
        );
    }

    #[test]
    fn a_moved_circle_stays_and_an_empty_space_survives() {
        let mut sp = Spaces::default();
        let circles = c(&["ui-a", "ui-b"]);
        sp.reconcile(&circles, None);
        let later = sp.create("later");
        assert!(sp.move_circle("ui-a", &later));
        assert!(!sp.move_circle("ui-a", &later));
        sp.reconcile(&circles, None);
        assert_eq!(sp.space_of("ui-a"), later);
        sp.reconcile(&c(&["ui-b"]), None);
        assert!(sp.get(&later).is_some());
    }

    #[test]
    fn legacy_prefix_fields_load_as_plain_spaces() {
        let sp: Spaces = serde_json::from_str(
            r#"{"list":[{"id":"general","name":"general"},
                {"id":"ui","name":"ui","prefix":"ui","auto":true}],
                "default":"general","of":{"ui-a":"ui"},"placed":["ui-a"]}"#,
        )
        .unwrap();
        assert_eq!(sp.space_of("ui-a"), "ui");
        assert!(serde_json::to_string(&sp).unwrap().find("prefix").is_none());
    }

    #[test]
    fn deleting_a_space_drops_its_circles_to_the_default_for_good() {
        let mut sp = Spaces::default();
        sp.reconcile(&c(&["ui-a", "ui-b"]), None);
        let p = sp.create("paceline");
        sp.move_circle("ui-a", &p);
        assert!(sp.delete(&p));
        sp.reconcile(&c(&["ui-a", "ui-b"]), None);
        assert_eq!(sp.space_of("ui-a"), "general");
        assert!(!sp.delete("general"), "the default can't be deleted");
    }

    #[test]
    fn the_default_can_move_and_be_renamed() {
        let mut sp = Spaces::default();
        sp.reconcile(&c(&["a"]), None);
        assert!(sp.rename("general", "misc WIP"));
        let w = sp.create("work");
        assert!(sp.set_default(&w));
        assert!(sp.delete("general"));
        assert_eq!(sp.space_of("a"), w);
    }

    #[test]
    fn reorder_moves_a_space_before_another_or_to_the_end() {
        let mut sp = Spaces::default();
        sp.reconcile(&c(&["a"]), None);
        let x = sp.create("x");
        let y = sp.create("y");
        assert!(sp.reorder(&y, Some("general")));
        assert_eq!(ids(&sp), vec![y.as_str(), "general", x.as_str()]);
        assert!(sp.reorder(&y, None));
        assert_eq!(ids(&sp), vec!["general", x.as_str(), y.as_str()]);
        assert!(!sp.reorder(&y, Some(&y)));
    }

    #[test]
    fn resolve_takes_an_id_or_a_unique_name() {
        let mut sp = Spaces::default();
        sp.reconcile(&c(&["a"]), None);
        let m = sp.create("Meetings");
        assert_eq!(sp.resolve(&m).as_deref(), Some(m.as_str()));
        assert_eq!(sp.resolve("meetings").as_deref(), Some(m.as_str()));
        assert_eq!(sp.resolve("nope"), None);
        let dup = sp.create("meetings");
        assert_ne!(dup, m, "ids stay unique");
        assert_eq!(sp.resolve("MEETINGS"), None, "ambiguous name");
        assert_eq!(
            sp.resolve(&m).as_deref(),
            Some(m.as_str()),
            "an exact id still wins"
        );
    }

    #[test]
    fn dead_circles_are_forgotten() {
        let mut sp = Spaces::default();
        sp.reconcile(&c(&["a", "b"]), None);
        sp.move_circle("b", "general");
        sp.reconcile(&c(&["a"]), None);
        assert!(!sp.of.contains_key("b"));
    }

    #[test]
    fn json_roundtrip_and_empty_blob() {
        let mut sp = Spaces::default();
        sp.reconcile(&c(&["ui-a", "ui-b"]), None);
        let back: Spaces = serde_json::from_str(&serde_json::to_string(&sp).unwrap()).unwrap();
        assert_eq!(back, sp);
        let empty: Spaces = serde_json::from_str("{}").unwrap();
        assert!(empty.is_empty());
    }
}

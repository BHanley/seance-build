//! `ctl space …` / `ctl move`: edits the `spaces` field of the rail blob the
//! GUIs own (`daemon/fsbridge.rs` SubsLoad/SubsSave) and broadcasts it like a
//! GUI save would. Every other key in the blob is carried through untouched.

use serde_json::{json, Value};

use seance_core::protocol::GuiEvent;
use seance_core::spaces::Spaces;

use super::helpers::atomic_write_bytes;
use super::Engine;

impl Engine {
    pub(super) fn space_op(
        &mut self,
        action: &str,
        args: &[String],
        scope: Option<&str>,
    ) -> Result<Value, String> {
        let path = crate::daemon::fsbridge::subs_path();
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
        let mut root: Value = serde_json::from_str(&raw)
            .map_err(|e| format!("rail arrangement unreadable ({}): {e}", path.display()))?;
        if !root.is_object() {
            return Err(format!(
                "rail arrangement is not an object ({})",
                path.display()
            ));
        }
        let mut spaces: Spaces = match root.get("spaces") {
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("spaces: {e}"))?,
            None => Spaces::default(),
        };
        let circles: Vec<(String, String)> = self
            .known_workspace_slugs()
            .into_iter()
            .map(|s| {
                let label = self.workspace_label(&s);
                (s, label)
            })
            .collect();
        let mut changed = spaces.reconcile(&circles, None);
        let space = |spaces: &Spaces, key: &str| {
            spaces
                .resolve(key)
                .ok_or_else(|| format!("no space '{key}' (ambiguous or missing)"))
        };
        let arg = |i: usize, what: &str| {
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("space {action}: expected {what}"))
        };

        let out = match action {
            "list" | "ls" => {
                let ordered: Vec<String> = circles.iter().map(|(s, _)| s.clone()).collect();
                let rows: Vec<Value> = spaces
                    .list
                    .iter()
                    .map(|s| {
                        let members: Vec<Value> = spaces
                            .circles_in(&s.id, &ordered)
                            .into_iter()
                            .map(|c| json!({"workspace": c, "label": self.workspace_label(&c)}))
                            .collect();
                        json!({
                            "id": s.id,
                            "name": s.name,
                            "default": spaces.default_id() == s.id,
                            "circles": members,
                        })
                    })
                    .collect();
                json!({"spaces": rows})
            }
            "new" | "create" => {
                let name = args.join(" ");
                if name.trim().is_empty() {
                    return Err("space new: expected NAME".into());
                }
                let id = spaces.create(&name);
                changed = true;
                json!({"created": id})
            }
            "rename" => {
                let id = space(&spaces, &arg(0, "SPACE NAME")?)?;
                let name = args[1..].join(" ");
                changed |= spaces.rename(&id, &name);
                json!({"space": id, "name": name.trim()})
            }
            "rm" | "delete" => {
                let id = space(&spaces, &arg(0, "SPACE")?)?;
                if !spaces.delete(&id) {
                    return Err(format!("'{id}' is the default space; it can't be deleted"));
                }
                changed = true;
                json!({"deleted": id, "circles_to": spaces.default_id()})
            }
            "default" => {
                let id = space(&spaces, &arg(0, "SPACE")?)?;
                changed |= spaces.set_default(&id);
                json!({"default": id})
            }
            "move" => {
                // `move SPACE` moves the caller's own circle.
                let (circle, target) = match args.len() {
                    0 => return Err("move: expected [CIRCLE] SPACE".into()),
                    1 => (
                        scope
                            .map(str::to_string)
                            .ok_or("move: expected CIRCLE SPACE outside a pane")?,
                        args[0].clone(),
                    ),
                    _ => (args[0].clone(), args[1..].join(" ")),
                };
                let slug = self
                    .resolve_workspace(&circle)
                    .ok_or_else(|| format!("no circle '{circle}'"))?;
                let id = space(&spaces, &target)?;
                changed |= spaces.move_circle(&slug, &id);
                json!({"workspace": slug, "space": id})
            }
            other => {
                return Err(format!(
                    "space: unknown action '{other}' (list|new|rename|rm|default|move)"
                ))
            }
        };

        if changed {
            root["spaces"] = serde_json::to_value(&spaces).map_err(|e| e.to_string())?;
            let body = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
            atomic_write_bytes(&path, body.as_bytes()).map_err(|e| format!("rail save: {e}"))?;
            self.broadcast(GuiEvent::RailPrefs { json: body });
        }
        Ok(out)
    }
}

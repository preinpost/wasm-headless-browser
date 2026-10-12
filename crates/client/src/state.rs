//! What one invocation leaves for the next. Each command is its own
//! process, as an agent runs them, so the target being driven and the refs
//! of the last snapshot live in a small JSON file per session.
//!
//! The directory is `WHB_STATE_DIR`, else `$HOME/.whb`, else `./.whb`. `HOME`
//! is read directly: `dirs`-style lookups give nothing under WASIX, and a
//! sandbox shares files between commands only where it is told to.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct State {
    /// The target (tab) this session drives.
    pub target: Option<String>,
    /// The document the refs were taken from; see `DOCUMENT_ID` in main.
    pub document: Option<String>,
    /// `e3` to the element's `backendDOMNodeId`.
    pub refs: BTreeMap<String, u64>,
    /// The number of the last ref handed out.
    pub last_ref: u64,
}

pub fn directory() -> PathBuf {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if let Some(dir) = set("WHB_STATE_DIR") {
        return dir.into();
    }
    if let Some(home) = set("HOME") {
        return PathBuf::from(home).join(".whb");
    }
    PathBuf::from(".whb")
}

/// A session name is a file name, so it is kept to a safe alphabet.
pub fn check_session(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.');
    if ok {
        Ok(())
    } else {
        Err(format!("session name {name:?}: use letters, digits, '-', '_' and '.' (64 at most)"))
    }
}

fn path(session: &str) -> PathBuf {
    directory().join(format!("{session}.json"))
}

impl State {
    pub fn load(session: &str) -> Result<State, String> {
        let path = path(session);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let value: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(State::from_json(&value))
    }

    pub fn save(&self, session: &str) -> Result<(), String> {
        let path = path(session);
        let dir = directory();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        // Written whole and renamed, so a reader never sees half a file.
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, self.to_json().to_string()).map_err(|e| format!("{}: {e}", partial.display()))?;
        std::fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn remove(session: &str) {
        let _ = std::fs::remove_file(path(session));
    }

    /// Forgets the refs: the page they pointed into is gone.
    pub fn forget_refs(&mut self) {
        self.refs.clear();
        self.last_ref = 0;
        self.document = None;
    }

    fn to_json(&self) -> Value {
        json!({ "target": self.target, "document": self.document, "refs": self.refs, "lastRef": self.last_ref })
    }

    fn from_json(value: &Value) -> State {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
        let refs = value
            .get("refs")
            .and_then(Value::as_object)
            .map(|refs| refs.iter().filter_map(|(k, v)| Some((k.clone(), v.as_u64()?))).collect())
            .unwrap_or_default();
        State {
            target: text("target"),
            document: text("document"),
            refs,
            last_ref: value.get("lastRef").and_then(Value::as_u64).unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_json() {
        let mut state = State { target: Some("T1".into()), document: Some("d".into()), last_ref: 2, ..State::default() };
        state.refs.insert("e1".into(), 7);
        state.refs.insert("e2".into(), 9);
        assert_eq!(State::from_json(&state.to_json()), state);
        assert_eq!(State::from_json(&json!({})), State::default());
    }

    #[test]
    fn session_names_stay_inside_the_directory() {
        assert!(check_session("default").is_ok());
        assert!(check_session("login-2.a_b").is_ok());
        for bad in ["", "../x", "a/b", ".hidden", "a b", &"x".repeat(65)] {
            assert!(check_session(bad).is_err(), "{bad:?}");
        }
    }
}

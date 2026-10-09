//! Credentials, held in memory and kept from the client.
//!
//! The client never needs a password: it types a placeholder,
//! `{{secret:NAME}}`, and the engine puts the value in at the last moment,
//! inside `Input.insertText`, and only into a page from the origin the secret
//! is bound to. Everything the engine sends back over CDP has the value
//! replaced by the placeholder again, so a client that reads the field, or a
//! snapshot that shows a username, gets the placeholder.
//!
//! Values arrive on stdin (see the `browser` binary), never on the command
//! line or in the environment, live only in this process's memory, and are
//! overwritten when it drops them.
//!
//! What this does not do: stop a hostile client. One that runs page script
//! can still leak a value a character at a time (`value.charCodeAt(0)`).
//! The guarantee is that the value does not show up in a client's
//! transcript, logs or snapshots by accident. The page itself reads the
//! value, as it must to log in, and copies it into its own JavaScript
//! strings, which cannot be wiped.

use serde_json::Value;

/// One named credential and the origin allowed to receive it.
pub struct Secret {
    name: String,
    origin: String,
    value: String,
}

impl Drop for Secret {
    /// Overwrites the value, so it does not outlive its use in freed memory.
    fn drop(&mut self) {
        // SAFETY: writing zero bytes keeps the string valid UTF-8, and the
        // volatile writes keep the compiler from eliding them.
        unsafe {
            for byte in self.value.as_mut_vec().iter_mut() {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret {{ name: {:?}, origin: {:?}, value: <{} bytes> }}", self.name, self.origin, self.value.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// `{{secret:NAME}}` names no secret the engine was given.
    Unknown(String),
    /// The secret exists but is bound to another origin.
    WrongOrigin { name: String, allowed: String, page: String },
    /// A `--secret` specification that is not `NAME@ORIGIN`.
    BadSpec(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Unknown(name) => write!(f, "no secret named '{name}' was given to the engine"),
            SecretError::WrongOrigin { name, allowed, page } => {
                write!(f, "secret '{name}' is for {allowed}, and this page is {page}; it was not typed")
            }
            SecretError::BadSpec(spec) => {
                write!(f, "'{spec}' is not NAME@ORIGIN, for example gw@https://gw.example.com")
            }
        }
    }
}

impl std::error::Error for SecretError {}

/// The engine's credentials.
#[derive(Debug, Default)]
pub struct Secrets {
    secrets: Vec<Secret>,
}

impl Secrets {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Adds a secret from a `NAME@ORIGIN` specification and its value. The
    /// origin is normalised (`https://Host:443/x` becomes `https://host`).
    pub fn add(&mut self, spec: &str, value: String) -> Result<(), SecretError> {
        let bad = || SecretError::BadSpec(spec.to_string());
        let (name, origin) = spec.split_once('@').ok_or_else(bad)?;
        let valid_name = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
        if !valid_name {
            return Err(bad());
        }
        let origin = url::Url::parse(origin).map_err(|_| bad())?.origin();
        if !origin.is_tuple() {
            return Err(bad());
        }
        self.secrets.retain(|s| s.name != name);
        self.secrets.push(Secret { name: name.to_string(), origin: origin.ascii_serialization(), value });
        Ok(())
    }

    /// The names and origins given, without values, for a startup banner.
    pub fn describe(&self) -> Vec<String> {
        self.secrets.iter().map(|s| format!("{} for {}", s.name, s.origin)).collect()
    }

    /// `text` with every `{{secret:NAME}}` replaced by its value, when the
    /// page at `page_url` is from the secret's origin. Text without
    /// placeholders comes back as it was.
    pub fn resolve(&self, text: &str, page_url: &str) -> Result<String, SecretError> {
        if !text.contains("{{secret:") {
            return Ok(text.to_string());
        }
        let page = url::Url::parse(page_url).map(|u| u.origin().ascii_serialization()).unwrap_or_else(|_| "null".into());
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("{{secret:") {
            out.push_str(&rest[..start]);
            let after = &rest[start + "{{secret:".len()..];
            let Some(end) = after.find("}}") else {
                out.push_str(&rest[start..]);
                return Ok(out);
            };
            let name = &after[..end];
            let secret = self.secrets.iter().find(|s| s.name == name).ok_or_else(|| SecretError::Unknown(name.to_string()))?;
            if secret.origin != page {
                return Err(SecretError::WrongOrigin { name: name.to_string(), allowed: secret.origin.clone(), page });
            }
            out.push_str(&secret.value);
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// Replaces every value inside `text` with its placeholder.
    pub fn scrub_str(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            if !secret.value.is_empty() && out.contains(&secret.value) {
                out = out.replace(&secret.value, &format!("{{{{secret:{}}}}}", secret.name));
            }
        }
        out
    }

    /// Replaces every value in every string of a CDP message, keys included,
    /// with its placeholder.
    pub fn scrub(&self, value: &mut Value) {
        if self.secrets.is_empty() {
            return;
        }
        match value {
            Value::String(s) => {
                if self.secrets.iter().any(|secret| !secret.value.is_empty() && s.contains(&secret.value)) {
                    *s = self.scrub_str(s);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.scrub(v)),
            Value::Object(map) => {
                let leaking: Vec<String> =
                    map.keys().filter(|k| self.secrets.iter().any(|s| !s.value.is_empty() && k.contains(&s.value))).cloned().collect();
                for key in leaking {
                    if let Some(v) = map.remove(&key) {
                        map.insert(self.scrub_str(&key), v);
                    }
                }
                map.values_mut().for_each(|v| self.scrub(v));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn secrets() -> Secrets {
        let mut s = Secrets::new();
        s.add("gw", "hunter2".into()).unwrap_err();
        s.add("gw@https://GW.example.com:443/login", "hunter2".into()).unwrap();
        s.add("user@https://gw.example.com", "kim".into()).unwrap();
        s
    }

    #[test]
    fn placeholders_resolve_only_for_their_origin() {
        let s = secrets();
        assert_eq!(s.resolve("{{secret:gw}}", "https://gw.example.com/#/login").unwrap(), "hunter2");
        assert_eq!(s.resolve("a{{secret:user}}b", "https://gw.example.com/").unwrap(), "akimb");
        assert_eq!(s.resolve("plain", "https://anything.test/").unwrap(), "plain");
        let wrong = s.resolve("{{secret:gw}}", "https://evil.test/").unwrap_err();
        assert!(matches!(wrong, SecretError::WrongOrigin { .. }));
        assert!(!wrong.to_string().contains("hunter2"));
        assert_eq!(s.resolve("{{secret:nope}}", "https://gw.example.com/").unwrap_err(), SecretError::Unknown("nope".into()));
        // http and https are different origins.
        assert!(s.resolve("{{secret:gw}}", "http://gw.example.com/").is_err());
    }

    #[test]
    fn outgoing_messages_carry_placeholders_not_values() {
        let s = secrets();
        let mut message = json!({
            "result": { "type": "string", "value": "hunter2" },
            "nodes": [{ "name": { "value": "logged in as kim" } }],
            "hunter2": 1,
        });
        s.scrub(&mut message);
        let text = message.to_string();
        assert!(!text.contains("hunter2") && !text.contains("as kim"), "{text}");
        assert_eq!(message["result"]["value"], "{{secret:gw}}");
        assert_eq!(message["nodes"][0]["name"]["value"], "logged in as {{secret:user}}");
    }

    #[test]
    fn values_never_show_in_debug_output() {
        let s = secrets();
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(s.describe(), ["gw for https://gw.example.com", "user for https://gw.example.com"]);
    }

    #[test]
    fn bad_specifications_are_refused() {
        let mut s = Secrets::new();
        for spec in ["noorigin", "@https://x.test", "a b@https://x.test", "x@not a url", "x@data:text/plain,1"] {
            assert!(s.add(spec, "v".into()).is_err(), "{spec}");
        }
    }
}

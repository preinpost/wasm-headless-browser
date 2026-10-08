//! A rule set, not an interpreter.
//!
//! M0 recognises the expressions a client actually sends and answers them from
//! the fixed page model. Anything unrecognised comes back as `undefined`, which
//! is honest: this engine cannot run JavaScript yet.
//!
//! Every rule below was observed in `docs/capture/frames.jsonl`. When the real
//! engine arrives, this module is deleted rather than extended.

use serde_json::{json, Value};

use crate::dom::Document;

pub enum Outcome {
    Value(Value),
    Object { class: &'static str, description: String, dom: usize },
    Undefined,
}

pub fn evaluate(expr: &str, doc: &mut Document) -> Outcome {
    let e = expr.trim().trim_end_matches(';').trim();

    // Literals and probes.
    match e {
        "" => return Outcome::Undefined,
        "1" => return Outcome::Value(json!({ "type": "number", "value": 1.0, "description": "1" })),
        "1+1" => {
            return Outcome::Value(json!({ "type": "number", "value": 2.0, "description": "2" }))
        }
        "location.href" => return string(doc.url.clone()),
        "document.URL" => return string(doc.url.clone()),
        "document.title" => return string(doc.title.clone()),
        "document.body" => return object(doc.body, "HTMLBodyElement", "body"),
        "document.documentElement" => return object(doc.root, "HTMLHtmlElement", "html"),
        "window.innerWidth" => {
            return Outcome::Value(json!({ "type": "number", "value": doc.viewport.0 }))
        }
        "window.innerHeight" => {
            return Outcome::Value(json!({ "type": "number", "value": doc.viewport.1 }))
        }
        _ => {}
    }

    // Element lookups.
    if e.starts_with("document.querySelector(") || e.starts_with("document.body.querySelector(") {
        return match literal(e).and_then(|sel| doc.query(&sel)) {
            Some(index) => {
                let tag = doc.nodes[index].tag;
                object(index, class_for(tag), tag)
            }
            None => null_value(),
        };
    }

    if e.starts_with("document.querySelectorAll(") || e.starts_with("document.body.querySelectorAll(")
    {
        // Clients that need the members run their own traversal, so an opaque
        // array-shaped object is enough to keep the expression from failing.
        return Outcome::Object {
            class: "NodeList",
            description: "NodeList".into(),
            dom: usize::MAX,
        };
    }

    // Scrolling and history are side effects; the answers are all undefined.
    if e.starts_with("window.scrollBy") || e.starts_with("window.scrollTo") {
        return Outcome::Undefined;
    }
    if e.starts_with("history.back") {
        doc.go_back();
        return Outcome::Undefined;
    }
    if e.starts_with("history.forward") {
        doc.go_forward();
        return Outcome::Undefined;
    }

    // agent-browser resolves a click point through a large IIFE that computes
    // the centre and checks for an overlapping element. The marker is the
    // `blocker` key it returns.
    if e.contains("blocker") && e.contains("getBoundingClientRect") {
        let (x, y) = literal(e)
            .and_then(|sel| doc.query(&sel))
            .map(|index| {
                let r = doc.nodes[index].rect;
                (r.x + r.w / 2.0, r.y + r.h / 2.0)
            })
            .unwrap_or((0.0, 0.0));
        return Outcome::Value(json!({
            "type": "object",
            "value": { "x": x, "y": y, "blocker": Value::Null },
        }));
    }

    // Its companion scan for clickable candidates.
    if e.contains("interactiveRoles") {
        return Outcome::Value(json!({ "type": "object", "subtype": "array", "value": [] }));
    }

    Outcome::Undefined
}

fn string(s: String) -> Outcome {
    Outcome::Value(json!({ "type": "string", "value": s }))
}

fn null_value() -> Outcome {
    Outcome::Value(json!({ "type": "object", "subtype": "null", "value": Value::Null }))
}

fn object(index: usize, class: &'static str, description: &str) -> Outcome {
    Outcome::Object { class, description: description.to_string(), dom: index }
}

/// Pulls the first quoted literal out of an expression.
fn literal(e: &str) -> Option<String> {
    let bytes = e.as_bytes();
    let open = bytes.iter().position(|b| *b == b'"' || *b == b'\'')?;
    let quote = bytes[open];
    let rest = &bytes[open + 1..];
    let close = rest.iter().position(|b| *b == quote)?;
    String::from_utf8(rest[..close].to_vec()).ok()
}

fn class_for(tag: &str) -> &'static str {
    match tag {
        "html" => "HTMLHtmlElement",
        "body" => "HTMLBodyElement",
        "a" => "HTMLAnchorElement",
        "button" => "HTMLButtonElement",
        "input" => "HTMLInputElement",
        "form" => "HTMLFormElement",
        "label" => "HTMLLabelElement",
        "p" => "HTMLParagraphElement",
        "h1" => "HTMLHeadingElement",
        _ => "HTMLElement",
    }
}

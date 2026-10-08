//! Accessibility tree projection.
//!
//! This is what `agent-browser snapshot` actually renders, so it is the single
//! most valuable thing a headless engine can expose. Chrome computes roles and
//! names through its own algorithm; we derive them straight from the tag and
//! the associated label, which is enough for a client to identify elements.

use serde_json::{json, Value};

use crate::dom::{Document, Node};

pub fn build(doc: &Document) -> Value {
    let mut ids: Vec<Option<String>> = vec![None; doc.nodes.len()];
    let mut counter = 1u64;
    assign(doc, doc.root, &mut ids, &mut counter);

    let mut nodes = Vec::new();
    emit(doc, doc.root, &ids, &mut nodes);

    json!({ "nodes": nodes })
}

fn include(node: &Node) -> bool {
    !matches!(node.tag, "head" | "title")
}

fn assign(doc: &Document, index: usize, ids: &mut [Option<String>], counter: &mut u64) {
    if !include(&doc.nodes[index]) {
        return;
    }
    ids[index] = Some(counter.to_string());
    *counter += 1;
    for child in doc.nodes[index].children.clone() {
        assign(doc, child, ids, counter);
    }
}

fn emit(doc: &Document, index: usize, ids: &[Option<String>], out: &mut Vec<Value>) {
    let Some(node_id) = ids[index].clone() else {
        return;
    };

    let node = &doc.nodes[index];
    let child_ids: Vec<String> =
        node.children.iter().filter_map(|c| ids[*c].clone()).collect();

    let role = role_of(node);
    let name = name_of(doc, index, role);

    let mut entry = json!({
        "nodeId": node_id,
        "ignored": false,
        "role": { "type": "role", "value": role },
        "backendDOMNodeId": index + 1,
    });

    if let Some(parent) = node.parent.and_then(|p| ids[p].clone()) {
        entry["parentId"] = json!(parent);
    }
    if !child_ids.is_empty() {
        entry["childIds"] = json!(child_ids);
    }
    if !name.is_empty() {
        entry["name"] = json!({ "type": "computedString", "value": name });
    }
    if let Some(level) = heading_level(node) {
        entry["properties"] = json!([
            { "name": "level", "value": { "type": "integer", "value": level } }
        ]);
    }
    if role == "textbox" {
        let value = node.attr("value").unwrap_or("");
        entry["value"] = json!({ "type": "string", "value": value });
    }

    out.push(entry);

    for child in node.children.clone() {
        emit(doc, child, ids, out);
    }
}

fn role_of(node: &Node) -> &'static str {
    if node.text_node {
        return "StaticText";
    }
    match node.tag {
        "html" => "RootWebArea",
        "body" | "div" | "span" => "generic",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "p" => "paragraph",
        "form" => "form",
        "label" => "LabelText",
        "button" => "button",
        "a" => {
            if node.attr("href").is_some() {
                "link"
            } else {
                "generic"
            }
        }
        "input" => match node.attr("type").unwrap_or("text") {
            "password" | "text" | "email" | "search" => "textbox",
            "checkbox" => "checkbox",
            "radio" => "radio",
            "submit" | "button" | "reset" => "button",
            _ => "textbox",
        },
        "textarea" => "textbox",
        "select" => "combobox",
        _ => "generic",
    }
}

fn name_of(doc: &Document, index: usize, role: &str) -> String {
    let node = &doc.nodes[index];
    match role {
        "RootWebArea" => doc.title.clone(),
        "StaticText" => node.text.clone().unwrap_or_default(),
        "heading" | "paragraph" | "link" | "button" => doc.text_of(index),
        "textbox" => {
            // Prefer the label that points at this control, then the
            // placeholder, then the id.
            if let Some(id) = node.id {
                for (i, other) in doc.nodes.iter().enumerate() {
                    if other.tag == "label" && other.attr("for") == Some(id) {
                        let text = doc.text_of(i);
                        if !text.is_empty() {
                            return text;
                        }
                    }
                }
            }
            if let Some(placeholder) = node.attr("placeholder") {
                if !placeholder.is_empty() {
                    return placeholder.to_string();
                }
            }
            node.id.unwrap_or("").to_string()
        }
        _ => String::new(),
    }
}

fn heading_level(node: &Node) -> Option<u8> {
    match node.tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

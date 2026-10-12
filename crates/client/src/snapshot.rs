//! The accessibility tree as text, in the shape agent-browser prints, so
//! prompts and scripts written for one read the other:
//!
//! ```text
//! - form
//!   - heading "Sign in" [level=1, ref=e1]
//!   - textbox "User" [ref=e2]: kim
//!   - button "Sign in" [ref=e4]
//! ```
//!
//! Interactive roles always get a ref, content roles (headings, cells, list
//! items, landmarks) when they have a name. Ignored nodes are skipped and
//! their children take their place, as the root web area's do.

use std::collections::HashMap;

use serde_json::Value;

const INTERACTIVE: &[&str] = &[
    "button",
    "link",
    "textbox",
    "checkbox",
    "radio",
    "combobox",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "treeitem",
    "Iframe",
];

const CONTENT: &[&str] = &[
    "heading",
    "cell",
    "gridcell",
    "columnheader",
    "rowheader",
    "listitem",
    "article",
    "region",
    "main",
    "navigation",
];

/// Roots whose children are the page; they are not printed themselves.
const ROOTS: &[&str] = &["RootWebArea", "WebArea"];

fn key(id: &Value) -> String {
    id.as_str().map(str::to_string).unwrap_or_else(|| id.to_string())
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Whitespace runs as single spaces, as names are shown.
fn tidy(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn property<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    node.get("properties")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(Value::as_str) == Some(name))?
        .get("value")?
        .get("value")
}

/// Renders `Accessibility.getFullAXTree`'s result. `assign` names the ref
/// for an element's backend node id.
pub fn render(tree: &Value, interactive_only: bool, assign: &mut dyn FnMut(u64) -> String) -> String {
    let nodes: Vec<&Value> = tree.get("nodes").and_then(Value::as_array).map(|n| n.iter().collect()).unwrap_or_default();
    let by_id: HashMap<String, &Value> = nodes.iter().filter_map(|n| Some((key(n.get("nodeId")?), *n))).collect();
    let roots: Vec<&Value> = nodes
        .iter()
        .copied()
        .filter(|n| n.get("parentId").map(|p| !by_id.contains_key(&key(p))).unwrap_or(true))
        .collect();
    let mut lines = Vec::new();
    for root in roots {
        walk(root, 0, "", &by_id, interactive_only, assign, &mut lines);
    }
    lines.join("\n")
}

#[allow(clippy::too_many_arguments)]
fn walk(
    node: &Value,
    depth: usize,
    parent_name: &str,
    by_id: &HashMap<String, &Value>,
    interactive_only: bool,
    assign: &mut dyn FnMut(u64) -> String,
    lines: &mut Vec<String>,
) {
    let role = node.get("role").and_then(|r| r.get("value")).map(text).unwrap_or_default();
    let name = tidy(&node.get("name").and_then(|n| n.get("value")).map(text).unwrap_or_default());
    let ignored = node.get("ignored").and_then(Value::as_bool).unwrap_or(false);
    // A button's or heading's own text says nothing its name did not.
    let repeated = role == "StaticText" && !parent_name.is_empty() && name == parent_name;
    let silent = ignored
        || repeated
        || ROOTS.contains(&role.as_str())
        || role.is_empty()
        || (role == "StaticText" && name.is_empty());

    let mut child_depth = depth;
    // Ignored and root nodes pass their parent's name on to their children.
    let mut child_parent = parent_name.to_string();
    if !silent {
        let wants_ref = INTERACTIVE.contains(&role.as_str()) || (CONTENT.contains(&role.as_str()) && !name.is_empty());
        let reference = wants_ref
            .then(|| node.get("backendDOMNodeId").and_then(Value::as_u64))
            .flatten()
            .map(|backend| assign(backend));
        if !interactive_only || reference.is_some() {
            lines.push(line(node, &role, &name, reference.as_deref(), if interactive_only { 0 } else { depth }));
        }
        child_depth = depth + 1;
        child_parent = name.clone();
    }
    for child in node.get("childIds").and_then(Value::as_array).into_iter().flatten() {
        if let Some(child) = by_id.get(&key(child)) {
            walk(child, child_depth, &child_parent, by_id, interactive_only, assign, lines);
        }
    }
}

fn line(node: &Value, role: &str, name: &str, reference: Option<&str>, depth: usize) -> String {
    let mut out = format!("{}- {role}", "  ".repeat(depth));
    if !name.is_empty() {
        out.push(' ');
        out.push_str(&serde_json::to_string(name).unwrap_or_default());
    }
    let mut attrs = Vec::new();
    if let Some(level) = property(node, "level") {
        attrs.push(format!("level={}", text(level)));
    }
    if let Some(checked) = property(node, "checked") {
        attrs.push(format!("checked={}", text(checked)));
    }
    if let Some(expanded) = property(node, "expanded") {
        attrs.push(format!("expanded={}", text(expanded)));
    }
    for flag in ["selected", "disabled", "required"] {
        if property(node, flag).and_then(Value::as_bool) == Some(true) {
            attrs.push(flag.to_string());
        }
    }
    if let Some(reference) = reference {
        attrs.push(format!("ref={reference}"));
    }
    if !attrs.is_empty() {
        out.push_str(&format!(" [{}]", attrs.join(", ")));
    }
    let value = node.get("value").and_then(|v| v.get("value")).map(text).unwrap_or_default();
    if !value.is_empty() && value != name {
        out.push_str(&format!(": {value}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The engine's shape for the login fixture, filled in.
    fn tree() -> Value {
        let node = |id: u64, parent: Option<u64>, role: &str, name: &str, backend: u64| {
            let mut n = json!({ "nodeId": id, "ignored": false, "role": { "value": role }, "backendDOMNodeId": backend });
            if let Some(p) = parent {
                n["parentId"] = json!(p);
            }
            if !name.is_empty() {
                n["name"] = json!({ "value": name });
            }
            n
        };
        let mut nodes = vec![
            node(1, None, "RootWebArea", "Fixture Login", 1),
            node(2, Some(1), "generic", "", 2),
            node(3, Some(2), "form", "", 3),
            node(4, Some(3), "heading", "Sign in", 4),
            node(5, Some(3), "LabelText", "", 5),
            node(6, Some(5), "StaticText", "User", 6),
            node(7, Some(3), "textbox", "User", 7),
            node(8, Some(3), "button", "Sign in", 8),
            node(9, Some(3), "paragraph", "", 9),
            node(10, Some(3), "checkbox", "Remember   me", 10),
            node(11, Some(4), "StaticText", "Sign in", 11),
            node(12, Some(9), "StaticText", "Sign in", 12),
        ];
        nodes[1]["ignored"] = json!(true);
        nodes[3]["properties"] = json!([{ "name": "level", "value": { "value": 1 } }]);
        nodes[6]["value"] = json!({ "value": "kim" });
        nodes[7]["properties"] = json!([{ "name": "disabled", "value": { "value": true } }]);
        nodes[9]["properties"] = json!([{ "name": "checked", "value": { "value": "true" } }]);
        let children = [(1, vec![2]), (2, vec![3]), (3, vec![4, 5, 7, 8, 9, 10]), (4, vec![11]), (5, vec![6]), (9, vec![12])];
        for (parent, kids) in children {
            nodes[parent - 1]["childIds"] = json!(kids);
        }
        json!({ "nodes": nodes })
    }

    fn numbered() -> impl FnMut(u64) -> String {
        let mut next = 0;
        move |_| {
            next += 1;
            format!("e{next}")
        }
    }

    #[test]
    fn the_tree_prints_as_agent_browser_prints_it() {
        let mut assign = numbered();
        let text = render(&tree(), false, &mut assign);
        assert_eq!(
            text,
            "- form\n  \
               - heading \"Sign in\" [level=1, ref=e1]\n  \
               - LabelText\n    \
                 - StaticText \"User\"\n  \
               - textbox \"User\" [ref=e2]: kim\n  \
               - button \"Sign in\" [disabled, ref=e3]\n  \
               - paragraph\n    \
                 - StaticText \"Sign in\"\n  \
               - checkbox \"Remember me\" [checked=true, ref=e4]"
        );
    }

    #[test]
    fn interactive_only_is_the_flat_list_of_refs() {
        let mut assign = numbered();
        let text = render(&tree(), true, &mut assign);
        assert_eq!(
            text,
            "- heading \"Sign in\" [level=1, ref=e1]\n\
             - textbox \"User\" [ref=e2]: kim\n\
             - button \"Sign in\" [disabled, ref=e3]\n\
             - checkbox \"Remember me\" [checked=true, ref=e4]"
        );
    }

    #[test]
    fn refs_go_to_the_backend_nodes_they_name() {
        let mut seen = Vec::new();
        let mut assign = |backend: u64| {
            seen.push(backend);
            format!("e{backend}")
        };
        render(&tree(), true, &mut assign);
        assert_eq!(seen, vec![4, 7, 8, 10]);
    }

    #[test]
    fn names_are_quoted_as_json() {
        let mut assign = numbered();
        let tree = json!({ "nodes": [
            { "nodeId": "a", "role": { "value": "link" }, "name": { "value": "say \"hi\"\n" }, "backendDOMNodeId": 3 }
        ]});
        assert_eq!(render(&tree, false, &mut assign), "- link \"say \\\"hi\\\"\" [ref=e1]");
    }
}

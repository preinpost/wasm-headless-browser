//! The accessibility tree.
//!
//! This is the highest-value projection the engine produces. `agent-browser`
//! renders `snapshot` from it and resolves `@eN` refs against it, so the whole
//! agent workflow rests on roles and names being right.
//!
//! It is a projection, not a copy: the tree is recomputed from the DOM on
//! demand and node ids are assigned per build. Chrome's accessibility
//! implementation is far more thorough, but a client needs roles, names,
//! parentage and a backend node id, and that is what this produces.

use serde_json::{json, Value};

use crate::dom::{Document, NodeId};
use crate::style::Styles;

#[derive(Debug, Clone)]
pub struct AxNode {
    pub id: String,
    /// Computed role, e.g. `heading` or `textbox`.
    pub role: &'static str,
    /// Accessible name. Empty when the element has none.
    pub name: String,
    /// The DOM node this came from, offset by one so ids are never zero.
    pub backend_dom_node_id: usize,
    pub parent: Option<String>,
    pub children: Vec<String>,
    /// Heading level, when the role is `heading`.
    pub level: Option<u8>,
    /// Current value, for controls that carry one.
    pub value: Option<String>,
}

/// Builds the tree in document order.
pub fn build(doc: &Document) -> Vec<AxNode> {
    let mut nodes = Vec::new();
    let mut counter = 0usize;
    let mut roots = Vec::new();
    let document = doc.document_id();
    let styles = crate::style::document_styles(doc);
    for child in doc.children(document).to_vec() {
        collect(doc, &styles, child, None, &mut counter, &mut nodes, &mut roots);
    }
    nodes
}

/// True when the node and its whole subtree should stay out of the tree.
///
/// Chrome marks such nodes ignored but still emits them, so a client can see
/// the shape. Dropping the subtree keeps the tree connected and is enough for
/// every client observed so far.
fn is_excluded(doc: &Document, id: NodeId) -> bool {
    // Never in the accessibility tree.
    const NEVER: &[&str] = &[
        "script",
        "style",
        "head",
        "title",
        "meta",
        "link",
        "base",
        "noscript",
        "template",
        "param",
        "source",
        "track",
    ];
    if let Some(tag) = doc.tag(id) {
        if NEVER.contains(&tag) {
            return true;
        }
    }
    if doc.has_attr(id, "hidden") {
        return true;
    }
    if doc.attr(id, "aria-hidden") == Some("true") {
        return true;
    }
    if doc.is_tag(id, "input") && doc.attr(id, "type") == Some("hidden") {
        return true;
    }
    false
}

/// Walks one node, appending it and its subtree to `out` in document order.
///
/// `siblings` receives the id of anything this call produced at the caller's
/// level. Pass-through nodes such as comments use it to hand their children up
/// without a node of their own.
fn collect(
    doc: &Document,
    styles: &Styles,
    id: NodeId,
    parent: Option<String>,
    counter: &mut usize,
    out: &mut Vec<AxNode>,
    siblings: &mut Vec<String>,
) {
    if is_excluded(doc, id) {
        return;
    }
    let style = styles.get(id);
    // Not rendered: the node and everything under it. An element outside
    // the computed tree (inside a template, say) has no style and is not
    // rendered either.
    if doc.is_element(id) && style.is_none_or(|s| s.get("display") == "none") {
        return;
    }
    // Invisible but still laid out: no node of its own, but a descendant may
    // be visible again, so its children are still walked.
    if style.is_some_and(|s| matches!(s.get("visibility"), "hidden" | "collapse")) {
        for child in doc.children(id).to_vec() {
            collect(doc, styles, child, parent.clone(), counter, out, siblings);
        }
        return;
    }

    let is_root = doc.is_tag(id, "html");

    if doc.is_text(id) {
        // Text is as visible as the element it is in.
        let hidden = doc
            .parent(id)
            .and_then(|p| styles.get(p))
            .is_some_and(|s| matches!(s.get("visibility"), "hidden" | "collapse"));
        if hidden {
            return;
        }
        let text = collapse(&doc.node(id).text);
        if text.is_empty() {
            return;
        }
        *counter += 1;
        let node_id = counter.to_string();
        out.push(AxNode {
            id: node_id.clone(),
            role: "StaticText",
            name: text,
            backend_dom_node_id: id + 1,
            parent,
            children: Vec::new(),
            level: None,
            value: None,
        });
        siblings.push(node_id);
        return;
    }

    if doc.is_element(id) {
        *counter += 1;
        let node_id = counter.to_string();
        let (role, name) = if is_root {
            ("RootWebArea", doc.title().unwrap_or_default())
        } else {
            let role = role_of(doc, id);
            (role, accessible_name(doc, id, role))
        };

        // Reserve the slot before recursing so the node precedes its children,
        // which is the order Chrome emits and the order clients expect.
        let slot = out.len();
        out.push(AxNode {
            id: node_id.clone(),
            role,
            name,
            backend_dom_node_id: id + 1,
            parent,
            children: Vec::new(),
            level: if is_root { None } else { heading_level(doc, id) },
            value: if is_root { None } else { control_value(doc, id) },
        });

        let mut children = Vec::new();
        for child in doc.children(id).to_vec() {
            collect(doc, styles, child, Some(node_id.clone()), counter, out, &mut children);
        }
        out[slot].children = children;
        siblings.push(node_id);
        return;
    }

    // Comments, doctypes and anything else non-rendered: no node of their own,
    // but their children stay in the tree at the caller's level.
    for child in doc.children(id).to_vec() {
        collect(doc, styles, child, parent.clone(), counter, out, siblings);
    }
}

/// Collapses whitespace the way accessible names are computed.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first token of an explicit `role` attribute, if it maps to a known role.
fn explicit_role(doc: &Document, id: NodeId) -> Option<&'static str> {
    let raw = doc.attr(id, "role")?;
    let first = raw.split_whitespace().next()?.to_ascii_lowercase();
    Some(match first.as_str() {
        "alert" => "alert",
        "article" => "article",
        "banner" => "banner",
        "button" => "button",
        "cell" => "cell",
        "checkbox" => "checkbox",
        "columnheader" => "columnheader",
        "combobox" => "combobox",
        "complementary" => "complementary",
        "contentinfo" => "contentinfo",
        "dialog" => "dialog",
        "figure" => "figure",
        "form" => "form",
        "heading" => "heading",
        "img" => "img",
        "link" => "link",
        "list" => "list",
        "listbox" => "listbox",
        "listitem" => "listitem",
        "main" => "main",
        "menu" => "menu",
        "menubar" => "menubar",
        "menuitem" => "menuitem",
        "menuitemcheckbox" => "menuitemcheckbox",
        "menuitemradio" => "menuitemradio",
        "navigation" => "navigation",
        "option" => "option",
        "presentation" | "none" => "generic",
        "progressbar" => "progressbar",
        "radio" => "radio",
        "region" => "region",
        "row" => "row",
        "rowheader" => "rowheader",
        "search" => "search",
        "searchbox" => "searchbox",
        "separator" => "separator",
        "slider" => "slider",
        "spinbutton" => "spinbutton",
        "status" => "status",
        "switch" => "switch",
        "tab" => "tab",
        "table" => "table",
        "tablist" => "tablist",
        "tabpanel" => "tabpanel",
        "term" => "term",
        "textbox" => "textbox",
        "toolbar" => "toolbar",
        "tooltip" => "tooltip",
        "tree" => "tree",
        "treeitem" => "treeitem",
        _ => return None,
    })
}

/// The implicit role of an element, expressed as the ARIA role name.
pub fn role_of(doc: &Document, id: NodeId) -> &'static str {
    if let Some(role) = explicit_role(doc, id) {
        return role;
    }
    let Some(tag) = doc.tag(id) else {
        return "generic";
    };

    match tag {
        "a" | "area" => {
            if doc.has_attr(id, "href") {
                "link"
            } else {
                "generic"
            }
        }
        "address" => "group",
        "article" => "article",
        "aside" => "complementary",
        "b" | "strong" => "strong",
        "blockquote" => "blockquote",
        "body" | "div" | "span" => "generic",
        "button" => "button",
        "caption" => "caption",
        "code" => "code",
        "dd" => "definition",
        "del" | "s" => "deletion",
        "details" => "group",
        "dfn" => "term",
        "dialog" => "dialog",
        "dl" => "list",
        "dt" => "term",
        "em" => "emphasis",
        "fieldset" => "group",
        "figcaption" => "caption",
        "figure" => "figure",
        "footer" => "contentinfo",
        "form" => "form",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "header" => "banner",
        "hr" => "separator",
        "i" => "emphasis",
        "img" => {
            // An image with no accessible name and no alt is presentational.
            if doc.attr(id, "alt") == Some("") && doc.attr(id, "role").is_none() {
                "generic"
            } else {
                "img"
            }
        }
        "input" => match doc.attr(id, "type").unwrap_or("text") {
            "button" | "submit" | "reset" | "image" => "button",
            "checkbox" => "checkbox",
            "radio" => "radio",
            "range" => "slider",
            "number" => "spinbutton",
            "search" => "searchbox",
            "email" | "tel" | "text" | "url" => "textbox",
            // A password field has no readable value but is still a textbox.
            "password" => "textbox",
            _ => "textbox",
        },
        "ins" => "insertion",
        "label" => "LabelText",
        "legend" => "legend",
        "li" => "listitem",
        "main" => "main",
        "mark" => "mark",
        "menu" => {
            if doc.attr(id, "type").is_some_and(|t| t.eq_ignore_ascii_case("toolbar")) {
                "toolbar"
            } else {
                "list"
            }
        }
        "meter" => "meter",
        "nav" => "navigation",
        "ol" | "ul" => "list",
        "option" => "option",
        "optgroup" => "group",
        "output" => "status",
        "p" => "paragraph",
        "pre" => "generic",
        "progress" => "progressbar",
        "section" => "region",
        "select" => {
            if doc.has_attr(id, "multiple") || doc.attr(id, "size").is_some_and(|s| s.parse::<u32>().map(|n| n > 1).unwrap_or(false))
            {
                "listbox"
            } else {
                "combobox"
            }
        }
        "sub" => "subscript",
        "summary" => "button",
        "sup" => "superscript",
        "table" => "table",
        "tbody" | "thead" | "tfoot" => "rowgroup",
        "td" => "cell",
        "textarea" => "textbox",
        "th" => match doc.attr(id, "scope") {
            Some("row") => "rowheader",
            Some("col") | Some("colgroup") => "columnheader",
            _ => "cell",
        },
        "time" => "time",
        "tr" => "row",
        "video" | "audio" => "generic",
        _ => "generic",
    }
}

fn heading_level(doc: &Document, id: NodeId) -> Option<u8> {
    if let Some(tag) = doc.tag(id) {
        if let Some(digit) = tag.strip_prefix('h').and_then(|d| d.parse::<u8>().ok()) {
            if (1..=6).contains(&digit) {
                return Some(digit);
            }
        }
    }
    if role_of(doc, id) == "heading" {
        if let Some(level) = doc.attr(id, "aria-level").and_then(|v| v.parse::<u8>().ok()) {
            return Some(level);
        }
        return Some(2);
    }
    None
}

fn control_value(doc: &Document, id: NodeId) -> Option<String> {
    match role_of(doc, id) {
        "textbox" | "searchbox" | "combobox" | "slider" | "spinbutton" | "listbox" => {
            // A password field's value must never surface here.
            if doc.attr(id, "type") == Some("password") {
                return Some(String::new());
            }
            Some(doc.current_value(id))
        }
        "checkbox" | "radio" | "switch" => {
            if doc.attr(id, "aria-checked") == Some("mixed") {
                return Some("mixed".to_string());
            }
            let checked = (doc.is_tag(id, "input") && doc.current_checked(id)) || doc.attr(id, "aria-checked") == Some("true");
            Some(if checked { "true" } else { "false" }.to_string())
        }
        _ => None,
    }
}

/// The accessible name, following the parts of the specification that matter in
/// practice: `aria-labelledby`, then `aria-label`, then tag-specific sources,
/// then an associated label, then the element's own text.
pub fn accessible_name(doc: &Document, id: NodeId, role: &str) -> String {
    if let Some(ids) = doc.attr(id, "aria-labelledby") {
        let mut parts = Vec::new();
        for reference in ids.split_whitespace() {
            if let Some(target) = doc.find_id(doc.document_id(), reference) {
                let text = collapse(&doc.text_content(target));
                if !text.is_empty() {
                    parts.push(text);
                }
            }
        }
        if !parts.is_empty() {
            return parts.join(" ");
        }
    }

    if let Some(label) = doc.attr(id, "aria-label") {
        let label = collapse(label);
        if !label.is_empty() {
            return label;
        }
    }

    // Sources that outrank a label: an image's alt text is its name, and a
    // submit button is named by its value.
    if let Some(name) = strong_tag_name(doc, id) {
        return name;
    }

    // A form control takes its name from a label, either by `for` or by being
    // wrapped in one. This outranks a placeholder, which is only a hint.
    if is_labelable(role) {
        if let Some(name) = label_for(doc, id) {
            return name;
        }
    }

    // Weaker tag-specific fallbacks, such as a placeholder.
    if let Some(name) = weak_tag_name(doc, id) {
        return name;
    }

    // Content-derived names.
    if names_from_content(role) {
        let text = collapse(&doc.text_content(id));
        if !text.is_empty() {
            return text;
        }
    }

    doc.attr(id, "title").map(collapse).unwrap_or_default()
}

/// Names that outrank an associated label.
fn strong_tag_name(doc: &Document, id: NodeId) -> Option<String> {
    let tag = doc.tag(id)?;
    match tag {
        "img" | "area" => doc.attr(id, "alt").map(collapse).filter(|s| !s.is_empty()),
        "input" => {
            let kind = doc.attr(id, "type").unwrap_or("text");
            match kind {
                "button" | "submit" | "reset" => {
                    if let Some(value) = doc.attr(id, "value") {
                        let value = collapse(value);
                        if !value.is_empty() {
                            return Some(value);
                        }
                    }
                    Some(match kind {
                        "submit" => "Submit".to_string(),
                        "reset" => "Reset".to_string(),
                        _ => String::new(),
                    })
                }
                "image" => Some(
                    doc.attr(id, "alt")
                        .map(collapse)
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "Submit".to_string()),
                ),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Names that only apply when nothing better was found. A placeholder is a
/// hint about expected input, not a name, so a `<label>` beats it.
fn weak_tag_name(doc: &Document, id: NodeId) -> Option<String> {
    match doc.tag(id)? {
        "input" | "textarea" => {
            doc.attr(id, "placeholder").map(collapse).filter(|s| !s.is_empty())
        }
        _ => None,
    }
}

fn is_labelable(role: &str) -> bool {
    matches!(
        role,
        "textbox"
            | "searchbox"
            | "checkbox"
            | "radio"
            | "switch"
            | "combobox"
            | "listbox"
            | "slider"
            | "spinbutton"
            | "button"
            | "meter"
            | "progressbar"
    )
}

fn label_for(doc: &Document, id: NodeId) -> Option<String> {
    // `<label for="...">`
    if let Some(own_id) = doc.attr(id, "id") {
        for node in doc.descendants(doc.document_id()) {
            if doc.is_tag(node, "label") && doc.attr(node, "for") == Some(own_id) {
                let text = collapse(&doc.text_content(node));
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
    }
    // A label wrapping the control, as in `<label><input> Name</label>`.
    let mut parent = doc.parent(id);
    while let Some(node) = parent {
        if doc.is_tag(node, "label") {
            let text = collapse(&doc.text_content(node));
            if !text.is_empty() {
                return Some(text);
            }
        }
        parent = doc.parent(node);
    }
    None
}

fn names_from_content(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "link"
            | "heading"
            | "paragraph"
            | "listitem"
            | "option"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "tab"
            | "treeitem"
            | "cell"
            | "rowheader"
            | "columnheader"
            | "term"
            | "definition"
            | "caption"
            | "legend"
            | "LabelText"
            | "tooltip"
            | "status"
            | "alert"
            | "emphasis"
            | "strong"
            | "code"
            | "blockquote"
            | "mark"
            | "label"
    )
}

/// The CDP shape `Accessibility.getFullAXTree` returns.
pub fn to_cdp(nodes: &[AxNode]) -> Value {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        let mut entry = json!({
            "nodeId": node.id,
            "ignored": false,
            "role": { "type": "role", "value": node.role },
            "backendDOMNodeId": node.backend_dom_node_id,
        });
        if let Some(parent) = &node.parent {
            entry["parentId"] = json!(parent);
        }
        if !node.children.is_empty() {
            entry["childIds"] = json!(node.children);
        }
        if !node.name.is_empty() {
            entry["name"] = json!({ "type": "computedString", "value": node.name });
        }
        let mut properties = Vec::new();
        if let Some(level) = node.level {
            properties.push(json!({ "name": "level", "value": { "type": "integer", "value": level } }));
        }
        // A toggle's state is the `checked` property, as Chrome reports it,
        // not a value.
        let toggle = matches!(node.role, "checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio");
        match &node.value {
            Some(state) if toggle => {
                properties.push(json!({ "name": "checked", "value": { "type": "tristate", "value": state } }));
            }
            Some(value) => entry["value"] = json!({ "type": "string", "value": value }),
            None => {}
        }
        if !properties.is_empty() {
            entry["properties"] = json!(properties);
        }
        out.push(entry);
    }
    json!({ "nodes": out })
}

#[cfg(test)]
mod tests {
    use super::{build, to_cdp, AxNode};
    use crate::parse::parse_html;

    /// The same page `engine-fake` served. The point of this test is that the
    /// real path reproduces the output `agent-browser` already accepted.
    const LOGIN_PAGE: &str = r#"
<!doctype html>
<html>
  <head><title>Sandbox Login</title></head>
  <body>
    <h1>Sandbox Login</h1>
    <p>Sign in to continue to the sandbox.</p>
    <form id="login">
      <label for="user">User</label>
      <input id="user" type="text" name="user">
      <label for="pw">Password</label>
      <input id="pw" type="password" name="pw">
      <button id="submit" type="submit">Sign in</button>
    </form>
    <a id="more" href="/more">More info</a>
  </body>
</html>
"#;

    fn names_of(nodes: &[AxNode], role: &str) -> Vec<String> {
        nodes.iter().filter(|n| n.role == role).map(|n| n.name.clone()).collect()
    }

    fn find<'a>(nodes: &'a [AxNode], role: &str, name: &str) -> Option<&'a AxNode> {
        nodes.iter().find(|n| n.role == role && n.name == name)
    }

    #[test]
    fn login_page_matches_what_agent_browser_accepted() {
        let doc = parse_html(LOGIN_PAGE);
        let nodes = build(&doc);

        let root = nodes.first().expect("a root node");
        assert_eq!(root.role, "RootWebArea");
        assert_eq!(root.name, "Sandbox Login");

        assert_eq!(names_of(&nodes, "heading"), vec!["Sandbox Login"]);
        assert!(
            names_of(&nodes, "paragraph").iter().any(|n| n.starts_with("Sign in to continue")),
            "paragraph name missing: {:?}",
            names_of(&nodes, "paragraph")
        );
        assert!(find(&nodes, "form", "").is_some(), "form node missing");

        // Labels are their own role and name the controls through `for`.
        let user = find(&nodes, "textbox", "User").expect("textbox named User");
        assert!(user.value.is_some(), "textbox should carry a value");
        let password = find(&nodes, "textbox", "Password").expect("textbox named Password");
        // A password field must never expose a value.
        assert_eq!(password.value.as_deref(), Some(""));

        assert!(find(&nodes, "button", "Sign in").is_some(), "button missing");
        assert!(find(&nodes, "link", "More info").is_some(), "link missing");
    }

    #[test]
    fn heading_levels_are_reported() {
        let doc = parse_html("<body><h1>one</h1><h3>three</h3><div role='heading' aria-level='4'>four</div></body>");
        let nodes = build(&doc);
        let levels: Vec<(String, Option<u8>)> =
            nodes.iter().filter(|n| n.role == "heading").map(|n| (n.name.clone(), n.level)).collect();
        assert_eq!(
            levels,
            vec![
                ("one".to_string(), Some(1)),
                ("three".to_string(), Some(3)),
                ("four".to_string(), Some(4)),
            ]
        );
    }

    #[test]
    fn inline_styles_hide_subtrees() {
        let doc = parse_html(
            "<body><div style='display: none'><button>hidden screen</button></div>\
             <div style='visibility:hidden'><p>invisible</p></div><button>shown</button></body>",
        );
        let names: Vec<String> = build(&doc).into_iter().map(|n| n.name).collect();
        assert!(names.iter().any(|n| n == "shown"), "{names:?}");
        assert!(!names.iter().any(|n| n == "hidden screen" || n == "invisible"), "{names:?}");
    }

    #[test]
    fn excluded_subtrees_disappear() {
        let doc = parse_html(
            "<body><script>var x=1</script><style>a{}</style><div hidden>nope</div><span aria-hidden='true'>also nope</span><input type='hidden' value='secret'><p>kept</p></body>",
        );
        let nodes = build(&doc);
        let all_names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(!all_names.contains(&"nope"), "hidden subtree survived: {all_names:?}");
        assert!(!all_names.contains(&"also nope"), "aria-hidden subtree survived: {all_names:?}");
        assert!(
            !nodes.iter().any(|n| n.value.as_deref() == Some("secret")),
            "hidden input leaked"
        );
        assert!(all_names.contains(&"kept"), "visible text was dropped: {all_names:?}");
    }

    #[test]
    fn name_sources_are_resolved_in_order() {
        let doc = parse_html(
            r#"<body>
                <span id="lbl">From reference</span>
                <button aria-labelledby="lbl" aria-label="ignored">Also ignored</button>
                <button aria-label="From label">Ignored</button>
                <img alt="From alt" src="x.png">
                <input type="submit" value="Send">
                <input type="text" placeholder="From placeholder">
                <label><input type="checkbox"> Wrapped label</label>
            </body>"#,
        );
        let nodes = build(&doc);

        // aria-labelledby beats aria-label beats content.
        assert!(find(&nodes, "button", "From reference").is_some());
        assert!(find(&nodes, "button", "From label").is_some());
        assert!(find(&nodes, "img", "From alt").is_some());
        assert!(find(&nodes, "button", "Send").is_some());
        assert!(find(&nodes, "textbox", "From placeholder").is_some());
        assert!(find(&nodes, "checkbox", "Wrapped label").is_some());
    }

    #[test]
    fn a_label_outranks_a_placeholder() {
        // Both are present, and the label is the name. Getting this backwards
        // makes a client look for "type here" instead of "Search terms".
        let doc = parse_html(
            "<body><label for='q'>Search terms</label><input id='q' placeholder='type here'></body>",
        );
        let nodes = build(&doc);
        assert!(find(&nodes, "textbox", "Search terms").is_some(), "label should win");
        assert!(find(&nodes, "textbox", "type here").is_none(), "placeholder should not win");

        // With no label, the placeholder is all there is.
        let doc = parse_html("<body><input id='q' placeholder='type here'></body>");
        let nodes = build(&doc);
        assert!(find(&nodes, "textbox", "type here").is_some());
    }

    #[test]
    fn an_alt_outranks_a_label() {
        // An image inside a label is still named by its alt text.
        let doc = parse_html("<body><label><img src='x' alt='From alt'> caption</label></body>");
        let nodes = build(&doc);
        assert!(find(&nodes, "img", "From alt").is_some());
    }

    #[test]
    fn checkboxes_report_checked_state() {
        let doc = parse_html(
            "<body><input type='checkbox' id='a' checked><input type='checkbox' id='b'><input type='checkbox' id='c' aria-checked='mixed'></body>",
        );
        let nodes = build(&doc);
        let values: Vec<Option<String>> =
            nodes.iter().filter(|n| n.role == "checkbox").map(|n| n.value.clone()).collect();
        assert_eq!(values, vec![Some("true".into()), Some("false".into()), Some("mixed".into())]);
        // As Chrome reports it: a `checked` property, not a value.
        let cdp = to_cdp(&nodes);
        let first = cdp["nodes"].as_array().unwrap().iter().find(|n| n["role"]["value"] == "checkbox").unwrap();
        assert_eq!(first["properties"][0], serde_json::json!({ "name": "checked", "value": { "type": "tristate", "value": "true" } }));
        assert!(first.get("value").is_none());
    }

    #[test]
    fn text_nodes_become_static_text_children() {
        let doc = parse_html("<body><p>hello world</p></body>");
        let nodes = build(&doc);
        let paragraph = nodes.iter().find(|n| n.role == "paragraph").expect("paragraph");
        assert_eq!(paragraph.name, "hello world");
        let children: Vec<&AxNode> =
            paragraph.children.iter().map(|id| nodes.iter().find(|n| &n.id == id).unwrap()).collect();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].role, "StaticText");
        assert_eq!(children[0].name, "hello world");
    }

    #[test]
    fn tree_links_are_consistent() {
        let doc = parse_html("<body><div><p>a</p><p>b</p></div></body>");
        let nodes = build(&doc);

        // Every child id resolves, and every parent id is really the parent.
        for node in &nodes {
            for child in &node.children {
                let found = nodes.iter().find(|n| n.id == *child).expect("child id resolves");
                assert_eq!(found.parent.as_deref(), Some(node.id.as_str()));
            }
            if let Some(parent) = &node.parent {
                let found = nodes.iter().find(|n| n.id == *parent).expect("parent id resolves");
                assert!(found.children.contains(&node.id), "parent does not list this child");
            }
        }
        // Ids are unique.
        let mut ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate node ids");

        // backendDOMNodeIds point at real element or text nodes.
        for node in &nodes {
            assert!(node.backend_dom_node_id >= 1);
            assert!(node.backend_dom_node_id <= doc.len());
        }
    }

    #[test]
    fn cdp_shape_uses_the_fields_clients_read() {
        let doc = parse_html("<body><h2>Title</h2></body>");
        let nodes = build(&doc);
        let value = to_cdp(&nodes);
        let arr = value["nodes"].as_array().expect("nodes array");
        assert!(!arr.is_empty());
        let heading = arr
            .iter()
            .find(|n| n["role"]["value"] == "heading")
            .expect("a heading node");
        assert_eq!(heading["name"]["value"], "Title");
        assert_eq!(heading["properties"][0]["name"], "level");
        assert_eq!(heading["properties"][0]["value"]["value"], 2);
        assert!(heading["backendDOMNodeId"].is_number());
        assert!(heading["parentId"].is_string());
    }

    #[test]
    fn empty_document_still_produces_a_root() {
        // The parser synthesises html/head/body even from nothing. `head` is
        // excluded, so what remains is the root and an empty body.
        let doc = parse_html("");
        let nodes = build(&doc);
        assert_eq!(nodes[0].role, "RootWebArea");
        assert_eq!(nodes[0].name, "");
        assert_eq!(nodes.iter().filter(|n| n.role == "RootWebArea").count(), 1);
        assert!(nodes.iter().all(|n| n.role == "RootWebArea" || n.role == "generic"));
    }

    #[test]
    fn nodes_are_emitted_in_document_order() {
        // Parents precede their children, and siblings follow source order.
        let doc = parse_html("<body><div id='outer'><p id='first'>a</p><p id='second'>b</p></div><span>c</span></body>");
        let nodes = build(&doc);
        let paragraphs: Vec<&str> = nodes
            .iter()
            .filter(|n| n.role == "paragraph")
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(paragraphs, vec!["a", "b"]);

        for node in &nodes {
            for child in &node.children {
                let child_pos = nodes.iter().position(|n| n.id == *child).unwrap();
                let parent_pos = nodes.iter().position(|n| n.id == node.id).unwrap();
                assert!(parent_pos < child_pos, "child emitted before its parent");
            }
        }
    }
}

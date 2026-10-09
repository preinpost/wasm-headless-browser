//! Styles.
//!
//! For now this is the inline `style` attribute alone, which is how React
//! apps commonly hide a screen (`style="display: none"`). The cascade (M1.4)
//! adds stylesheets underneath the same entry points.

use crate::dom::{Document, NodeId};

/// The declarations of the element's `style` attribute, in order, with names
/// lowercased and `!important` dropped. Later declarations win, as in CSS.
pub fn inline_declarations(doc: &Document, id: NodeId) -> Vec<(String, String)> {
    let Some(text) = doc.attr(id, "style") else { return Vec::new() };
    text.split(';')
        .filter_map(|part| {
            let (name, value) = part.split_once(':')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            let name = if name.starts_with("--") { name.to_string() } else { name.to_ascii_lowercase() };
            let mut value = value.trim();
            if let Some(stripped) = value.strip_suffix("!important").or_else(|| value.strip_suffix("! important")) {
                value = stripped.trim_end();
            }
            Some((name, value.to_string()))
        })
        .collect()
}

/// The last inline value given for `name`.
pub fn inline_value(doc: &Document, id: NodeId, name: &str) -> Option<String> {
    inline_declarations(doc, id).into_iter().rev().find(|(n, _)| n == name).map(|(_, v)| v)
}

/// Whether the element's own style takes it and its subtree out of what is
/// rendered: `display: none`, or `visibility: hidden` / `collapse`.
///
/// Visibility is inherited and a descendant can set it back to `visible`;
/// treating the subtree as hidden is the approximation until the cascade
/// computes inheritance.
pub fn hides(doc: &Document, id: NodeId) -> bool {
    let decls = inline_declarations(doc, id);
    let last = |name: &str| decls.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.to_ascii_lowercase());
    last("display").as_deref() == Some("none") || matches!(last("visibility").as_deref(), Some("hidden") | Some("collapse"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_html;

    fn first(doc: &Document, tag: &str) -> NodeId {
        doc.find_tag(doc.document_id(), tag).unwrap()
    }

    #[test]
    fn reads_inline_declarations_the_way_css_does() {
        let doc = parse_html("<p style='DISPLAY: block; color:red ; display: none !important; --x: 1'>x</p>");
        let p = first(&doc, "p");
        assert_eq!(inline_value(&doc, p, "display").as_deref(), Some("none"));
        assert_eq!(inline_value(&doc, p, "color").as_deref(), Some("red"));
        assert_eq!(inline_value(&doc, p, "--x").as_deref(), Some("1"));
        assert!(hides(&doc, p));
    }

    #[test]
    fn hidden_and_shown_by_inline_style() {
        let doc = parse_html("<div style='visibility:hidden'>a</div><span style='display:inline'>b</span>");
        assert!(hides(&doc, first(&doc, "div")));
        assert!(!hides(&doc, first(&doc, "span")));
    }
}

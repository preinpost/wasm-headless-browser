//! Selector matching.
//!
//! A deliberately small subset of CSS: type, `*`, `#id`, `.class`,
//! `[attr]`, `[attr="value"]`, and the descendant combinator. Pseudo-classes,
//! combinators other than descendant, and specificity are not implemented.
//!
//! This exists because clients send `document.querySelector("a")` as a probe
//! long before they send anything complicated, and because resolving a
//! selector against the real tree is the difference between an engine that can
//! answer questions about a page and one that cannot.

use crate::dom::{Document, NodeId};

/// The first match in document order, or `None`.
pub fn query(doc: &Document, root: NodeId, selector: &str) -> Option<NodeId> {
    query_all(doc, root, selector).into_iter().next()
}

/// Every match in document order.
pub fn query_all(doc: &Document, root: NodeId, selector: &str) -> Vec<NodeId> {
    let groups = parse(selector);
    if groups.is_empty() {
        return Vec::new();
    }
    doc.descendants(root)
        .filter(|id| doc.is_element(*id))
        .filter(|id| groups.iter().any(|group| matches_sequence(doc, *id, group)))
        .collect()
}

/// True when the element matches the whole selector.
pub fn matches(doc: &Document, id: NodeId, selector: &str) -> bool {
    if !doc.is_element(id) {
        return false;
    }
    parse(selector).iter().any(|group| matches_sequence(doc, id, group))
}

/// One compound selector, all of whose parts must match the same element.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<(String, Option<String>)>,
}

impl Compound {
    fn matches(&self, doc: &Document, id: NodeId) -> bool {
        if let Some(tag) = &self.tag {
            if doc.tag(id) != Some(tag.as_str()) {
                return false;
            }
        }
        if let Some(want) = &self.id {
            if doc.attr(id, "id") != Some(want.as_str()) {
                return false;
            }
        }
        for class in &self.classes {
            let has = doc
                .attr(id, "class")
                .map(|c| c.split_whitespace().any(|part| part == class))
                .unwrap_or(false);
            if !has {
                return false;
            }
        }
        for (name, value) in &self.attrs {
            match doc.attr(id, name) {
                None => return false,
                Some(actual) => {
                    if let Some(want) = value {
                        if actual != want {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }
}

/// A group joined by descendant combinators: `div p a` is three compounds.
pub type Sequence = Vec<Compound>;

/// Splits a selector into comma-separated sequences of compounds.
///
/// Anything unsupported in a compound makes that compound unmatchable rather
/// than the whole selector unusable, which keeps a client's more elaborate
/// selectors from silently matching the wrong element.
pub fn parse(selector: &str) -> Vec<Sequence> {
    let mut groups = Vec::new();
    for group in selector.split(',') {
        let mut sequence = Vec::new();
        for part in group.split_whitespace() {
            if let Some(compound) = parse_compound(part) {
                sequence.push(compound);
            } else {
                sequence.clear();
                break;
            }
        }
        if !sequence.is_empty() {
            groups.push(sequence);
        }
    }
    groups
}

fn parse_compound(text: &str) -> Option<Compound> {
    if text.is_empty() {
        return None;
    }
    if text == "*" {
        return Some(Compound::default());
    }

    let mut compound = Compound::default();
    let bytes = text.as_bytes();
    let mut i = 0;

    // Leading type selector.
    if bytes[0].is_ascii_alphabetic() {
        while i < bytes.len() && bytes[i] != b'#' && bytes[i] != b'.' && bytes[i] != b'[' {
            i += 1;
        }
        compound.tag = Some(text[..i].to_ascii_lowercase());
    }

    while i < bytes.len() {
        let marker = bytes[i];
        i += 1;
        match marker {
            b'#' => {
                let start = i;
                while i < bytes.len() && !matches!(bytes[i], b'#' | b'.' | b'[') {
                    i += 1;
                }
                if start == i {
                    return None;
                }
                compound.id = Some(text[start..i].to_string());
            }
            b'.' => {
                let start = i;
                while i < bytes.len() && !matches!(bytes[i], b'#' | b'.' | b'[') {
                    i += 1;
                }
                if start == i {
                    return None;
                }
                compound.classes.push(text[start..i].to_string());
            }
            b'[' => {
                let close = text[i..].find(']')? + i;
                let inner = &text[i..close];
                i = close + 1;
                match inner.split_once('=') {
                    Some((name, value)) => compound.attrs.push((
                        name.trim().to_ascii_lowercase(),
                        Some(value.trim().trim_matches(['"', '\'']).to_string()),
                    )),
                    None => compound.attrs.push((inner.trim().to_ascii_lowercase(), None)),
                }
            }
            // Pseudo-classes, combinators and anything else: refuse rather
            // than pretend.
            _ => return None,
        }
    }

    if compound == Compound::default() && text != "*" {
        return None;
    }
    Some(compound)
}

/// Matches the rightmost compound against `id`, then walks ancestors for the
/// rest, which is what a descendant combinator means.
fn matches_sequence(doc: &Document, id: NodeId, sequence: &Sequence) -> bool {
    let Some((last, rest)) = sequence.split_last() else {
        return false;
    };
    if !last.matches(doc, id) {
        return false;
    }
    let mut current = doc.parent(id);
    for compound in rest.iter().rev() {
        let mut found = false;
        while let Some(node) = current {
            if doc.is_element(node) && compound.matches(doc, node) {
                found = true;
                current = doc.parent(node);
                break;
            }
            current = doc.parent(node);
        }
        if !found {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_html;

    const PAGE: &str = r#"
    <body>
      <div id="top" class="outer shell">
        <p class="lead">first</p>
        <p>second</p>
        <a href="/x" data-id="7">link</a>
      </div>
      <section>
        <div><a href="/y">nested link</a></div>
      </section>
    </body>"#;

    fn text(doc: &Document, id: NodeId) -> String {
        doc.text_content(id).trim().to_string()
    }

    #[test]
    fn matches_type_selectors() {
        let doc = parse_html(PAGE);
        let all = query_all(&doc, doc.document_id(), "p");
        assert_eq!(all.len(), 2);
        assert_eq!(text(&doc, all[0]), "first");
    }

    #[test]
    fn matches_id_and_class() {
        let doc = parse_html(PAGE);
        let top = query(&doc, doc.document_id(), "#top").expect("id selector");
        assert!(doc.is_tag(top, "div"));
        assert_eq!(query_all(&doc, doc.document_id(), ".lead").len(), 1);
        assert_eq!(query_all(&doc, doc.document_id(), "div.shell").len(), 1);
        assert_eq!(query_all(&doc, doc.document_id(), ".missing").len(), 0);
    }

    #[test]
    fn matches_attributes() {
        let doc = parse_html(PAGE);
        assert_eq!(query_all(&doc, doc.document_id(), "[href]").len(), 2);
        assert_eq!(query_all(&doc, doc.document_id(), "[href=\"/y\"]").len(), 1);
        assert_eq!(query_all(&doc, doc.document_id(), "a[data-id=\"7\"]").len(), 1);
    }

    #[test]
    fn matches_the_universal_selector() {
        let doc = parse_html("<body><p>a</p></body>");
        let all = query_all(&doc, doc.document_id(), "*");
        // html, head, body, p
        assert!(all.len() >= 4, "got {} elements", all.len());
    }

    #[test]
    fn matches_descendant_combinators() {
        let doc = parse_html(PAGE);
        let links = query_all(&doc, doc.document_id(), "section a");
        assert_eq!(links.len(), 1);
        assert_eq!(text(&doc, links[0]), "nested link");

        // The outer div's link is not inside a section.
        assert_eq!(query_all(&doc, doc.document_id(), "section div a").len(), 1);
        assert_eq!(query_all(&doc, doc.document_id(), "section p").len(), 0);
    }

    #[test]
    fn matches_comma_separated_groups() {
        let doc = parse_html(PAGE);
        // Two paragraphs and two links.
        let either = query_all(&doc, doc.document_id(), "p, a");
        assert_eq!(either.len(), 4);
    }

    #[test]
    fn results_are_in_document_order() {
        let doc = parse_html(PAGE);
        let texts: Vec<String> =
            query_all(&doc, doc.document_id(), "a").iter().map(|id| text(&doc, *id)).collect();
        assert_eq!(texts, vec!["link", "nested link"]);
    }

    #[test]
    fn scopes_to_the_given_root() {
        let doc = parse_html(PAGE);
        let top = query(&doc, doc.document_id(), "#top").unwrap();
        assert_eq!(query_all(&doc, top, "p").len(), 2);
        // The section is a sibling of #top, not a descendant.
        assert_eq!(query_all(&doc, top, "a").len(), 1);
    }

    #[test]
    fn unsupported_selectors_match_nothing_rather_than_something_wrong() {
        let doc = parse_html(PAGE);
        assert_eq!(query_all(&doc, doc.document_id(), "p:first-child").len(), 0);
        assert_eq!(query_all(&doc, doc.document_id(), "div > p").len(), 0);
        assert_eq!(query_all(&doc, doc.document_id(), "div + p").len(), 0);
        assert_eq!(query_all(&doc, doc.document_id(), "").len(), 0);
    }

    #[test]
    fn matching_a_single_element_agrees_with_query() {
        let doc = parse_html(PAGE);
        let top = query(&doc, doc.document_id(), "#top").unwrap();
        assert!(matches(&doc, top, "div#top.outer"));
        assert!(matches(&doc, top, "body div"));
        assert!(!matches(&doc, top, "section div"));
    }
}

//! HTML parsing into the arena DOM.
//!
//! `html5ever` does the tokenizing, tree construction and error recovery; this
//! module only supplies the tree. Implementing `TreeSink` here rather than
//! pulling in `markup5ever_rcdom` is what keeps the DOM `Send` and keeps node
//! handles as plain indices, which M2 will need.
//!
//! The sink has to take `&self` for every mutation, so the tree sits behind a
//! mutex. That costs a lock per parser callback and buys a `Send` document,
//! which is the trade `browser-core`'s `Engine` trait needs.

use std::ops::Deref;
use std::sync::Mutex;

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{parse_document, parse_fragment as html5ever_fragment, ParseOpts};
use markup5ever::interface::{Attribute, ElemName};
use markup5ever::{LocalName, Namespace, QualName};

use crate::dom::{Attr, Document, Node, NodeData, NodeId, Quirks};

/// The element name borrowed for the duration of one parser callback. It owns a
/// `QualName` because the tree lives behind a mutex and cannot be borrowed for
/// the `'a` the trait signature asks for. Cloning a `QualName` only bumps two
/// atom refcounts.
#[derive(Debug, Clone)]
pub struct OwnedName(QualName);

impl ElemName for OwnedName {
    fn ns(&self) -> &Namespace {
        &self.0.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

pub struct DomSink {
    inner: Mutex<Document>,
    errors: Mutex<u32>,
}

impl DomSink {
    pub fn new() -> Self {
        DomSink { inner: Mutex::new(Document::new()), errors: Mutex::new(0) }
    }
}

impl Default for DomSink {
    fn default() -> Self {
        Self::new()
    }
}

fn attr_from(attr: Attribute) -> Attr {
    Attr { name: attr.name, value: attr.value.to_string() }
}

impl TreeSink for DomSink {
    type Handle = NodeId;
    type Output = Document;
    type ElemName<'a> = OwnedName;

    fn finish(self) -> Document {
        self.inner.into_inner().unwrap_or_else(|e| e.into_inner())
    }

    fn parse_error(&self, _msg: std::borrow::Cow<'static, str>) {
        if let Ok(mut n) = self.errors.lock() {
            *n += 1;
        }
    }

    fn get_document(&self) -> NodeId {
        self.inner.lock().map(|d| d.document_id()).unwrap_or(0)
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> OwnedName {
        let doc = self.inner.lock().unwrap();
        // The trait documents this as element-only, so a missing name is a
        // parser bug rather than something to paper over.
        OwnedName(doc.node(*target).name.clone().expect("elem_name on a non-element node"))
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        _flags: ElementFlags,
    ) -> NodeId {
        let mut doc = self.inner.lock().unwrap();
        let is_template = name.local.deref() == "template";
        let id = doc.push(Node {
            data: NodeData::Element { contents: None },
            name: Some(name),
            attrs: attrs.into_iter().map(attr_from).collect(),
            ..Node::default()
        });
        if is_template {
            // A template's contents live in their own fragment, which the
            // parser asks for through `get_template_contents`.
            let contents = doc.push(Node { data: NodeData::Document, ..Node::default() });
            doc.set_template_contents(id, contents);
        }
        id
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        let mut doc = self.inner.lock().unwrap();
        doc.push(Node { data: NodeData::Comment, text: text.to_string(), ..Node::default() })
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        let mut doc = self.inner.lock().unwrap();
        doc.push(Node {
            data: NodeData::Comment,
            text: format!("{target} {data}"),
            ..Node::default()
        })
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        let mut doc = self.inner.lock().unwrap();
        match child {
            NodeOrText::AppendNode(id) => doc.append_child(*parent, id),
            NodeOrText::AppendText(text) => {
                // Adjacent text nodes are not allowed; the parser expects the
                // sink to merge them.
                let last = doc.children(*parent).last().copied();
                match last {
                    Some(last) if doc.is_text(last) => {
                        doc.append_text_to(last, &text);
                    }
                    _ => {
                        let id = doc.push(Node {
                            data: NodeData::Text,
                            text: text.to_string(),
                            ..Node::default()
                        });
                        doc.append_child(*parent, id);
                    }
                }
            }
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = {
            let doc = self.inner.lock().unwrap();
            doc.parent(*element).is_some()
        };
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        let mut doc = self.inner.lock().unwrap();
        match new_node {
            NodeOrText::AppendNode(id) => doc.insert_before(*sibling, id),
            NodeOrText::AppendText(text) => {
                // Merge with the old previous sibling if it is text, as the
                // spec requires.
                let parent = doc.parent(*sibling);
                let prev = parent.and_then(|p| {
                    let children = doc.children(p);
                    let pos = children.iter().position(|c| c == sibling)?;
                    pos.checked_sub(1).map(|i| children[i])
                });
                match prev {
                    Some(prev) if doc.is_text(prev) => doc.append_text_to(prev, &text),
                    _ => {
                        let id = doc.push(Node {
                            data: NodeData::Text,
                            text: text.to_string(),
                            ..Node::default()
                        });
                        doc.insert_before(*sibling, id);
                    }
                }
            }
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let mut doc = self.inner.lock().unwrap();
        let data = if public_id.is_empty() && system_id.is_empty() {
            name.to_string()
        } else {
            format!("{name} \"{public_id}\" \"{system_id}\"")
        };
        let id = doc.push(Node { data: NodeData::Doctype { name: data }, ..Node::default() });
        let document = doc.document_id();
        doc.append_child(document, id);
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        let doc = self.inner.lock().unwrap();
        doc.template_contents(*target).unwrap_or(*target)
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        let mut doc = self.inner.lock().unwrap();
        doc.set_quirks(match mode {
            QuirksMode::NoQuirks => Quirks::NoQuirks,
            QuirksMode::LimitedQuirks => Quirks::LimitedQuirks,
            QuirksMode::Quirks => Quirks::Quirks,
        });
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let mut doc = self.inner.lock().unwrap();
        for attr in attrs {
            let name = attr.name.local.deref().to_string();
            if doc.attr(*target, &name).is_none() {
                let converted = attr_from(attr);
                doc.push_attr(*target, converted);
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        let mut doc = self.inner.lock().unwrap();
        doc.detach(*target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        let mut doc = self.inner.lock().unwrap();
        let children = doc.children(*node).to_vec();
        for child in children {
            doc.append_child(*new_parent, child);
        }
    }
}

/// Parses a complete HTML document.
pub fn parse_html(input: &str) -> Document {
    let sink = DomSink::new();
    parse_document(sink, ParseOpts::default()).one(input)
}

/// Parses `input` as the contents of a `context` element, as `innerHTML` and
/// `insertAdjacentHTML` do. Returns the scratch document and the node whose
/// children are the parsed fragment.
pub fn parse_fragment(input: &str, context: &str) -> (Document, NodeId) {
    let sink = DomSink::new();
    let name = QualName::new(None, markup5ever::ns!(html), LocalName::from(context));
    let doc = html5ever_fragment(sink, ParseOpts::default(), name, Vec::new(), false).one(input);
    // html5ever puts the fragment under a synthetic <html> root.
    let container = doc
        .children(doc.document_id())
        .iter()
        .copied()
        .find(|id| doc.is_element(*id))
        .unwrap_or(doc.document_id());
    (doc, container)
}

/// Parses bytes, decoding as UTF-8 and replacing anything invalid. Real pages
/// arrive as bytes, often with a mismatched or missing declaration.
pub fn parse_html_bytes(input: &[u8]) -> Document {
    let sink = DomSink::new();
    parse_document(sink, ParseOpts::default()).from_utf8().one(input)
}

#[cfg(test)]
mod tests {
    use super::{parse_fragment, parse_html};

    #[test]
    fn parses_a_fragment_in_context() {
        let (doc, root) = parse_fragment("<li>a</li><li>b <b>c</b></li>", "ul");
        let kids: Vec<_> = doc.children(root).iter().filter_map(|c| doc.tag(*c)).collect();
        assert_eq!(kids, ["li", "li"]);
        assert_eq!(doc.text_content(root), "ab c");
    }

    use crate::dom::NodeData;

    fn tags(doc: &crate::dom::Document) -> Vec<String> {
        doc.descendants(doc.document_id())
            .filter_map(|id| doc.tag(id).map(str::to_string))
            .collect()
    }

    #[test]
    fn builds_the_document_structure() {
        let doc = parse_html("<!doctype html><html><head><title>T</title></head><body><p>hi</p></body></html>");
        let tags = tags(&doc);
        assert!(tags.contains(&"html".to_string()), "html missing: {tags:?}");
        assert!(tags.contains(&"head".to_string()), "head missing: {tags:?}");
        assert!(tags.contains(&"body".to_string()), "body missing: {tags:?}");
        assert_eq!(doc.title().as_deref(), Some("T"));
        assert_eq!(doc.quirks(), crate::dom::Quirks::NoQuirks);
    }

    #[test]
    fn inserts_implied_elements() {
        // No <html>, <head> or <body> in the source; the parser must supply them.
        let doc = parse_html("<p>hello");
        assert_eq!(doc.find_tag(doc.document_id(), "body").is_some(), true);
        assert_eq!(doc.find_tag(doc.document_id(), "html").is_some(), true);
        let body = doc.find_tag(doc.document_id(), "body").unwrap();
        assert_eq!(doc.text_content(body).trim(), "hello");
    }

    #[test]
    fn merges_adjacent_text_nodes() {
        // Foster parenting and entity handling produce split text runs; the sink
        // is responsible for merging them.
        let doc = parse_html("<body><div>a&amp;b</div></body>");
        let div = doc.find_tag(doc.document_id(), "div").unwrap();
        let text_children: Vec<_> =
            doc.children(div).iter().filter(|id| doc.is_text(**id)).collect();
        assert_eq!(text_children.len(), 1, "text was split into {} nodes", text_children.len());
        assert_eq!(doc.text_content(div), "a&b");
    }

    #[test]
    fn recovers_from_malformed_markup() {
        // Unclosed tags and misnesting are the whole reason to use html5ever.
        let doc = parse_html("<body><b><i>x</b></i><table><tr><td>cell</table>");
        let body = doc.find_tag(doc.document_id(), "body").unwrap();
        assert!(doc.text_content(body).contains("x"));
        assert!(doc.text_content(body).contains("cell"));
    }

    #[test]
    fn reads_attributes_and_their_names() {
        let doc =
            parse_html(r#"<body><a href="/x" data-id="7" aria-label="More">text</a></body>"#);
        let a = doc.find_tag(doc.document_id(), "a").unwrap();
        assert_eq!(doc.attr(a, "href"), Some("/x"));
        assert_eq!(doc.attr(a, "data-id"), Some("7"));
        assert_eq!(doc.attr(a, "aria-label"), Some("More"));
        assert_eq!(doc.attr(a, "missing"), None);
        assert_eq!(doc.attrs_with_prefix(a, "aria-").count(), 1);
    }

    #[test]
    fn keeps_template_contents_separate() {
        let doc = parse_html("<body><template><p>inside</p></template><p>outside</p></body>");
        let template = doc.find_tag(doc.document_id(), "template").unwrap();
        let contents = doc.template_contents(template).expect("template contents");
        assert!(doc.is_tag(doc.find_tag(contents, "p").unwrap(), "p"));
        // The template's own children stay empty; its content is elsewhere.
        assert!(doc.children(template).is_empty());
        let body = doc.find_tag(doc.document_id(), "body").unwrap();
        assert_eq!(doc.text_content(body), "outside");
    }

    #[test]
    fn records_comments_and_doctype() {
        let doc = parse_html("<!doctype html><!-- note --><body>x</body>");
        let mut saw_doctype = false;
        let mut saw_comment = false;
        for id in doc.descendants(doc.document_id()) {
            match &doc.node(id).data {
                NodeData::Doctype { name } => {
                    assert_eq!(name, "html");
                    saw_doctype = true;
                }
                NodeData::Comment => {
                    assert_eq!(doc.node(id).text.trim(), "note");
                    saw_comment = true;
                }
                _ => {}
            }
        }
        assert!(saw_doctype && saw_comment);
    }

    #[test]
    fn descendants_are_in_document_order() {
        let doc = parse_html("<body><div><b>one</b><i>two</i></div></body>");
        let div = doc.find_tag(doc.document_id(), "div").unwrap();
        let order: Vec<String> =
            doc.descendants(div).filter_map(|id| doc.tag(id).map(str::to_string)).collect();
        assert_eq!(order, vec!["div", "b", "i"]);
    }

    #[test]
    fn document_is_send() {
        // The Engine trait requires this; an Rc-based DOM would fail here.
        fn assert_send<T: Send>() {}
        assert_send::<crate::dom::Document>();
        let doc = parse_html("<body>x</body>");
        std::thread::spawn(move || doc.len()).join().unwrap();
    }
}

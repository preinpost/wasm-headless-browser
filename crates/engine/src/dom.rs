//! An arena DOM.
//!
//! Nodes live in one `Vec` and refer to each other by index. That choice is
//! deliberate and it is the reason this is not `markup5ever_rcdom`:
//!
//!   * indices are `Send`, so `browser-core`'s `Engine` trait stays object-safe
//!     across threads. `Rc<RefCell<_>>` is not.
//!   * the layout and accessibility passes walk children constantly, and an
//!     arena makes that a slice iteration instead of a pointer chase.
//!   * M2 needs stable handles to hand to QuickJS. An index is exactly that.
//!
//! Nothing here knows about CSS or layout. This is the tree, and only the tree.

use markup5ever::interface::QualName;

pub type NodeId = usize;

#[derive(Debug, Clone)]
pub struct Attr {
    pub name: QualName,
    pub value: String,
}

impl Attr {
    /// The attribute's local name, lowercased as HTML requires.
    pub fn local_name(&self) -> &str {
        self.name.local.as_ref()
    }
}

#[derive(Debug, Clone)]
pub enum NodeData {
    Document,
    Doctype { name: String },
    /// `template_contents` holds the fragment a `<template>` element owns. The
    /// parser creates it separately, and `get_template_contents` hands it back.
    Element { contents: Option<NodeId> },
    Text,
    Comment,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub data: NodeData,
    /// The element's qualified name. Present only for elements, and kept out of
    /// any cell so that `TreeSink::elem_name` can hand out a borrow.
    pub name: Option<QualName>,
    pub attrs: Vec<Attr>,
    /// Character data for text, comments and doctypes.
    pub text: String,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

impl Default for Node {
    fn default() -> Self {
        Node {
            data: NodeData::Document,
            name: None,
            attrs: Vec::new(),
            text: String::new(),
            parent: None,
            children: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quirks {
    #[default]
    NoQuirks,
    LimitedQuirks,
    Quirks,
}

#[derive(Debug, Default, Clone)]
pub struct Document {
    nodes: Vec<Node>,
    /// The `<Document>` node. Always 0, but named for readability at call sites.
    document: NodeId,
    quirks: Quirks,
}

impl Document {
    pub fn new() -> Self {
        Document { nodes: vec![Node::default()], document: 0, quirks: Quirks::NoQuirks }
    }

    pub fn document_id(&self) -> NodeId {
        self.document
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn quirks(&self) -> Quirks {
        self.quirks
    }

    pub fn set_quirks(&mut self, quirks: Quirks) {
        self.quirks = quirks;
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id].parent
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].children
    }

    pub fn is_element(&self, id: NodeId) -> bool {
        matches!(self.nodes[id].data, NodeData::Element { .. })
    }

    pub fn is_text(&self, id: NodeId) -> bool {
        matches!(self.nodes[id].data, NodeData::Text)
    }

    /// The element's local name, lowercased. `None` for non-elements.
    pub fn tag(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].name.as_ref().map(|n| n.local.as_ref())
    }

    /// True when the node is an element with this local name.
    pub fn is_tag(&self, id: NodeId, tag: &str) -> bool {
        self.tag(id) == Some(tag)
    }

    pub fn attrs(&self, id: NodeId) -> &[Attr] {
        &self.nodes[id].attrs
    }

    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.nodes[id].attrs.iter().find(|a| a.local_name() == name).map(|a| a.value.as_str())
    }

    pub fn has_attr(&self, id: NodeId, name: &str) -> bool {
        self.attr(id, name).is_some()
    }

    /// Attributes whose local name starts with `prefix`, for `aria-*` and
    /// `data-*` style questions.
    pub fn attrs_with_prefix<'a>(&'a self, id: NodeId, prefix: &'a str) -> impl Iterator<Item = &'a Attr> {
        self.nodes[id].attrs.iter().filter(move |a| a.local_name().starts_with(prefix))
    }

    pub fn template_contents(&self, id: NodeId) -> Option<NodeId> {
        match self.nodes[id].data {
            NodeData::Element { contents } => contents,
            _ => None,
        }
    }

    /// Every node in the subtree, pre-order, the node itself first.
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants { doc: self, stack: vec![id] }
    }

    /// The text of the subtree, concatenated. Whitespace is preserved; callers
    /// that want it collapsed should do so themselves.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        for node in self.descendants(id) {
            if let NodeData::Text = self.nodes[node].data {
                out.push_str(&self.nodes[node].text);
            }
        }
        out
    }

    /// The first element in the subtree whose local name matches.
    pub fn find_tag(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        self.descendants(id).find(|n| self.is_tag(*n, tag))
    }

    /// The first element in the subtree carrying this id attribute.
    pub fn find_id(&self, id: NodeId, value: &str) -> Option<NodeId> {
        self.descendants(id).find(|n| self.attr(*n, "id") == Some(value))
    }

    /// The first `<title>` element's text.
    pub fn title(&self) -> Option<String> {
        let head = self.find_tag(self.document, "head")?;
        let title = self.find_tag(head, "title")?;
        Some(self.text_content(title).trim().to_string())
    }

    /// Insert `child` as the last child of `parent`, detaching it first if it
    /// already has a parent.
    pub(crate) fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    /// Remove `id` from its parent, leaving it intact.
    pub(crate) fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.nodes[id].parent.take() {
            if let Some(pos) = self.nodes[parent].children.iter().position(|c| *c == id) {
                self.nodes[parent].children.remove(pos);
            }
        }
    }

    /// Insert `child` immediately before `sibling`.
    pub(crate) fn insert_before(&mut self, sibling: NodeId, child: NodeId) {
        self.detach(child);
        if let Some(parent) = self.nodes[sibling].parent {
            let pos = self.nodes[parent]
                .children
                .iter()
                .position(|c| *c == sibling)
                .unwrap_or(self.nodes[parent].children.len());
            self.nodes[parent].children.insert(pos, child);
            self.nodes[child].parent = Some(parent);
        }
    }

    /// Merge `text` into the node's text, which is what the parser expects when
    /// two text nodes would otherwise become adjacent.
    pub(crate) fn append_text_to(&mut self, id: NodeId, text: &str) {
        self.nodes[id].text.push_str(text);
    }

    pub(crate) fn push(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    pub(crate) fn push_attr(&mut self, id: NodeId, attr: Attr) {
        self.nodes[id].attrs.push(attr);
    }

    pub(crate) fn set_template_contents(&mut self, id: NodeId, contents: NodeId) {
        if let NodeData::Element { contents: slot } = &mut self.nodes[id].data {
            *slot = Some(contents);
        }
    }
}

pub struct Descendants<'a> {
    doc: &'a Document,
    stack: Vec<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let id = self.stack.pop()?;
        // Push children reversed so they come out in document order.
        for child in self.doc.children(id).iter().rev() {
            self.stack.push(*child);
        }
        Some(id)
    }
}

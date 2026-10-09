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

use std::collections::HashMap;

use markup5ever::interface::QualName;
use markup5ever::{ns, LocalName, Namespace};

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
    /// A `DocumentFragment`: a parentless container whose children move, not
    /// itself, when it is inserted.
    Fragment,
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
    /// Form control state that is not an attribute: what the user typed or
    /// script assigned to `value`, and the current `checked` and `selected`.
    /// `None` means "still the default", which the attribute supplies.
    pub control: Control,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Control {
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub selected: Option<bool>,
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
            control: Control::default(),
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

#[derive(Debug, Clone)]
pub struct Document {
    nodes: Vec<Node>,
    /// The `<Document>` node. Always 0, but named for readability at call sites.
    document: NodeId,
    quirks: Quirks,
    url: String,
    /// Bumped by every change that can affect styles, so computed styles are
    /// recomputed only when something changed.
    version: u64,
    /// Fetched stylesheet text, by the `<link>` that asked for it.
    stylesheets: HashMap<NodeId, String>,
    /// Rules script added through the CSSOM (`insertRule`), by the owning
    /// `<style>` or `<link>`, as text.
    cssom: HashMap<NodeId, String>,
    viewport: (f64, f64),
    /// Computed styles and the parsed sheets behind them. See `style.rs`.
    pub(crate) style_cache: crate::style::StyleCache,
    pub(crate) layout_cache: crate::layout::LayoutCache,
}

impl Default for Document {
    fn default() -> Self {
        Document::new()
    }
}

impl Document {
    pub fn new() -> Self {
        Document {
            nodes: vec![Node::default()],
            document: 0,
            quirks: Quirks::NoQuirks,
            url: String::new(),
            version: 0,
            stylesheets: HashMap::new(),
            cssom: HashMap::new(),
            viewport: (800.0, 600.0),
            style_cache: Default::default(),
            layout_cache: Default::default(),
        }
    }

    /// Changes whenever something that styles depend on does.
    pub fn version(&self) -> u64 {
        self.version
    }

    fn touch(&mut self) {
        self.version += 1;
    }

    pub fn viewport(&self) -> (f64, f64) {
        self.viewport
    }

    pub fn set_viewport(&mut self, viewport: (f64, f64)) {
        if self.viewport != viewport {
            self.viewport = viewport;
            self.touch();
        }
    }

    /// The fetched text of the stylesheet `<link>` `id`.
    pub fn stylesheet(&self, id: NodeId) -> Option<&str> {
        self.stylesheets.get(&id).map(String::as_str)
    }

    pub fn set_stylesheet(&mut self, id: NodeId, text: String) {
        self.stylesheets.insert(id, text);
        self.touch();
    }

    /// Rules script added to `id`'s sheet through the CSSOM.
    pub fn cssom(&self, id: NodeId) -> Option<&str> {
        self.cssom.get(&id).map(String::as_str)
    }

    pub fn set_cssom(&mut self, id: NodeId, text: String) {
        self.cssom.insert(id, text);
        self.touch();
    }

    pub fn document_id(&self) -> NodeId {
        self.document
    }

    /// The URL this document was loaded from. The engine keeps it beside the
    /// tree so that script can read `location.href` and the CDP layer can
    /// report the frame's url from one place.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn set_url(&mut self, url: &str) {
        self.url = url.to_string();
    }

    pub fn set_title(&mut self, title: &str) {
        // Replace the first <title> in <head>, creating one if the document has
        // none, which is what assigning to document.title does.
        let head = self.find_tag(self.document, "head");
        let existing = head.and_then(|h| self.find_tag(h, "title"));
        match (existing, head) {
            (Some(title_node), _) => {
                for child in self.children(title_node).to_vec() {
                    self.detach(child);
                }
                let text = self.push(Node {
                    data: NodeData::Text,
                    text: title.to_string(),
                    ..Node::default()
                });
                self.append_child(title_node, text);
            }
            (None, Some(head)) => {
                let title_node = self.push(Node {
                    data: NodeData::Element { contents: None },
                    name: Some(QualName::new(
                        None,
                        ns!(html),
                        LocalName::from("title"),
                    )),
                    ..Node::default()
                });
                self.append_child(head, title_node);
                let text = self.push(Node {
                    data: NodeData::Text,
                    text: title.to_string(),
                    ..Node::default()
                });
                self.append_child(title_node, text);
            }
            (None, None) => {}
        }
    }

    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) {
        self.touch();
        if let Some(existing) = self.nodes[id].attrs.iter_mut().find(|a| a.local_name() == name) {
            existing.value = value.to_string();
            return;
        }
        let attr = Attr {
            name: QualName::new(None, ns!(html), LocalName::from(name)),
            value: value.to_string(),
        };
        self.nodes[id].attrs.push(attr);
    }

    pub fn remove_attr(&mut self, id: NodeId, name: &str) {
        self.touch();
        self.nodes[id].attrs.retain(|a| a.local_name() != name);
    }

    /// Creates a detached element. Script builds subtrees and attaches them.
    pub fn create_element(&mut self, tag: &str) -> NodeId {
        self.push(Node {
            data: NodeData::Element { contents: None },
            name: Some(QualName::new(None, ns!(html), LocalName::from(tag))),
            ..Node::default()
        })
    }

    pub fn create_text(&mut self, text: String) -> NodeId {
        self.push(Node { data: NodeData::Text, text, ..Node::default() })
    }

    /// `textContent = text`. An element loses its children and gains one text
    /// node, or none when the text is empty; a text or comment node is edited
    /// in place. The detached children stay in the arena, unreachable.
    pub fn set_text_content(&mut self, id: NodeId, text: &str) {
        self.touch();
        match self.nodes[id].data {
            NodeData::Text | NodeData::Comment => self.nodes[id].text = text.to_string(),
            NodeData::Element { .. } => {
                for child in self.nodes[id].children.clone() {
                    self.detach(child);
                }
                if !text.is_empty() {
                    let node = self.create_text(text.to_string());
                    self.append_child(id, node);
                }
            }
            NodeData::Fragment => {
                for child in self.nodes[id].children.clone() {
                    self.detach(child);
                }
                if !text.is_empty() {
                    let node = self.create_text(text.to_string());
                    self.append_child(id, node);
                }
            }
            NodeData::Document | NodeData::Doctype { .. } => {}
        }
    }

    /// Deep copy, returned detached. `cloneNode(true)` in the specification.
    pub fn clone_subtree(&mut self, id: NodeId) -> NodeId {
        self.clone_node(id, true)
    }

    /// `cloneNode(deep)`. The copy is detached and owns copies of the
    /// children, never the originals.
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> NodeId {
        let source = &self.nodes[id];
        let copy = self.push(Node {
            data: source.data.clone(),
            name: source.name.clone(),
            attrs: source.attrs.clone(),
            text: source.text.clone(),
            parent: None,
            children: Vec::new(),
            control: source.control.clone(),
        });
        if deep {
            for child in self.nodes[id].children.clone() {
                let child_copy = self.clone_node(child, true);
                self.append_child(copy, child_copy);
            }
        }
        copy
    }

    pub fn create_comment(&mut self, text: String) -> NodeId {
        self.push(Node { data: NodeData::Comment, text, ..Node::default() })
    }

    pub fn create_fragment(&mut self) -> NodeId {
        self.push(Node { data: NodeData::Fragment, ..Node::default() })
    }

    /// `createElementNS`. SVG and MathML elements keep their namespace, so
    /// `namespaceURI` reads back what script asked for.
    pub fn create_element_ns(&mut self, namespace: &str, tag: &str) -> NodeId {
        let local = tag.rsplit(':').next().unwrap_or(tag);
        self.push(Node {
            data: NodeData::Element { contents: None },
            name: Some(QualName::new(None, Namespace::from(namespace), LocalName::from(local))),
            ..Node::default()
        })
    }

    /// The element's namespace URI, empty for non-elements.
    pub fn namespace(&self, id: NodeId) -> &str {
        self.nodes[id].name.as_ref().map(|n| n.ns.as_ref()).unwrap_or("")
    }

    /// Inserts `child` into `parent` before `reference`, or last when there is
    /// no reference. A reference that is not a child of `parent` appends,
    /// rather than inserting somewhere else.
    pub fn insert_child(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) {
        self.touch();
        match reference {
            Some(r) if r != child && self.nodes[r].parent == Some(parent) => {
                self.detach(child);
                let pos = self.nodes[parent].children.iter().position(|c| *c == r).unwrap_or(0);
                self.nodes[parent].children.insert(pos, child);
                self.nodes[child].parent = Some(parent);
            }
            _ => self.append_child(parent, child),
        }
    }

    /// True when `ancestor` is `id` or one of its ancestors.
    pub fn is_inclusive_ancestor(&self, ancestor: NodeId, mut id: NodeId) -> bool {
        loop {
            if id == ancestor {
                return true;
            }
            match self.nodes[id].parent {
                Some(parent) => id = parent,
                None => return false,
            }
        }
    }

    /// Copies `id` and its subtree out of `other` into this document,
    /// detached. How a parsed fragment joins the live tree.
    pub fn import(&mut self, other: &Document, id: NodeId) -> NodeId {
        let source = other.node(id);
        let copy = self.push(Node {
            data: source.data.clone(),
            name: source.name.clone(),
            attrs: source.attrs.clone(),
            text: source.text.clone(),
            parent: None,
            children: Vec::new(),
            control: Control::default(),
        });
        for child in other.children(id).to_vec() {
            let child_copy = self.import(other, child);
            self.append_child(copy, child_copy);
        }
        copy
    }

    /// `document.write`, reduced to appending text to the body. The full
    /// specification re-opens the parser; appending is wrong in general and
    /// keeps simple pages working.
    pub fn document_write(&mut self, text: &str) {
        let Some(body) = self.find_tag(self.document, "body") else { return };
        let node = self.push(Node { data: NodeData::Text, text: text.to_string(), ..Node::default() });
        self.append_child(body, node);
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

    pub fn control(&self, id: NodeId) -> &Control {
        &self.nodes[id].control
    }

    pub fn control_mut(&mut self, id: NodeId) -> &mut Control {
        self.touch();
        &mut self.nodes[id].control
    }

    /// The current value of a form control: what was typed or assigned, else
    /// the `value` attribute. A `<textarea>`'s default is its text.
    pub fn current_value(&self, id: NodeId) -> String {
        if let Some(value) = &self.nodes[id].control.value {
            return value.clone();
        }
        if self.is_tag(id, "textarea") {
            return self.text_content(id);
        }
        self.attr(id, "value").unwrap_or("").to_string()
    }

    /// Whether a checkbox or radio is checked now.
    pub fn current_checked(&self, id: NodeId) -> bool {
        self.nodes[id].control.checked.unwrap_or_else(|| self.has_attr(id, "checked"))
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
        self.touch();
        self.detach(child);
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    /// Remove `id` from its parent, leaving it intact.
    pub(crate) fn detach(&mut self, id: NodeId) {
        self.touch();
        if let Some(parent) = self.nodes[id].parent.take() {
            if let Some(pos) = self.nodes[parent].children.iter().position(|c| *c == id) {
                self.nodes[parent].children.remove(pos);
            }
        }
    }

    /// Insert `child` immediately before `sibling`.
    pub(crate) fn insert_before(&mut self, sibling: NodeId, child: NodeId) {
        self.touch();
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
        self.touch();
        self.nodes[id].text.push_str(text);
    }

    pub(crate) fn push(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    pub(crate) fn push_attr(&mut self, id: NodeId, attr: Attr) {
        self.touch();
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

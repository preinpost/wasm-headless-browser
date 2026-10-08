//! A tiny document model.
//!
//! This is not a DOM. There is no parser, no CSS, and no tree mutation. It is
//! the smallest thing that lets a client ask "what is on this page" and get a
//! self-consistent answer, while the real engine is being written.

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub tag: &'static str,
    pub id: Option<&'static str>,
    pub class: &'static str,
    pub attrs: Vec<(&'static str, String)>,
    pub text: Option<String>,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
    pub rect: Rect,
    /// Text nodes and element nodes are different things. Conflating them
    /// makes `<h1>Title</h1>` look like a bare `StaticText` in the
    /// accessibility tree instead of a heading that contains one.
    pub text_node: bool,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    pub fn is_element(&self) -> bool {
        !self.text_node
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    pub nodes: Vec<Node>,
    pub root: usize,
    pub body: usize,
    pub title: String,
    pub url: String,
    pub viewport: (f64, f64),
    pub history: Vec<String>,
    pub history_index: usize,
}

/// The page M0 pretends to serve. It mirrors the shape of the real target:
/// a login form plus a link to follow.
pub const FAKE_TITLE: &str = "Sandbox Login";

impl Document {
    pub fn blank() -> Self {
        Document::build("about:blank", "")
    }

    pub fn for_url(url: &str) -> Self {
        let title = if url == "about:blank" { "" } else { FAKE_TITLE };
        Document::build(url, title)
    }

    fn build(url: &str, title: &str) -> Self {
        let mut doc = Document {
            nodes: Vec::new(),
            root: 0,
            body: 0,
            title: title.to_string(),
            url: url.to_string(),
            viewport: (800.0, 600.0),
            history: vec![url.to_string()],
            history_index: 0,
        };

        let html = doc.add("html", None, "", &[], None, None);
        let head = doc.add("head", None, "", &[], None, Some(html));
        doc.add("title", None, "", &[], Some(title.to_string()), Some(head));
        let body = doc.add("body", None, "", &[], None, Some(html));
        doc.root = html;
        doc.body = body;

        if url == "about:blank" {
            doc.layout();
            return doc;
        }

        doc.add("h1", None, "", &[], Some("Sandbox Login".into()), Some(body));
        doc.add(
            "p",
            None,
            "",
            &[],
            Some("Sign in to continue to the sandbox.".into()),
            Some(body),
        );
        let form = doc.add("form", Some("login"), "", &[], None, Some(body));

        doc.add("label", None, "", &[("for", "user".into())], Some("User".into()), Some(form));
        doc.add(
            "input",
            Some("user"),
            "",
            &[("type", "text".into()), ("name", "user".into()), ("value", "".into())],
            None,
            Some(form),
        );

        doc.add("label", None, "", &[("for", "pw".into())], Some("Password".into()), Some(form));
        doc.add(
            "input",
            Some("pw"),
            "",
            &[("type", "password".into()), ("name", "pw".into()), ("value", "".into())],
            None,
            Some(form),
        );

        doc.add("button", Some("submit"), "", &[("type", "submit".into())], Some("Sign in".into()), Some(form));

        doc.add("a", Some("more"), "", &[("href", "/more".into())], Some("More info".into()), Some(body));

        doc.layout();
        doc
    }

    /// Appends an element. `text` becomes a child text node, which is how a
    /// browser sees it.
    fn add(
        &mut self,
        tag: &'static str,
        id: Option<&'static str>,
        class: &'static str,
        attrs: &[(&'static str, String)],
        text: Option<String>,
        parent: Option<usize>,
    ) -> usize {
        let index = self.nodes.len();
        self.nodes.push(Node {
            tag,
            id,
            class,
            attrs: attrs.to_vec(),
            text: None,
            children: Vec::new(),
            parent,
            rect: Rect::default(),
            text_node: false,
        });
        if let Some(p) = parent {
            self.nodes[p].children.push(index);
        }
        if let Some(content) = text {
            self.push_text(content, index);
        }
        index
    }

    fn push_text(&mut self, content: String, parent: usize) {
        let index = self.nodes.len();
        self.nodes.push(Node {
            tag: "",
            id: None,
            class: "",
            attrs: Vec::new(),
            text: Some(content),
            children: Vec::new(),
            parent: Some(parent),
            rect: Rect::default(),
            text_node: true,
        });
        self.nodes[parent].children.push(index);
    }

    /// Block stacking. Text nodes inherit their parent's box. This is enough
    /// for "is it visible" and "where is its centre", which is all M0 needs.
    fn layout(&mut self) {
        let (vw, _vh) = self.viewport;
        let margin = 8.0;
        let mut y = margin;
        for i in 0..self.nodes.len() {
            if !self.nodes[i].is_element() {
                continue;
            }
            match self.nodes[i].tag {
                "html" | "head" | "title" => continue,
                _ => {}
            }
            let h = match self.nodes[i].tag {
                "h1" => 32.0,
                "p" => 24.0,
                "form" => 128.0,
                "label" => 20.0,
                "input" => 28.0,
                "button" => 32.0,
                "a" => 22.0,
                _ => 20.0,
            };
            self.nodes[i].rect = Rect { x: margin, y, w: vw - margin * 2.0, h };
            y += h + 6.0;
        }
        // Text nodes take the box of their nearest element parent.
        let parent_boxes: Vec<Option<usize>> = self.nodes.iter().map(|n| n.parent).collect();
        for i in 0..self.nodes.len() {
            if self.nodes[i].is_element() {
                continue;
            }
            if let Some(p) = parent_boxes[i] {
                self.nodes[i].rect = self.nodes[p].rect;
            }
        }
    }

    /// A deliberately small selector engine: tag, `#id`, `.class`, `*`,
    /// `tag#id`, `tag.class`, and `[attr]` / `[attr="value"]`.
    pub fn query(&self, selector: &str) -> Option<usize> {
        self.query_all(selector).into_iter().next()
    }

    pub fn query_all(&self, selector: &str) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|i| self.nodes[*i].is_element())
            .filter(|i| matches_selector(&self.nodes[*i], selector))
            .collect()
    }

    pub fn text_of(&self, index: usize) -> String {
        let mut out = String::new();
        collect_text(&self.nodes, index, &mut out);
        out.trim().to_string()
    }

    pub fn navigate(&mut self, url: &str) {
        self.url = url.to_string();
        self.title = if url == "about:blank" { String::new() } else { FAKE_TITLE.to_string() };
        self.history.truncate(self.history_index + 1);
        if self.history.last().map(String::as_str) != Some(url) {
            self.history.push(url.to_string());
            self.history_index = self.history.len() - 1;
        }
        let rebuilt = Document::build(url, &self.title);
        let history = std::mem::take(&mut self.history);
        let index = self.history_index;
        *self = rebuilt;
        self.history = history;
        self.history_index = index;
    }

    pub fn go_back(&mut self) -> bool {
        if self.history_index == 0 {
            return false;
        }
        self.history_index -= 1;
        let url = self.history[self.history_index].clone();
        self.navigate_without_history(&url);
        true
    }

    pub fn go_forward(&mut self) -> bool {
        if self.history_index + 1 >= self.history.len() {
            return false;
        }
        self.history_index += 1;
        let url = self.history[self.history_index].clone();
        self.navigate_without_history(&url);
        true
    }

    fn navigate_without_history(&mut self, url: &str) {
        let history = self.history.clone();
        let index = self.history_index;
        let rebuilt = Document::build(url, if url == "about:blank" { "" } else { FAKE_TITLE });
        *self = rebuilt;
        self.history = history;
        self.history_index = index;
    }
}

fn collect_text(nodes: &[Node], index: usize, out: &mut String) {
    if let Some(t) = &nodes[index].text {
        out.push_str(t);
        out.push(' ');
    }
    for child in nodes[index].children.clone() {
        collect_text(nodes, child, out);
    }
}

fn matches_selector(node: &Node, selector: &str) -> bool {
    let selector = selector.trim();
    if selector.is_empty() {
        return false;
    }
    if selector == "*" {
        return true;
    }

    // Attribute selectors: [attr] or [attr="value"]
    if selector.starts_with('[') && selector.ends_with(']') {
        let inner = &selector[1..selector.len() - 1];
        return match inner.split_once('=') {
            Some((k, v)) => node.attr(k.trim()).map(|got| got == v.trim().trim_matches('"')) == Some(true),
            None => node.attr(inner.trim()).is_some(),
        };
    }

    let bytes = selector.as_bytes();
    let mut i = 0;

    // Optional leading tag name.
    while i < bytes.len() && bytes[i] != b'#' && bytes[i] != b'.' {
        i += 1;
    }
    let tag = &selector[..i];
    if !tag.is_empty() && node.tag != tag {
        return false;
    }

    // Any sequence of #id and .class parts.
    while i < bytes.len() {
        let marker = bytes[i];
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != b'#' && bytes[i] != b'.' {
            i += 1;
        }
        let part = &selector[start..i];
        match marker {
            b'#' => {
                if node.id != Some(part) {
                    return false;
                }
            }
            b'.' => {
                if !node.class.split(' ').any(|c| c == part) {
                    return false;
                }
            }
            _ => return false,
        }
    }

    true
}

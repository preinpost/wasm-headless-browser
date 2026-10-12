//! A private rendering projection. Generated boxes never enter the live DOM,
//! selector matching, accessibility tree or script-visible node handles.
//! Original node IDs are retained; generated IDs map back to their owner for
//! pointer input. The projection is cached with layout, not rebuilt by paint.

use std::collections::HashMap;

use crate::dom::{Document, NodeData, NodeId};
use crate::style::Styles;

#[derive(Debug)]
pub(crate) struct RenderTree {
    pub doc: Document,
    pub styles: Styles,
    pub owners: HashMap<NodeId, NodeId>,
    pub text_sources: HashMap<NodeId, TextSource>,
}

#[derive(Debug)]
pub(crate) struct TextSource {
    pub source: NodeId,
    pub parent: NodeId,
    pub first: bool,
}

impl RenderTree {
    pub fn new(source: &Document, styles: &Styles) -> Self {
        let mut tree = Self { doc: source.clone(), styles: styles.clone(), owners: HashMap::new(), text_sources: HashMap::new() };
        for id in source.descendants(source.document_id()) {
            if !source.is_element(id) || styles.display_none(source, id)
                || source.namespace(id) != "http://www.w3.org/1999/xhtml"
                || matches!(source.tag(id), Some("img" | "input" | "textarea" | "select" | "iframe" | "script" | "style" | "video" | "canvas")) {
                continue;
            }
            for name in ["before", "after"] {
                let Some(style) = styles.pseudo(id, name) else { continue };
                if style.get("display") == "none" { continue; }
                let Some(content) = generated_content(style.get("content"), source, id) else { continue };
                let generated = tree.doc.create_element("whb-pseudo");
                tree.styles.insert(generated, style.clone());
                tree.owners.insert(generated, id);
                if !content.is_empty() {
                    let text = tree.doc.create_text(content);
                    tree.doc.append_child(generated, text);
                    tree.owners.insert(text, id);
                }
                let before = if name == "before" { tree.doc.children(id).first().copied() } else { None };
                tree.doc.insert_child(id, generated, before);
            }
        }
        tree.fragment_text();
        tree
    }

    /// Words and CJK break opportunities remain separate inline items, so
    /// a run can wrap across nested inline elements instead of assigning
    /// each element an independent flex row. Source text is kept intact for
    /// secret scrubbing before any piece is painted.
    fn fragment_text(&mut self) {
        let texts: Vec<_> = self.doc.descendants(self.doc.document_id())
            .filter(|id| matches!(self.doc.node(*id).data, NodeData::Text)).collect();
        for source in texts {
            let Some(parent) = self.doc.parent(source) else { continue };
            let Some(style) = self.styles.get(parent).cloned() else { continue };
            if self.styles.display_none(&self.doc, parent) || self.doc.namespace(parent) != "http://www.w3.org/1999/xhtml"
                || matches!(style.get("white-space"), "pre" | "pre-wrap" | "pre-line" | "break-spaces")
                || std::iter::successors(Some(parent), |id| self.doc.parent(*id))
                    .any(|id| matches!(self.doc.tag(id), Some("script" | "style" | "textarea" | "select"))) {
                continue;
            }
            let original = self.doc.node(source).text.clone();
            if original.trim().is_empty() && matches!(style.get("display"), "flex" | "inline-flex" | "grid" | "inline-grid") {
                self.doc.detach(source);
                continue;
            }
            let pieces = words(&original, style.get("word-break"), style.get("overflow-wrap"), style.get("white-space") == "nowrap");
            let owner = self.owners.get(&parent).copied().unwrap_or(parent);
            let mut text_style = style.inherited_text().with_value("display", "inline-block")
                .with_value("white-space", "pre").with_value("text-align", "left").with_value("text-indent", "0px");
            // Decorations propagate through inline descendants, even though
            // text-decoration itself is not an inherited CSS property.
            for name in ["text-decoration", "text-decoration-line"] {
                if let Some(value) = std::iter::successors(Some(parent), |id| self.doc.parent(*id))
                    .filter_map(|id| self.styles.get(id)).map(|s| s.get(name))
                    .find(|v| !matches!(*v, "" | "none")) {
                    text_style = text_style.with_value(name, value);
                }
            }
            for (n, piece) in pieces.into_iter().enumerate() {
                let fragment = self.doc.create_element("whb-text");
                let text = self.doc.create_text(piece);
                self.doc.append_child(fragment, text);
                self.doc.insert_child(parent, fragment, Some(source));
                self.styles.insert(fragment, text_style.clone());
                self.owners.insert(fragment, owner);
                self.text_sources.insert(fragment, TextSource { source, parent, first: n == 0 });
            }
            self.doc.detach(source);
        }
    }
}

/// Collapse HTML whitespace without discarding spaces between adjacent
/// text/inline nodes. A trailing space travels with its word; CJK permits
/// breaks between characters unless `keep-all` was requested.
fn words(text: &str, word_break: &str, overflow_wrap: &str, nowrap: bool) -> Vec<String> {
    let mut normalized = String::new();
    let mut previous_space = false;
    for c in text.chars() {
        let space = matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
        if space { if !previous_space { normalized.push(' '); } }
        else { normalized.push(c); }
        previous_space = space;
    }
    if nowrap { return (!normalized.is_empty()).then_some(normalized).into_iter().collect(); }
    let cjk = |c: char| matches!(c as u32, 0x2e80..=0xa4cf | 0xac00..=0xd7af | 0xf900..=0xfaff | 0x20000..=0x2ffff);
    let break_all = word_break == "break-all" || overflow_wrap == "anywhere";
    let mut pieces: Vec<String> = Vec::new();
    let mut word = String::new();
    for c in normalized.chars() {
        if c == ' ' {
            if !word.is_empty() { word.push(c); pieces.push(std::mem::take(&mut word)); }
            else if let Some(last) = pieces.last_mut() { last.push(c); }
            else { pieces.push(" ".into()); }
        } else if break_all || (cjk(c) && word_break != "keep-all") {
            if !word.is_empty() { pieces.push(std::mem::take(&mut word)); }
            pieces.push(c.to_string());
        } else {
            word.push(c);
            if c == '-' { pieces.push(std::mem::take(&mut word)); }
        }
    }
    if !word.is_empty() { pieces.push(word); }
    pieces
}

/// Textual generated content: quoted strings, CSS escapes and attr().
/// Unsupported counters and content images do not create misleading text.
fn generated_content(value: &str, doc: &Document, owner: NodeId) -> Option<String> {
    let mut rest = value.trim();
    if rest.is_empty() || matches!(rest, "none" | "normal") { return None; }
    let mut text = String::new();
    let mut specified = false;
    while !rest.is_empty() {
        if rest.starts_with('/') { break; } // Optional accessible alternative.
        if let Some(quote @ ('\'' | '"')) = rest.chars().next() {
            let mut chars = rest[1..].char_indices().peekable();
            let mut closed = None;
            while let Some((at, c)) = chars.next() {
                if c == quote { closed = Some(at + 2); break; }
                if c == '\\' {
                    let (_, escaped) = chars.next()?;
                    if escaped.is_ascii_hexdigit() {
                        let mut hex = String::from(escaped);
                        while hex.len() < 6 && chars.peek().is_some_and(|(_, c)| c.is_ascii_hexdigit()) {
                            hex.push(chars.next()?.1);
                        }
                        if chars.peek().is_some_and(|(_, c)| c.is_ascii_whitespace()) { chars.next(); }
                        text.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?).filter(|c| *c != '\0').unwrap_or('\u{fffd}'));
                    } else if escaped != '\n' && escaped != '\r' {
                        text.push(escaped);
                    }
                } else { text.push(c); }
            }
            rest = rest.get(closed? ..)?.trim_start();
            specified = true;
        } else if let Some(inner) = rest.strip_prefix("attr(") {
            let end = inner.find(')')?;
            let name = inner[..end].trim();
            text.push_str(doc.attr(owner, name).unwrap_or(""));
            rest = inner[end + 1..].trim_start();
            specified = true;
        } else { return None; }
    }
    specified.then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_html;
    use crate::style::document_styles;

    #[test]
    fn generated_boxes_do_not_mutate_the_document_or_match_its_selectors() {
        let doc = parse_html("<style>#a:before{content:'[';color:red} #a::after{content:attr(data-label)}</style><a id=a data-label=']'>link</a>");
        let original = crate::browser::serialize(&doc, doc.document_id());
        let styles = document_styles(&doc);
        let tree = RenderTree::new(&doc, &styles);
        let owner = doc.find_id(doc.document_id(), "a").unwrap();
        assert_eq!(crate::browser::serialize(&doc, doc.document_id()), original);
        assert_eq!(doc.text_content(owner), "link");
        assert_eq!(tree.doc.text_content(owner), "[link]");
        assert!(tree.owners.values().filter(|id| **id == owner).count() >= 4);
        assert!(crate::select::query_all(&doc, doc.document_id(), "a::before").is_empty());
    }

    #[test]
    fn content_strings_keep_empty_values_and_decode_css_escapes() {
        let doc = parse_html("<div id=a data-label='value'></div>");
        let id = doc.find_id(doc.document_id(), "a").unwrap();
        assert_eq!(generated_content("\"\"", &doc, id), Some(String::new()));
        assert_eq!(generated_content("'\\41 ' attr(data-label) '!'", &doc, id).as_deref(), Some("Avalue!"));
        assert_eq!(generated_content("none", &doc, id), None);
        assert_eq!(generated_content("counter(item)", &doc, id), None);
    }
}

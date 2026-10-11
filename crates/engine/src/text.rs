//! `innerText`: an element's text as it is rendered, which is what a person
//! reads and what an agent usually wants, as HTML's "rendered text
//! collection steps" define it, from the computed styles.
//!
//! Left out: what is not rendered (`display: none`, which covers `<script>`,
//! `<style>` and `<head>` by the user-agent sheet) and text whose
//! `visibility` hides it. Whitespace collapses as `white-space` says, and
//! spaces at the start and end of a line go. Block-level boxes are set on
//! lines of their own, paragraphs with a blank line between, `<br>` breaks
//! the line, table cells are separated by tabs and rows by line breaks.
//! `text-transform` is applied.
//!
//! An element that is not being rendered gives its `textContent`, as the
//! specification says.

use crate::dom::{Document, NodeData, NodeId};
use crate::style::{ComputedStyle, Styles};

/// How a text node's whitespace is handled, from its `white-space`.
#[derive(Clone, Copy, PartialEq)]
enum Space {
    /// `normal`, `nowrap`: runs of whitespace become one space.
    Collapse,
    /// `pre-line`: spaces collapse, line breaks stay.
    PreLine,
    /// `pre`, `pre-wrap`, `break-spaces`: as written.
    Preserve,
}

/// The text being assembled, with the line breaks still owed to it.
#[derive(Default)]
struct Out {
    text: String,
    /// Required line breaks before whatever comes next; the larger request
    /// wins, and none are written at the very start or end.
    pending: usize,
    /// The last character written is a collapsible space, which a line
    /// break removes.
    soft_space: bool,
    /// An inline object such as an image sits on the line since the last
    /// text, so a space after it is not at the start of the line.
    after_object: bool,
}

impl Out {
    fn at_line_start(&self) -> bool {
        !self.after_object && (self.text.is_empty() || self.text.ends_with('\n'))
    }

    /// An image, a control or the like: no text, but content on the line.
    fn object(&mut self) {
        self.settle();
        self.after_object = true;
    }

    fn require_breaks(&mut self, n: usize) {
        self.pending = self.pending.max(n);
    }

    /// Writes the breaks owed, before anything visible.
    fn settle(&mut self) {
        if self.pending > 0 && !self.text.is_empty() {
            self.drop_soft_space();
            let have = self.text.chars().rev().take_while(|c| *c == '\n').count();
            for _ in have..self.pending {
                self.text.push('\n');
            }
        }
        self.pending = 0;
        self.after_object = false;
    }

    fn drop_soft_space(&mut self) {
        if self.soft_space && self.text.ends_with(' ') {
            self.text.pop();
        }
        self.soft_space = false;
    }

    /// A string written as it is, such as a tab between table cells.
    fn literal(&mut self, s: &str) {
        self.settle();
        if s == "\n" {
            self.drop_soft_space();
        }
        self.text.push_str(s);
        self.soft_space = false;
    }

    fn text(&mut self, raw: &str, space: Space) {
        match space {
            Space::Preserve => {
                if raw.is_empty() {
                    return;
                }
                self.settle();
                self.text.push_str(raw);
                self.soft_space = false;
            }
            Space::Collapse | Space::PreLine => {
                let lines: Vec<&str> = if space == Space::PreLine { raw.split('\n').collect() } else { vec![raw] };
                for (n, line) in lines.into_iter().enumerate() {
                    if n > 0 {
                        self.literal("\n");
                    }
                    let mut gap = false;
                    for c in line.chars() {
                        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c') {
                            gap = true;
                            continue;
                        }
                        if gap {
                            self.space();
                            gap = false;
                        }
                        self.settle();
                        self.text.push(c);
                        self.soft_space = false;
                    }
                    if gap {
                        self.space();
                    }
                }
            }
        }
    }

    /// One collapsible space, unless the line has just started or a space
    /// is already there.
    fn space(&mut self) {
        if self.pending > 0 || self.at_line_start() || self.soft_space || self.text.ends_with(' ') {
            return;
        }
        self.text.push(' ');
        self.soft_space = true;
    }

    fn finish(mut self) -> String {
        self.drop_soft_space();
        self.text
    }
}

fn space_of(style: &ComputedStyle) -> Space {
    match style.get("white-space") {
        "pre" | "pre-wrap" | "break-spaces" => Space::Preserve,
        "pre-line" => Space::PreLine,
        _ => Space::Collapse,
    }
}

fn transform(text: &str, style: &ComputedStyle) -> String {
    match style.get("text-transform") {
        "uppercase" => text.to_uppercase(),
        "lowercase" => text.to_lowercase(),
        "capitalize" => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphanumeric() {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
                start = c.is_whitespace();
            }
            out
        }
        _ => text.to_string(),
    }
}

/// Whether an element and every ancestor is rendered: styled, and none
/// `display: none`.
fn is_rendered(doc: &Document, styles: &Styles, id: NodeId) -> bool {
    let mut node = Some(id);
    while let Some(current) = node.filter(|n| doc.is_element(*n)) {
        match styles.get(current) {
            Some(style) if style.get("display") != "none" => {}
            _ => return false,
        }
        node = doc.parent(current);
    }
    true
}

fn is_block_level(display: &str) -> bool {
    matches!(
        display.split_whitespace().next().unwrap_or(""),
        "block" | "flex" | "grid" | "list-item" | "table" | "flow-root" | "table-caption" | "flow"
    )
}

/// The next element sibling of `id` that displays as `display`.
fn has_later(doc: &Document, styles: &Styles, id: NodeId, display: &str) -> bool {
    let Some(parent) = doc.parent(id) else { return false };
    doc.children(parent)
        .iter()
        .skip_while(|c| **c != id)
        .skip(1)
        .any(|c| styles.get(*c).is_some_and(|s| s.get("display") == display))
}

fn collect(doc: &Document, styles: &Styles, id: NodeId, out: &mut Out) {
    match &doc.node(id).data {
        NodeData::Text => {
            let Some(style) = doc.parent(id).and_then(|p| styles.get(p)) else { return };
            if matches!(style.get("visibility"), "hidden" | "collapse") {
                return;
            }
            let text = transform(&doc.node(id).text, style);
            out.text(&text, space_of(style));
        }
        NodeData::Element { .. } => {
            let Some(style) = styles.get(id) else { return };
            let display = style.get("display");
            if display == "none" {
                return;
            }
            if doc.is_tag(id, "br") {
                out.literal("\n");
                return;
            }
            const OBJECTS: &[&str] = &["img", "input", "textarea", "video", "audio", "canvas", "iframe", "embed", "object", "svg"];
            if OBJECTS.iter().any(|t| doc.is_tag(id, t)) && !is_block_level(display) {
                out.object();
                return;
            }
            let breaks = if doc.is_tag(id, "p") {
                2
            } else if is_block_level(display) {
                1
            } else {
                0
            };
            out.require_breaks(breaks);
            for child in doc.children(id).to_vec() {
                collect(doc, styles, child, out);
            }
            out.require_breaks(breaks);
            if display == "table-cell" && has_later(doc, styles, id, "table-cell") {
                out.literal("\t");
            }
            if display == "table-row" {
                out.require_breaks(1);
            }
        }
        _ => {
            for child in doc.children(id).to_vec() {
                collect(doc, styles, child, out);
            }
        }
    }
}

/// `element.innerText`.
pub fn inner_text(doc: &Document, id: NodeId) -> String {
    let styles = crate::style::document_styles(doc);
    if !is_rendered(doc, &styles, id) {
        return doc.text_content(id);
    }
    let mut out = Out::default();
    for child in doc.children(id).to_vec() {
        collect(doc, &styles, child, &mut out);
    }
    out.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_html;

    fn text(html: &str) -> String {
        let doc = parse_html(html);
        let body = doc.descendants(doc.document_id()).find(|n| doc.is_tag(*n, "body")).unwrap();
        inner_text(&doc, body)
    }

    #[test]
    fn it_is_the_text_a_person_reads() {
        assert_eq!(
            text("<body>\n  <h1> Title </h1>\n  <script>var x = 1;</script><style>p{}</style>\n  <p>One   two\nthree</p><p>Para</p>\n  <div>a<span> b </span>c</div></body>"),
            "Title\n\nOne two three\n\nPara\n\na b c"
        );
        assert_eq!(text("<body>line<br>next <b>bold</b>, <i>it</i>.</body>"), "line\nnext bold, it.");
        assert_eq!(text("<body><div style='display:none'>hidden</div><div style='visibility:hidden'>x<span style='visibility:visible'>shown</span></div></body>"), "shown");
        assert_eq!(text("<body><pre>  a\n    b</pre><div style='text-transform:uppercase'>up</div></body>"), "  a\n    b\nUP");
        assert_eq!(text("<body><ul><li>one</li><li>two</li></ul></body>"), "one\ntwo");
        // An image is content on its line: the space after it stays.
        assert_eq!(text("<body><div><img src=x> after</div><div> before</div></body>"), " after\nbefore");
        assert_eq!(text("<body><select><option>a</option><option>b</option></select></body>"), "a\nb");
    }

    #[test]
    fn tables_are_tabs_and_lines() {
        assert_eq!(
            text("<body><table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table></body>"),
            "a\tb\nc\td"
        );
    }

    #[test]
    fn something_not_rendered_gives_its_text_content() {
        let doc = parse_html("<body><div id=h style='display:none'><p>a</p><p>b</p></div></body>");
        let hidden = doc.find_id(doc.document_id(), "h").unwrap();
        assert_eq!(inner_text(&doc, hidden), "ab");
    }
}

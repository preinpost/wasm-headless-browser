//! Scrolling: the viewport's offset and those of scroll containers.
//!
//! Offsets live in the document (`Document::scroll_offset`), keyed by the
//! container's node, the document node standing for the viewport. Layout
//! does not see them: boxes stay where they were laid out, and
//! `Layout::client_rect` subtracts the offsets of every container around a
//! box, and the viewport's unless it is fixed. So scrolling costs no layout,
//! and hit testing, `getBoundingClientRect`, `DOM.getBoxModel` and painting
//! agree on where things are.
//!
//! A scroll container is an element whose `overflow` is not `visible`; any
//! of them can be scrolled by script, and those with `auto` or `scroll` by
//! the wheel too. Not done: `scroll-behavior: smooth` (scrolling is always
//! instant), `position: sticky`, scroll snapping, and scrollbars, which take
//! no room.

use crate::dom::{Document, NodeId};
use crate::layout::{self, Layout, Rect};
use crate::style::{self, Styles};

/// A container's scrollable size and its visible part, CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sizes {
    pub scroll_width: f64,
    pub scroll_height: f64,
    pub client_width: f64,
    pub client_height: f64,
}

impl Sizes {
    fn max_offset(&self) -> (f64, f64) {
        ((self.scroll_width - self.client_width).max(0.0), (self.scroll_height - self.client_height).max(0.0))
    }
}

/// The padding box, where a container shows its content: the border box
/// less its borders.
fn padding_box(styles: &Styles, rect: Rect, id: NodeId) -> Rect {
    let [top, right, bottom, left] = styles.get(id).map(crate::paint::border_widths).unwrap_or([0.0; 4]);
    Rect {
        x: rect.x + left,
        y: rect.y + top,
        width: (rect.width - left - right).max(0.0),
        height: (rect.height - top - bottom).max(0.0),
    }
}

/// The sizes of the viewport (for the document node) or of an element.
pub fn sizes(doc: &Document, layout: &Layout, styles: &Styles, id: NodeId) -> Option<Sizes> {
    if id == doc.document_id() {
        let (vw, vh) = doc.viewport();
        let (cw, ch) = layout.content_size((vw, vh));
        return Some(Sizes { scroll_width: cw, scroll_height: ch, client_width: vw, client_height: vh });
    }
    let inner = padding_box(styles, layout.rect(id)?, id);
    let (reach_x, reach_y) = layout.extent(id).unwrap_or((0.0, 0.0));
    Some(Sizes {
        scroll_width: inner.width.max(reach_x - inner.x),
        scroll_height: inner.height.max(reach_y - inner.y),
        client_width: inner.width,
        client_height: inner.height,
    })
}

/// Whether `id` is a scroll container: the viewport, or an element whose
/// overflow clips.
fn is_container(doc: &Document, styles: &Styles, id: NodeId) -> bool {
    id == doc.document_id()
        || styles.get(id).is_some_and(|s| s.get("overflow-x") != "visible" || s.get("overflow-y") != "visible")
}

/// Scrolls `id` to `offset`, kept within what it can scroll. Returns
/// whether anything moved.
pub fn scroll_to(doc: &mut Document, id: NodeId, offset: (f64, f64)) -> bool {
    let layout = layout::document_layout(doc);
    let styles = style::document_styles(doc);
    if !is_container(doc, &styles, id) {
        return false;
    }
    let Some(sizes) = sizes(doc, &layout, &styles, id) else { return false };
    let (max_x, max_y) = sizes.max_offset();
    let clamped = (offset.0.clamp(0.0, max_x).round(), offset.1.clamp(0.0, max_y).round());
    doc.set_scroll_offset(id, clamped)
}

/// How `scrollIntoView` lines a target up in a box, on one axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Align {
    Start,
    Center,
    End,
    Nearest,
    /// `scrollIntoViewIfNeeded`: leave it if it is wholly visible, else
    /// centre it.
    IfNeeded,
}

impl Align {
    pub fn parse(value: &str, default: Align) -> Align {
        match value {
            "start" => Align::Start,
            "center" => Align::Center,
            "end" => Align::End,
            "nearest" => Align::Nearest,
            "if-needed" => Align::IfNeeded,
            _ => default,
        }
    }

    /// How far to scroll so `[start, start + size)` sits as asked in
    /// `[view, view + extent)`.
    fn delta(self, start: f64, size: f64, view: f64, extent: f64) -> f64 {
        let end = start + size;
        let visible = start >= view && end <= view + extent;
        match self {
            Align::Start => start - view,
            Align::End => end - (view + extent),
            Align::Center => (start + size / 2.0) - (view + extent / 2.0),
            Align::IfNeeded if visible => 0.0,
            Align::IfNeeded => (start + size / 2.0) - (view + extent / 2.0),
            Align::Nearest if visible => 0.0,
            Align::Nearest if start < view || size > extent => start - view,
            Align::Nearest => end - (view + extent),
        }
    }
}

/// Scrolls every container around `id`, innermost first, then the viewport,
/// so that it is seen as `block` (vertically) and `inline` (horizontally)
/// ask. Returns the containers that moved, for their `scroll` events.
pub fn into_view(doc: &mut Document, id: NodeId, block: Align, inline: Align) -> Vec<NodeId> {
    let layout = layout::document_layout(doc);
    let styles = style::document_styles(doc);
    let mut containers: Vec<NodeId> = layout.clips_of(id).iter().rev().copied().collect();
    if !layout.is_fixed(id) {
        containers.push(doc.document_id());
    }
    let mut moved = Vec::new();
    for container in containers {
        let Some(target) = layout.client_rect(doc, id) else { break };
        let view = if container == doc.document_id() {
            let (vw, vh) = doc.viewport();
            Rect { x: 0.0, y: 0.0, width: vw, height: vh }
        } else {
            match layout.client_rect(doc, container) {
                Some(rect) => padding_box(&styles, rect, container),
                None => continue,
            }
        };
        let dx = inline.delta(target.x, target.width, view.x, view.width);
        let dy = block.delta(target.y, target.height, view.y, view.height);
        let (sx, sy) = doc.scroll_offset(container);
        if (dx != 0.0 || dy != 0.0) && scroll_to(doc, container, (sx + dx, sy + dy)) {
            moved.push(container);
        }
    }
    moved
}

/// The wheel at a viewport point: the innermost container under it that
/// the user may scroll (`overflow: auto` or `scroll`) and that can still
/// move that way, else the viewport, unless the page hid its overflow.
/// Returns what moved.
pub fn wheel(doc: &mut Document, x: f64, y: f64, dx: f64, dy: f64) -> Option<NodeId> {
    let layout = layout::document_layout(doc);
    let styles = style::document_styles(doc);
    let user_scrollable = |value: &str| matches!(value, "auto" | "scroll" | "overlay");
    let hit = layout.hit_test(doc, &styles, x, y);
    let mut node = hit;
    while let Some(id) = node.filter(|id| doc.is_element(*id)) {
        if let (Some(style), Some(sizes)) = (styles.get(id), sizes(doc, &layout, &styles, id)) {
            let (max_x, max_y) = sizes.max_offset();
            let (sx, sy) = doc.scroll_offset(id);
            let can_x = user_scrollable(style.get("overflow-x")) && ((dx > 0.0 && sx < max_x) || (dx < 0.0 && sx > 0.0));
            let can_y = user_scrollable(style.get("overflow-y")) && ((dy > 0.0 && sy < max_y) || (dy < 0.0 && sy > 0.0));
            if (can_x || can_y) && scroll_to(doc, id, (sx + dx, sy + dy)) {
                return Some(id);
            }
        }
        node = doc.parent(id);
    }
    // The viewport, unless the root or the body hides overflow, which
    // propagates to it.
    let root = doc.children(doc.document_id()).iter().copied().find(|c| doc.is_element(*c));
    let body = root.and_then(|r| doc.children(r).iter().copied().find(|c| doc.is_tag(*c, "body")));
    let hidden = [root, body].into_iter().flatten().any(|id| styles.get(id).is_some_and(|s| s.get("overflow-y") == "hidden"));
    let page = doc.document_id();
    let (sx, sy) = doc.scroll_offset(page);
    (!hidden && scroll_to(doc, page, (sx + dx, sy + dy))).then_some(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_html;

    fn page(html: &str) -> Document {
        let mut doc = parse_html(html);
        doc.set_viewport((200.0, 100.0));
        doc
    }

    fn client(doc: &Document, id: &str) -> Rect {
        let node = doc.find_id(doc.document_id(), id).unwrap();
        layout::document_layout(doc).client_rect(doc, node).unwrap()
    }

    #[test]
    fn the_page_scrolls_within_its_content_and_fixed_boxes_stay() {
        let mut doc = page(
            "<body style='margin:0'><div id=a style='height:300px'></div><div id=b style='height:20px'></div>\
             <div id=f style='position:fixed;top:0;height:10px;width:10px'></div></body>",
        );
        let root = doc.document_id();
        assert!(scroll_to(&mut doc, root, (0.0, 150.0)));
        assert_eq!(client(&doc, "b").y, 150.0);
        assert_eq!(client(&doc, "f").y, 0.0, "fixed");
        // 320px of content in a 100px viewport: at most 220.
        scroll_to(&mut doc, root, (0.0, 1000.0));
        assert_eq!(doc.scroll_offset(root), (0.0, 220.0));
        assert!(!scroll_to(&mut doc, root, (0.0, 220.0)), "nothing moved");
    }

    #[test]
    fn containers_scroll_their_content_and_into_view_reaches_through_them() {
        let mut doc = page(
            "<body style='margin:0'><div style='height:150px'></div>\
             <div id=box style='height:50px;overflow:auto'><div style='height:200px'></div><p id=t style='margin:0;height:10px'></p></div></body>",
        );
        let target = doc.find_id(doc.document_id(), "t").unwrap();
        let moved = into_view(&mut doc, target, Align::Start, Align::Nearest);
        let container = doc.find_id(doc.document_id(), "box").unwrap();
        assert_eq!(moved, vec![container, doc.document_id()]);
        assert_eq!(client(&doc, "t").y, 0.0, "at the top of the viewport");
        let sizes = sizes(&doc, &layout::document_layout(&doc), &style::document_styles(&doc), container).unwrap();
        assert_eq!((sizes.client_height, sizes.scroll_height), (50.0, 210.0));
    }

    #[test]
    fn the_wheel_scrolls_the_innermost_container_that_can_move() {
        let mut doc = page(
            "<body style='margin:0'><div id=box style='height:50px;overflow:auto'><div style='height:80px'>x</div></div>\
             <div style='height:300px'></div></body>",
        );
        let container = doc.find_id(doc.document_id(), "box").unwrap();
        assert_eq!(wheel(&mut doc, 10.0, 10.0, 0.0, 100.0), Some(container));
        assert_eq!(doc.scroll_offset(container), (0.0, 30.0));
        // At its end, the page takes over.
        assert_eq!(wheel(&mut doc, 10.0, 10.0, 0.0, 100.0), Some(doc.document_id()));
        assert_eq!(doc.scroll_offset(doc.document_id()), (0.0, 100.0));
    }
}

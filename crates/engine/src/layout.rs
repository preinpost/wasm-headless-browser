//! Layout: a box for every rendered element, from the cascade's styles.
//!
//! taffy lays out block, flex, grid and floats. What it does not have is
//! approximated around it:
//!
//!   * **Inline flow.** A run of inline-level children becomes an anonymous
//!     wrapping row, and each inline element a wrapping row of its own. Text
//!     breaks between words, not mid-run, and baselines are not aligned.
//!   * **Text.** There are no fonts, so a character is measured from the font
//!     size: about half an em for Latin, a full em for Hangul, CJK and other
//!     wide characters.
//!   * **Replaced elements.** Controls, images and frames take Chrome's
//!     default sizes unless styled. Images are not fetched, so an unsized
//!     image is empty.
//!   * **Containing blocks.** An absolutely positioned box is placed in its
//!     nearest positioned ancestor, and a fixed one in the viewport.
//!   * **calc().** A mix of percentages and lengths keeps the percentage.
//!
//! The aim is consistency rather than pixels: the box an element reports is
//! where a click lands on it, and what overlaps what is decided in paint
//! order.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use taffy::prelude::*;
use taffy::{BoxSizing, Clear, Float, GridTemplateComponent, MaxTrackSizingFunction, RepetitionCount, MinTrackSizingFunction, Overflow, TrackSizingFunction};
use taffy::style_helpers;

use crate::dom::{Document, NodeData, NodeId};
use crate::style::{self, ComputedStyle, Styles};

/// A box in CSS pixels, relative to the top left of the page.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// What units resolve against.
#[derive(Clone, Copy)]
struct Basis {
    font: f64,
    root_font: f64,
    viewport: (f64, f64),
}

/// A CSS length as taffy's length-or-percentage, or `None` for `auto` and
/// anything not understood.
fn length(value: &str, basis: Basis) -> Option<LengthPercentage> {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix("calc(").and_then(|v| v.strip_suffix(')')) {
        return calc(inner, basis);
    }
    if let Some(p) = value.strip_suffix('%') {
        return p.trim().parse::<f32>().ok().map(|p| LengthPercentage::percent(p / 100.0));
    }
    px(value, basis).map(|v| LengthPercentage::length(v as f32))
}

/// A length in px, from absolute units, font-relative units and viewport
/// units. Not percentages.
fn px(value: &str, basis: Basis) -> Option<f64> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    for (unit, factor) in [
        ("vw", basis.viewport.0 / 100.0),
        ("vh", basis.viewport.1 / 100.0),
        ("vmin", basis.viewport.0.min(basis.viewport.1) / 100.0),
        ("vmax", basis.viewport.0.max(basis.viewport.1) / 100.0),
        ("dvh", basis.viewport.1 / 100.0),
        ("svh", basis.viewport.1 / 100.0),
        ("lvh", basis.viewport.1 / 100.0),
    ] {
        if let Some(n) = lower.strip_suffix(unit) {
            if let Ok(n) = n.trim().parse::<f64>() {
                return Some(n * factor);
            }
        }
    }
    if value.ends_with('%') {
        return None;
    }
    style::length_px(value, basis.font, basis.root_font, 0.0)
}

/// `calc()` with `+`/`-` between terms. Lengths are summed; when
/// percentages are present, they win and the lengths are dropped, which
/// keeps `calc(100% - 20px)` close to right.
fn calc(inner: &str, basis: Basis) -> Option<LengthPercentage> {
    let mut lengths = 0.0;
    let mut percent = 0.0;
    let mut has_percent = false;
    let mut sign = 1.0;
    for token in inner.split_whitespace() {
        match token {
            "+" => sign = 1.0,
            "-" => sign = -1.0,
            t => {
                let t = t.trim_start_matches('(').trim_end_matches(')');
                if let Some(p) = t.strip_suffix('%') {
                    percent += sign * p.parse::<f64>().ok()?;
                    has_percent = true;
                } else {
                    lengths += sign * px(t, basis)?;
                }
                sign = 1.0;
            }
        }
    }
    Some(if has_percent {
        LengthPercentage::percent((percent / 100.0) as f32)
    } else {
        LengthPercentage::length(lengths as f32)
    })
}

fn dimension(value: &str, basis: Basis) -> Dimension {
    length(value, basis).map(Dimension::from).unwrap_or(Dimension::auto())
}

fn length_auto(value: &str, basis: Basis) -> LengthPercentageAuto {
    length(value, basis).map(LengthPercentageAuto::from).unwrap_or(LengthPercentageAuto::auto())
}

fn length_zero(value: &str, basis: Basis) -> LengthPercentage {
    length(value, basis).unwrap_or(LengthPercentage::length(0.0))
}

/// A border width: none unless a style is set, as CSS computes it.
fn border(style: &ComputedStyle, side: &str, basis: Basis) -> LengthPercentage {
    if matches!(style.get(&format!("border-{side}-style")), "none" | "hidden" | "") {
        return LengthPercentage::length(0.0);
    }
    match style.get(&format!("border-{side}-width")) {
        "thin" => LengthPercentage::length(1.0),
        "medium" | "" => LengthPercentage::length(3.0),
        "thick" => LengthPercentage::length(5.0),
        other => length_zero(other, basis),
    }
}

/// The used line height in px.
pub(crate) fn line_height(style: &ComputedStyle) -> f64 {
    let font = style.font_size();
    let value = style.get("line-height").trim();
    if value == "normal" || value.is_empty() {
        return font * 1.2;
    }
    if let Ok(n) = value.parse::<f64>() {
        return n * font;
    }
    if let Some(p) = value.strip_suffix('%') {
        return p.parse::<f64>().map(|p| p / 100.0 * font).unwrap_or(font * 1.2);
    }
    style::length_px(value, font, 16.0, font).unwrap_or(font * 1.2)
}

fn align_items(value: &str) -> Option<AlignItems> {
    Some(match value {
        "flex-start" | "self-start" => AlignItems::FLEX_START,
        "start" => AlignItems::START,
        "flex-end" | "self-end" => AlignItems::FLEX_END,
        "end" => AlignItems::END,
        "center" => AlignItems::CENTER,
        "baseline" | "first baseline" | "last baseline" => AlignItems::BASELINE,
        "stretch" => AlignItems::STRETCH,
        _ => return None,
    })
}

fn align_content(value: &str) -> Option<AlignContent> {
    Some(match value {
        "flex-start" => AlignContent::FLEX_START,
        "start" | "left" => AlignContent::START,
        "flex-end" => AlignContent::FLEX_END,
        "end" | "right" => AlignContent::END,
        "center" => AlignContent::CENTER,
        "stretch" => AlignContent::STRETCH,
        "space-between" => AlignContent::SPACE_BETWEEN,
        "space-around" => AlignContent::SPACE_AROUND,
        "space-evenly" => AlignContent::SPACE_EVENLY,
        _ => return None,
    })
}

fn overflow(value: &str) -> Overflow {
    match value {
        "hidden" => Overflow::Hidden,
        "clip" => Overflow::Clip,
        "scroll" | "auto" | "overlay" => Overflow::Scroll,
        _ => Overflow::Visible,
    }
}

/// Grid tracks from `grid-template-columns`: lengths, percentages, `fr`,
/// `auto`, `min-content`, `max-content`, `minmax()` and `repeat(n, ...)`.
/// Anything else gives no explicit tracks, and the grid's items flow into
/// implicit ones.
fn grid_tracks(value: &str, basis: Basis) -> Vec<GridTemplateComponent<String>> {
    let value = value.trim();
    if value == "none" || value.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for token in split_top(value) {
        if let Some(inner) = token.strip_prefix("repeat(").and_then(|t| t.strip_suffix(')')) {
            let Some((count, tracks)) = inner.split_once(',') else { return Vec::new() };
            let Ok(count) = count.trim().parse::<u16>() else {
                // auto-fill and auto-fit need the container's size; one track.
                let single: Vec<_> = split_top(tracks).iter().filter_map(|t| track(t, basis)).collect();
                if single.is_empty() {
                    return Vec::new();
                }
                out.push(style_helpers::repeat(RepetitionCount::AutoFill, single));
                continue;
            };
            let single: Vec<_> = split_top(tracks).iter().filter_map(|t| track(t, basis)).collect();
            for _ in 0..count {
                out.extend(single.iter().cloned().map(GridTemplateComponent::Single));
            }
        } else if token.starts_with('[') {
            // Line names are ignored.
        } else if let Some(t) = track(&token, basis) {
            out.push(GridTemplateComponent::Single(t));
        } else {
            return Vec::new();
        }
    }
    out
}

fn track(token: &str, basis: Basis) -> Option<TrackSizingFunction> {
    let token = token.trim();
    if let Some(inner) = token.strip_prefix("minmax(").and_then(|t| t.strip_suffix(')')) {
        let (min, max) = inner.split_once(',')?;
        let min = match min.trim() {
            "auto" => MinTrackSizingFunction::auto(),
            "min-content" => MinTrackSizingFunction::min_content(),
            "max-content" => MinTrackSizingFunction::max_content(),
            other => MinTrackSizingFunction::from(length(other, basis)?),
        };
        let max = match max.trim() {
            "auto" => MaxTrackSizingFunction::auto(),
            "min-content" => MaxTrackSizingFunction::min_content(),
            "max-content" => MaxTrackSizingFunction::max_content(),
            other if other.ends_with("fr") => MaxTrackSizingFunction::fr(other.trim_end_matches("fr").parse().ok()?),
            other => MaxTrackSizingFunction::from(length(other, basis)?),
        };
        return Some(style_helpers::minmax(min, max));
    }
    Some(match token {
        "auto" => TrackSizingFunction::AUTO,
        "min-content" => TrackSizingFunction::MIN_CONTENT,
        "max-content" => TrackSizingFunction::MAX_CONTENT,
        t if t.ends_with("fr") => style_helpers::fr(t.trim_end_matches("fr").parse::<f32>().ok()?),
        t => TrackSizingFunction::from(length(t, basis)?),
    })
}

/// Splits on whitespace outside parentheses and brackets.
fn split_top(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for c in value.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// How an element's box takes part in layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outer {
    Block,
    Inline,
    None,
    Contents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Inner {
    Flow,
    Flex,
    Grid,
}

fn display(style: &ComputedStyle) -> (Outer, Inner) {
    let value = style.get("display").trim().to_ascii_lowercase();
    match value.as_str() {
        "none" => (Outer::None, Inner::Flow),
        "contents" => (Outer::Contents, Inner::Flow),
        "inline" | "ruby" => (Outer::Inline, Inner::Flow),
        "inline-block" | "inline-table" | "inline flow-root" => (Outer::Inline, Inner::Flow),
        "inline-flex" | "inline flex" => (Outer::Inline, Inner::Flex),
        "inline-grid" | "inline grid" => (Outer::Inline, Inner::Grid),
        "flex" | "block flex" => (Outer::Block, Inner::Flex),
        "grid" | "block grid" => (Outer::Block, Inner::Grid),
        // Rows lay their cells out side by side; close enough to a table.
        "table-row" => (Outer::Block, Inner::Flex),
        _ => (Outer::Block, Inner::Flow),
    }
}

/// The taffy style for an element, before its role in the tree is known.
fn taffy_style(style: &ComputedStyle, basis: Basis) -> Style {
    let side = |prefix: &str, f: &dyn Fn(&str) -> LengthPercentageAuto| taffy::Rect {
        top: f(style.get(&format!("{prefix}top"))),
        right: f(style.get(&format!("{prefix}right"))),
        bottom: f(style.get(&format!("{prefix}bottom"))),
        left: f(style.get(&format!("{prefix}left"))),
    };
    let la = |v: &str| length_auto(v, basis);
    let padding = |s: &str| length_zero(style.get(&format!("padding-{s}")), basis);
    let gap = |name: &str| match style.get(name) {
        "normal" | "" => LengthPercentage::length(0.0),
        v => length_zero(v, basis),
    };
    let mut out = Style {
        box_sizing: if style.get("box-sizing") == "border-box" { BoxSizing::BorderBox } else { BoxSizing::ContentBox },
        position: match style.get("position") {
            "absolute" | "fixed" => Position::Absolute,
            _ => Position::Relative,
        },
        inset: side("", &la),
        margin: side("margin-", &la),
        padding: taffy::Rect { top: padding("top"), right: padding("right"), bottom: padding("bottom"), left: padding("left") },
        border: taffy::Rect {
            top: border(style, "top", basis),
            right: border(style, "right", basis),
            bottom: border(style, "bottom", basis),
            left: border(style, "left", basis),
        },
        size: Size { width: dimension(style.get("width"), basis), height: dimension(style.get("height"), basis) },
        min_size: Size { width: la(style.get("min-width")), height: la(style.get("min-height")) },
        max_size: Size { width: la(style.get("max-width")), height: la(style.get("max-height")) },
        overflow: taffy::Point { x: overflow(style.get("overflow-x")), y: overflow(style.get("overflow-y")) },
        flex_direction: match style.get("flex-direction") {
            "column" => FlexDirection::Column,
            "row-reverse" => FlexDirection::RowReverse,
            "column-reverse" => FlexDirection::ColumnReverse,
            _ => FlexDirection::Row,
        },
        flex_wrap: match style.get("flex-wrap") {
            "wrap" => FlexWrap::Wrap,
            "wrap-reverse" => FlexWrap::WrapReverse,
            _ => FlexWrap::NoWrap,
        },
        flex_grow: style.get("flex-grow").parse().unwrap_or(0.0),
        flex_shrink: style.get("flex-shrink").parse().unwrap_or(1.0),
        flex_basis: match style.get("flex-basis") {
            "auto" | "content" | "" => Dimension::auto(),
            v => dimension(v, basis),
        },
        align_items: align_items(style.get("align-items")),
        align_self: align_items(style.get("align-self")),
        justify_items: align_items(style.get("justify-items")),
        justify_self: align_items(style.get("justify-self")),
        align_content: align_content(style.get("align-content")),
        justify_content: align_content(style.get("justify-content")),
        gap: Size { width: gap("column-gap"), height: gap("row-gap") },
        grid_template_columns: grid_tracks(style.get("grid-template-columns"), basis),
        grid_template_rows: grid_tracks(style.get("grid-template-rows"), basis),
        float: match style.get("float") {
            "left" | "inline-start" => Float::Left,
            "right" | "inline-end" => Float::Right,
            _ => Float::None,
        },
        clear: match style.get("clear") {
            "left" => Clear::Left,
            "right" => Clear::Right,
            "both" => Clear::Both,
            _ => Clear::None,
        },
        ..Style::default()
    };
    if let Ok(ratio) = style.get("aspect-ratio").replace(' ', "").split('/').map(str::parse::<f32>).collect::<Result<Vec<_>, _>>() {
        out.aspect_ratio = match ratio.as_slice() {
            [w, h] if *h > 0.0 => Some(w / h),
            [r] => Some(*r),
            _ => None,
        };
    }
    out
}

// ---- measuring --------------------------------------------------------------

/// What a leaf measures itself from.
#[derive(Debug, Clone)]
enum Content {
    Text { words: Vec<f64>, space: f64, line: f64, wrap: bool },
    Replaced { width: f64, height: f64 },
}

/// The advance of `c`, from the font text is painted with (`crate::font`).
fn char_width(c: char, font: f64, monospace: bool) -> f64 {
    crate::font::advance(c, font, monospace)
}

fn text_content(text: &str, style: &ComputedStyle) -> Content {
    let font = style.font_size();
    let monospace = style.get("font-family").contains("monospace");
    let white_space = style.get("white-space");
    let wrap = !matches!(white_space, "nowrap" | "pre");
    let words = text
        .split_whitespace()
        .map(|w| w.chars().map(|c| char_width(c, font, monospace)).sum())
        .collect();
    Content::Text { words, space: char_width(' ', font, monospace), line: line_height(style), wrap }
}

impl Content {
    fn measure(&self, known: Size<Option<f32>>, available: Size<AvailableSpace>) -> Size<f32> {
        match self {
            Content::Text { words, space, line, wrap } => {
                let max_content: f64 = words.iter().sum::<f64>() + space * words.len().saturating_sub(1) as f64;
                let min_content = if *wrap { words.iter().cloned().fold(0.0, f64::max) } else { max_content };
                let width = match (known.width, available.width) {
                    (Some(w), _) => w as f64,
                    (None, AvailableSpace::Definite(w)) => max_content.min((w as f64).max(min_content)),
                    (None, AvailableSpace::MinContent) => min_content,
                    (None, AvailableSpace::MaxContent) => max_content,
                };
                let lines = if *wrap { wrap_lines(words, *space, width) } else { 1 };
                let height = known.height.map(f64::from).unwrap_or(lines as f64 * line);
                Size { width: width.ceil() as f32, height: height.ceil() as f32 }
            }
            Content::Replaced { width, height } => {
                let ratio = if *height > 0.0 { width / height } else { 0.0 };
                match (known.width, known.height) {
                    (Some(w), Some(h)) => Size { width: w, height: h },
                    (Some(w), None) => Size { width: w, height: if ratio > 0.0 { (w as f64 / ratio) as f32 } else { *height as f32 } },
                    (None, Some(h)) => Size { width: if ratio > 0.0 { (h as f64 * ratio) as f32 } else { *width as f32 }, height: h },
                    (None, None) => Size { width: *width as f32, height: *height as f32 },
                }
            }
        }
    }
}

/// Lines needed to set `words` greedily in `width`.
fn wrap_lines(words: &[f64], space: f64, width: f64) -> usize {
    line_starts(words, space, width).len()
}

/// Where each line starts, as an index into `words`, when they are set
/// greedily in `width`. Painting breaks text with this too, so the lines it
/// draws are the lines layout measured.
pub(crate) fn line_starts(words: &[f64], space: f64, width: f64) -> Vec<usize> {
    let mut starts = vec![0];
    let mut used: Option<f64> = None;
    for (i, w) in words.iter().enumerate() {
        used = Some(match used {
            None => *w,
            Some(u) if u + space + w > width + 0.5 => {
                starts.push(i);
                *w
            }
            Some(u) => u + space + w,
        });
    }
    starts
}

/// The advance of every character in `text`, and of a space, as layout
/// measures them for `style`.
pub(crate) fn measure_with(style: &ComputedStyle) -> impl Fn(char) -> f64 {
    let font = style.font_size();
    let monospace = style.get("font-family").contains("monospace");
    move |c| char_width(c, font, monospace)
}

/// The size an `<img>` has of its own, if its image has arrived. Layout does
/// not wait for it: the load waits for a page's images, as Chrome's `load`
/// event does, and a screenshot waits for any that came after.
fn intrinsic_size(doc: &Document, id: NodeId) -> Option<(f64, f64)> {
    let url = doc.image_url(id)?;
    let (w, h) = doc.images()?.size(&url, std::time::Instant::now())?;
    Some((w as f64, h as f64))
}

/// Chrome's default size for a replaced element or form control.
fn replaced_size(doc: &Document, id: NodeId, style: &ComputedStyle) -> Option<(f64, f64)> {
    let tag = doc.tag(id)?;
    let font = style.font_size();
    let line = (font * 1.2).ceil();
    let attr = |name: &str| doc.attr(id, name).and_then(|v| v.trim().trim_end_matches("px").parse::<f64>().ok());
    let monospace = style.get("font-family").contains("monospace");
    let text_width = |text: &str| text.chars().map(|c| char_width(c, font, monospace)).sum::<f64>();
    Some(match tag {
        "input" => match doc.attr(id, "type").unwrap_or("text").to_ascii_lowercase().as_str() {
            "hidden" => return None,
            "checkbox" | "radio" => (13.0, 13.0),
            "submit" | "button" | "reset" => {
                let label = doc.attr(id, "value").unwrap_or(if doc.attr(id, "type") == Some("reset") { "Reset" } else { "Submit" });
                (text_width(label) + 12.0, line + 6.0)
            }
            "range" => (129.0, 16.0),
            "color" => (50.0, 27.0),
            "image" => (attr("width").unwrap_or(0.0), attr("height").unwrap_or(0.0)),
            _ => (attr("size").map(|s| s * font * 0.55 + 6.0).unwrap_or(153.0), line + 6.0),
        },
        "textarea" => {
            let cols = attr("cols").unwrap_or(20.0);
            let rows = attr("rows").unwrap_or(2.0);
            (cols * font * 0.55 + 6.0, rows * line + 6.0)
        }
        "select" => {
            let widest = doc
                .descendants(id)
                .filter(|o| doc.is_tag(*o, "option"))
                .map(|o| text_width(doc.text_content(o).trim()))
                .fold(0.0, f64::max);
            (widest + 28.0, line + 6.0)
        }
        "img" => {
            // Attributes first; one alone keeps the image's aspect ratio.
            let natural = intrinsic_size(doc, id);
            let ratio = natural.filter(|(w, h)| *w > 0.0 && *h > 0.0).map(|(w, h)| w / h);
            match (attr("width"), attr("height"), natural) {
                (Some(w), Some(h), _) => (w, h),
                (Some(w), None, _) => (w, ratio.map(|r| w / r).unwrap_or(0.0)),
                (None, Some(h), _) => (ratio.map(|r| h * r).unwrap_or(0.0), h),
                (None, None, Some(size)) => size,
                (None, None, None) => (0.0, 0.0),
            }
        }
        "canvas" | "iframe" | "video" | "embed" | "object" | "svg" => {
            (attr("width").unwrap_or(300.0), attr("height").unwrap_or(150.0))
        }
        "meter" => (80.0, 16.0),
        "progress" => (160.0, 16.0),
        _ => return None,
    })
}

// ---- the tree ---------------------------------------------------------------

/// Boxes for a document, and what hit testing needs.
#[derive(Debug, Default)]
pub struct Layout {
    boxes: HashMap<NodeId, Rect>,
    /// Elements with a box, back to front.
    paint: Vec<NodeId>,
    /// The ancestors whose overflow clips an element, by element.
    clips: HashMap<NodeId, Vec<NodeId>>,
}

impl Layout {
    /// Elements with a box, back to front.
    pub fn paint_order(&self) -> &[NodeId] {
        &self.paint
    }

    /// The ancestors whose `overflow` clips `id`.
    pub fn clips_of(&self, id: NodeId) -> &[NodeId] {
        self.clips.get(&id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// How far boxes reach right and down: the page's scrollable size, at
    /// least the viewport's.
    pub fn content_size(&self, viewport: (f64, f64)) -> (f64, f64) {
        self.boxes.values().fold(viewport, |(w, h), r| (w.max(r.x + r.width), h.max(r.y + r.height)))
    }

    /// The element's border box, or a text node's box.
    pub fn rect(&self, id: NodeId) -> Option<Rect> {
        self.boxes.get(&id).copied()
    }

    /// The topmost element at a point that takes pointer input.
    pub fn hit_test(&self, styles: &Styles, x: f64, y: f64) -> Option<NodeId> {
        self.hits(styles, x, y).into_iter().next()
    }

    /// Every element at a point that takes pointer input, front to back.
    pub fn hits(&self, styles: &Styles, x: f64, y: f64) -> Vec<NodeId> {
        self.paint
            .iter()
            .rev()
            .copied()
            .filter(|id| self.boxes.get(id).is_some_and(|r| r.contains(x, y)))
            .filter(|id| {
                styles.get(*id).is_some_and(|s| {
                    s.get("pointer-events") != "none" && !matches!(s.get("visibility"), "hidden" | "collapse")
                })
            })
            .filter(|id| {
                self.clips.get(id).is_none_or(|clips| clips.iter().all(|c| self.boxes.get(c).is_some_and(|r| r.contains(x, y))))
            })
            .collect()
    }
}

/// Items an element's box contributes to its parent.
enum Item {
    /// In flow; `true` when inline-level.
    Flow(taffy::NodeId, bool),
    /// Absolutely positioned within the parent.
    OutOfFlow(taffy::NodeId),
}

struct Builder<'a> {
    doc: &'a Document,
    styles: &'a Styles,
    tree: TaffyTree<Content>,
    viewport: (f64, f64),
    root_font: f64,
    dom_of: HashMap<taffy::NodeId, NodeId>,
    node_of: HashMap<NodeId, taffy::NodeId>,
    /// Boxes placed in a containing block other than their parent: the
    /// positioned ancestor, or `None` for the viewport.
    hoisted: Vec<(Option<NodeId>, taffy::NodeId)>,
    /// Paint keys in document order: (stacking level, layer, element). The
    /// layer is the document position of the nearest positioned inclusive
    /// ancestor, offset past every unpositioned element, or the element's
    /// own position when there is none.
    order: Vec<(i64, usize, usize, NodeId)>,
    clips: HashMap<NodeId, Vec<NodeId>>,
}

impl Builder<'_> {
    fn basis(&self, style: &ComputedStyle) -> Basis {
        Basis { font: style.font_size(), root_font: self.root_font, viewport: self.viewport }
    }

    fn element(&mut self, id: NodeId, level: i64, layer: Option<usize>, clips: &[NodeId]) -> Vec<Item> {
        let Some(style) = self.styles.get(id) else { return Vec::new() };
        let (outer, inner) = display(style);
        match outer {
            Outer::None => return Vec::new(),
            Outer::Contents => return self.children(id, style, level, layer, clips, false),
            _ => {}
        }
        let position = style.get("position");
        let positioned = position != "static";
        let level = match style.get("z-index").parse::<i64>() {
            Ok(z) if positioned => z,
            _ => level,
        };
        // In a stacking context, positioned boxes paint after the unpositioned
        // ones, in document order, each together with its unpositioned
        // descendants.
        let position_in_document = self.order.len();
        let layer = if positioned { Some(position_in_document) } else { layer };
        let layer_key = layer.map(|l| usize::MAX / 2 + l).unwrap_or(position_in_document);
        self.order.push((level, layer_key, position_in_document, id));
        if !clips.is_empty() {
            self.clips.insert(id, clips.to_vec());
        }

        let mut taffy = taffy_style(style, self.basis(style));
        let node = if let Some((width, height)) = replaced_size(self.doc, id, style) {
            taffy.item_is_replaced = true;
            taffy.display = Display::Block;
            // A control keeps its own size unless styled; it is not
            // stretched to fill a flex line, as it would be if `auto`.
            let (css_width, css_height) = (style.get("width"), style.get("height"));
            if css_width == "auto" && !(css_height != "auto" && taffy.aspect_ratio.is_some()) {
                taffy.size.width = Dimension::length(width as f32);
            }
            if css_height == "auto" && css_width == "auto" {
                taffy.size.height = Dimension::length(height as f32);
            }
            self.tree.new_leaf_with_context(taffy, Content::Replaced { width, height }).expect("taffy leaf")
        } else {
            // A button's content is centred vertically in it, whatever its
            // height, as Chrome lays it out: a column of line boxes, centred.
            let button = self.doc.is_tag(id, "button") && inner == Inner::Flow;
            let inline_flow = outer == Outer::Inline && inner == Inner::Flow && !button;
            taffy.display = match inner {
                Inner::Flow if inline_flow => Display::Flex,
                Inner::Flow => Display::Block,
                Inner::Flex => Display::Flex,
                Inner::Grid => Display::Grid,
            };
            if inline_flow {
                // An inline box: its content wraps as a row would.
                taffy.flex_direction = FlexDirection::Row;
                taffy.flex_wrap = FlexWrap::Wrap;
                taffy.align_items = Some(AlignItems::FLEX_START);
            }
            if button {
                taffy.display = Display::Flex;
                taffy.flex_direction = FlexDirection::Column;
                taffy.flex_wrap = FlexWrap::NoWrap;
                taffy.justify_content = Some(JustifyContent::CENTER);
                taffy.align_items = Some(AlignItems::STRETCH);
            }
            let node = self.tree.new_leaf(taffy).expect("taffy node");
            let clips_inside: Vec<NodeId> = if style.get("overflow-x") != "visible" || style.get("overflow-y") != "visible" {
                clips.iter().copied().chain(std::iter::once(id)).collect()
            } else {
                clips.to_vec()
            };
            let as_items = inner != Inner::Flow;
            let items = self.children(id, style, level, layer, &clips_inside, as_items);
            let children = self.arrange(items, style, as_items || inline_flow);
            self.tree.set_children(node, &children).expect("taffy children");
            node
        };
        self.dom_of.insert(node, id);
        self.node_of.insert(id, node);

        match position {
            "fixed" => {
                self.hoisted.push((None, node));
                Vec::new()
            }
            "absolute" => {
                let containing = std::iter::successors(self.doc.parent(id), |p| self.doc.parent(*p))
                    .filter(|p| self.doc.is_element(*p))
                    .find(|p| {
                        self.styles
                            .get(*p)
                            .is_some_and(|s| s.get("position") != "static" || s.get("transform") != "none")
                    });
                let parent = self.doc.parent(id).filter(|p| self.doc.is_element(*p));
                if containing.is_some() && containing == parent {
                    vec![Item::OutOfFlow(node)]
                } else {
                    self.hoisted.push((containing, node));
                    Vec::new()
                }
            }
            _ => vec![Item::Flow(node, outer == Outer::Inline)],
        }
    }

    /// The items `id`'s children contribute. `as_items` is set for flex and
    /// grid containers, whose text children become items of their own.
    fn children(
        &mut self,
        id: NodeId,
        style: &ComputedStyle,
        level: i64,
        layer: Option<usize>,
        clips: &[NodeId],
        as_items: bool,
    ) -> Vec<Item> {
        let mut items = Vec::new();
        for child in self.doc.children(id).to_vec() {
            match &self.doc.node(child).data {
                NodeData::Element { .. } => items.extend(self.element(child, level, layer, clips)),
                NodeData::Text => {
                    let text = &self.doc.node(child).text;
                    let pre = matches!(style.get("white-space"), "pre" | "pre-wrap" | "pre-line" | "break-spaces");
                    if text.trim().is_empty() && !pre {
                        continue;
                    }
                    let content = text_content(text, style);
                    let leaf_style = Style { flex_shrink: 1.0, ..Style::default() };
                    let leaf = self.tree.new_leaf_with_context(leaf_style, content).expect("taffy text");
                    self.dom_of.insert(leaf, child);
                    let _ = as_items;
                    items.push(Item::Flow(leaf, true));
                }
                _ => {}
            }
        }
        items
    }

    /// Children for the container's taffy node. In block flow, each run of
    /// inline-level items goes into an anonymous wrapping row; elsewhere the
    /// items are used as they are.
    fn arrange(&mut self, items: Vec<Item>, style: &ComputedStyle, flat: bool) -> Vec<taffy::NodeId> {
        let mut children = Vec::new();
        let mut absolute = Vec::new();
        let mut run = Vec::new();
        let justify = match style.get("text-align") {
            "center" | "-webkit-center" => Some(AlignContent::CENTER),
            "right" | "end" | "-webkit-right" => Some(AlignContent::FLEX_END),
            _ => None,
        };
        let flush = |builder: &mut Self, run: &mut Vec<taffy::NodeId>, children: &mut Vec<taffy::NodeId>| {
            if run.is_empty() {
                return;
            }
            let line = Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                align_items: Some(AlignItems::FLEX_START),
                justify_content: justify,
                ..Style::default()
            };
            children.push(builder.tree.new_with_children(line, run).expect("taffy line"));
            run.clear();
        };
        for item in items {
            match item {
                Item::OutOfFlow(node) => absolute.push(node),
                Item::Flow(node, _) if flat => children.push(node),
                Item::Flow(node, true) => run.push(node),
                Item::Flow(node, false) => {
                    flush(self, &mut run, &mut children);
                    children.push(node);
                }
            }
        }
        flush(self, &mut run, &mut children);
        children.extend(absolute);
        children
    }
}

/// Lays out the whole document in a viewport.
pub fn compute(doc: &Document, styles: &Styles, viewport: (f64, f64)) -> Layout {
    let root_font = doc
        .children(doc.document_id())
        .iter()
        .find(|c| doc.is_element(**c))
        .and_then(|html| styles.get(*html))
        .map(ComputedStyle::font_size)
        .unwrap_or(16.0);
    let mut builder = Builder {
        doc,
        styles,
        tree: TaffyTree::new(),
        viewport,
        root_font,
        dom_of: HashMap::new(),
        node_of: HashMap::new(),
        hoisted: Vec::new(),
        order: Vec::new(),
        clips: HashMap::new(),
    };
    let mut top = Vec::new();
    for child in doc.children(doc.document_id()).to_vec() {
        if doc.is_element(child) {
            for item in builder.element(child, 0, None, &[]) {
                match item {
                    Item::Flow(node, _) | Item::OutOfFlow(node) => top.push(node),
                }
            }
        }
    }
    // The initial containing block is the viewport, definite in both
    // directions, so `height: 100%` on the root resolves as in a browser.
    // Content taller than it overflows, as it does there.
    let viewport_style = Style {
        display: Display::Block,
        size: Size { width: Dimension::length(viewport.0 as f32), height: Dimension::length(viewport.1 as f32) },
        ..Style::default()
    };
    let root = builder.tree.new_with_children(viewport_style, &top).expect("taffy root");
    for (containing, node) in std::mem::take(&mut builder.hoisted) {
        let parent = containing.and_then(|c| builder.node_of.get(&c).copied()).unwrap_or(root);
        let _ = builder.tree.add_child(parent, node);
    }
    let _ = builder.tree.compute_layout_with_measure(
        root,
        Size { width: AvailableSpace::Definite(viewport.0 as f32), height: AvailableSpace::MaxContent },
        // taffy hands a leaf's whole layout to this function; its own
        // leaf algorithm applies size, padding and border around what the
        // content measures.
        |input, _node, content, style| {
            taffy::compute_leaf_layout(input, style, |_, _| 0.0, |known, available| match &content {
                Some(content) => content.measure(known, available),
                None => Size::ZERO,
            })
        },
    );

    // Locations are relative to the parent box; sum them into page space.
    // A translation moves the element and everything inside it.
    let mut boxes = HashMap::new();
    let mut stack = vec![(root, 0.0f64, 0.0f64)];
    while let Some((node, px, py)) = stack.pop() {
        let Ok(layout) = builder.tree.layout(node) else { continue };
        let mut x = px + layout.location.x as f64;
        let mut y = py + layout.location.y as f64;
        let size = (layout.size.width as f64, layout.size.height as f64);
        if let Some(style) = builder.dom_of.get(&node).and_then(|dom| styles.get(*dom)) {
            let basis = Basis { font: style.font_size(), root_font, viewport };
            let (dx, dy) = translation(style.get("transform"), size, basis);
            x += dx;
            y += dy;
        }
        if let Some(dom) = builder.dom_of.get(&node) {
            boxes.insert(*dom, Rect { x, y, width: layout.size.width as f64, height: layout.size.height as f64 });
        }
        for child in builder.tree.children(node).unwrap_or_default() {
            stack.push((child, x, y));
        }
    }

    // Back to front: stacking level, then layer, then document order.
    let mut order = builder.order;
    order.sort_by_key(|(level, layer, at, _)| (*level, *layer, *at));
    Layout { boxes, paint: order.into_iter().map(|(_, _, _, id)| id).collect(), clips: builder.clips }
}

/// The translation part of a `transform`: `translate()`, `translateX()`,
/// `translateY()`, `translate3d()` and the offsets of `matrix()`. Rotation
/// and scaling are ignored. Percentages are of the element's own size.
fn translation(transform: &str, size: (f64, f64), basis: Basis) -> (f64, f64) {
    if transform == "none" || transform.is_empty() {
        return (0.0, 0.0);
    }
    let resolve = |value: &str, own: f64| -> f64 {
        let value = value.trim();
        match value.strip_suffix('%') {
            Some(p) => p.trim().parse::<f64>().map(|p| p / 100.0 * own).unwrap_or(0.0),
            None => px(value, basis).unwrap_or(0.0),
        }
    };
    let (mut dx, mut dy) = (0.0, 0.0);
    let mut rest = transform;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().to_ascii_lowercase();
        let Some(close) = rest[open..].find(')') else { break };
        let args: Vec<&str> = rest[open + 1..open + close].split(',').map(str::trim).collect();
        match name.as_str() {
            "translate" | "translate3d" => {
                dx += resolve(args.first().copied().unwrap_or("0"), size.0);
                dy += resolve(args.get(1).copied().unwrap_or("0"), size.1);
            }
            "translatex" => dx += resolve(args.first().copied().unwrap_or("0"), size.0),
            "translatey" => dy += resolve(args.first().copied().unwrap_or("0"), size.1),
            "matrix" if args.len() == 6 => {
                dx += args[4].parse::<f64>().unwrap_or(0.0);
                dy += args[5].parse::<f64>().unwrap_or(0.0);
            }
            "matrix3d" if args.len() == 16 => {
                dx += args[12].parse::<f64>().unwrap_or(0.0);
                dy += args[13].parse::<f64>().unwrap_or(0.0);
            }
            _ => {}
        }
        rest = &rest[open + close + 1..];
    }
    (dx, dy)
}

/// The document's layout as it is now, computed when the document changed
/// since the last call.
pub fn document_layout(doc: &Document) -> Arc<Layout> {
    // An image arriving changes layout without changing the document.
    let key = (doc.version(), doc.images().map(|i| i.generation()).unwrap_or(0));
    let mut cache = doc.layout_cache.0.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((cached, layout)) = &*cache {
        if *cached == key {
            return Arc::clone(layout);
        }
    }
    let styles = style::document_styles(doc);
    let layout = Arc::new(compute(doc, &styles, doc.viewport()));
    *cache = Some((key, Arc::clone(&layout)));
    layout
}

/// What a document keeps of its last layout, by document version and image
/// generation.
#[derive(Default)]
pub struct LayoutCache(Mutex<Option<((u64, u64), Arc<Layout>)>>);

impl Clone for LayoutCache {
    fn clone(&self) -> Self {
        LayoutCache::default()
    }
}

impl std::fmt::Debug for LayoutCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LayoutCache")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_html;

    fn lay(html: &str) -> (Document, Arc<Styles>, Arc<Layout>) {
        let doc = parse_html(html);
        let styles = style::document_styles(&doc);
        let layout = document_layout(&doc);
        (doc, styles, layout)
    }

    fn rect(doc: &Document, layout: &Layout, id: &str) -> Rect {
        layout.rect(doc.find_id(doc.document_id(), id).unwrap()).unwrap_or_default()
    }

    fn id_at(doc: &Document, styles: &Styles, layout: &Layout, x: f64, y: f64) -> String {
        layout
            .hit_test(styles, x, y)
            .map(|n| doc.attr(n, "id").unwrap_or(doc.tag(n).unwrap_or("?")).to_string())
            .unwrap_or_default()
    }

    #[test]
    fn blocks_stack_and_fill_the_width() {
        let (doc, _, layout) = lay(
            "<style>body { margin: 0 } #a { height: 50px } #b { height: 30px; margin-top: 10px; width: 50% }</style>\
             <body><div id='a'></div><div id='b'></div></body>",
        );
        assert_eq!(rect(&doc, &layout, "a"), Rect { x: 0.0, y: 0.0, width: 800.0, height: 50.0 });
        assert_eq!(rect(&doc, &layout, "b"), Rect { x: 0.0, y: 60.0, width: 400.0, height: 30.0 });
    }

    #[test]
    fn text_wraps_and_inline_boxes_sit_in_a_line() {
        let (doc, _, layout) = lay(
            "<style>body { margin: 0; font-size: 10px } #p { width: 100px }</style>\
             <body><p id='p'>aaaa bbbb cccc dddd eeee ffff</p><div id='d'><a id='a' href='/'>one</a> <b id='b'>two</b></div></body>",
        );
        let p = rect(&doc, &layout, "p");
        assert!(p.height >= 24.0, "six words in 100px wrap onto lines: {p:?}");
        let (a, b) = (rect(&doc, &layout, "a"), rect(&doc, &layout, "b"));
        assert_eq!(a.y, b.y, "inline boxes share a line");
        assert!(b.x >= a.x + a.width, "{a:?} {b:?}");
    }

    #[test]
    fn flex_rows_and_replaced_defaults() {
        let (doc, _, layout) = lay(
            "<style>body { margin: 0 } #row { display: flex; gap: 10px } #grow { flex: 1 }</style>\
             <body><div id='row'><input id='i'><div id='grow'>x</div><input id='c' type='checkbox'></div></body>",
        );
        let (input, grow, check) = (rect(&doc, &layout, "i"), rect(&doc, &layout, "grow"), rect(&doc, &layout, "c"));
        assert_eq!(input.width, 153.0);
        assert_eq!((check.width, check.height), (13.0, 13.0));
        assert_eq!(grow.x, 163.0);
        assert_eq!(grow.x + grow.width + 10.0, check.x);
    }

    #[test]
    fn hit_testing_follows_paint_order_and_pointer_events() {
        let (doc, styles, layout) = lay(
            "<style>body { margin: 0 } #base { height: 100px }\
             #modal { position: fixed; top: 0; left: 0; width: 200px; height: 50px; z-index: 10 }\
             #ghost { position: absolute; top: 0; left: 300px; width: 100px; height: 100px; pointer-events: none }\
             #wrap { position: relative; height: 40px } #inner { position: absolute; top: 10px; left: 10px; width: 20px; height: 20px }\
             </style><body><div id='base'><button id='under'>x</button></div><div id='modal'></div><div id='ghost'></div>\
             <div id='wrap'><span id='inner'></span></div><div id='gone' style='display:none'>g</div></body>",
        );
        assert_eq!(id_at(&doc, &styles, &layout, 10.0, 10.0), "modal");
        assert_eq!(id_at(&doc, &styles, &layout, 310.0, 10.0), "base", "pointer-events: none lets the click through");
        assert_eq!(id_at(&doc, &styles, &layout, 500.0, 60.0), "base");
        let inner = rect(&doc, &layout, "inner");
        assert_eq!((inner.x, inner.y), (10.0, 110.0), "placed in its positioned ancestor");
        assert_eq!(id_at(&doc, &styles, &layout, 15.0, 115.0), "inner");
        assert!(layout.rect(doc.find_id(doc.document_id(), "gone").unwrap()).is_none());
    }

    #[test]
    fn a_positioned_box_paints_with_its_content() {
        // The input is not positioned, its parent is; the input still paints
        // on top of its parent, as it does in a browser.
        let (doc, styles, layout) = lay(
            "<style>body { margin: 0 } #box { position: relative } #i { display: block }\
             #later { position: relative; margin-top: -10px; height: 30px }</style>\
             <body><div id='box'><input id='i'></div><div id='later'></div></body>",
        );
        assert_eq!(id_at(&doc, &styles, &layout, 20.0, 5.0), "i");
        // A later positioned sibling overlapping it still wins.
        assert_eq!(id_at(&doc, &styles, &layout, 20.0, 20.0), "later");
    }

    #[test]
    fn overflow_clips_hit_testing() {
        let (doc, styles, layout) = lay(
            "<style>body { margin: 0 } #box { width: 50px; height: 50px; overflow: hidden }\
             #big { width: 200px; height: 200px }</style><body><div id='box'><div id='big'></div></div></body>",
        );
        assert_eq!(id_at(&doc, &styles, &layout, 20.0, 20.0), "big");
        // Outside the box, the clipped child is not there; the body is.
        assert_eq!(id_at(&doc, &styles, &layout, 100.0, 20.0), "body");
    }

    #[test]
    fn percentages_resolve_against_the_viewport_and_translations_apply() {
        let (doc, _, layout) = lay(
            "<style>html, body { height: 100%; margin: 0 } #wrap { height: 100%; position: relative }\
             #mid { position: absolute; top: 50%; left: 50%; width: 200px; height: 100px; transform: translate(-50%, -50%) }\
             #foot { position: absolute; bottom: 10px; left: 0; height: 20px; width: 50px }</style>\
             <body><div id='wrap'><div id='mid'><span id='in'>x</span></div><div id='foot'></div></div></body>",
        );
        assert_eq!(rect(&doc, &layout, "wrap").height, 600.0);
        assert_eq!(rect(&doc, &layout, "mid"), Rect { x: 300.0, y: 250.0, width: 200.0, height: 100.0 });
        assert_eq!(rect(&doc, &layout, "in").y, 250.0, "descendants move with the translation");
        assert_eq!(rect(&doc, &layout, "foot").y, 570.0);
    }

    #[test]
    fn text_is_measured_with_the_font() {
        let (doc, _, layout) = lay(
            "<style>body { margin: 0; font-size: 10px } span { white-space: nowrap }</style>\
             <body><span id='k'>로그인</span><br><span id='l'>abc</span></body>",
        );
        // Noto Sans KR sets Hangul at 0.92em: 27.6px, rounded up.
        assert_eq!(rect(&doc, &layout, "k").width, 28.0);
        assert!(rect(&doc, &layout, "l").width < 20.0);
    }
}

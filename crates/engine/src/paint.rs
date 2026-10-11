//! Screenshots: the layout, painted.
//!
//! What is painted, back to front in the layout's paint order: the canvas
//! background (the root's, or the body's when the root has none),
//! background colours, borders with their radius, text in the embedded font
//! (`crate::font`), form controls (Chrome's look when the page has not
//! styled them, values, placeholders, checks), list bullets, opacity, and
//! clipping by `overflow`. Coordinates are those of the layout, so what a
//! screenshot shows is where `DOM.getBoxModel` says it is, and where a click
//! lands.
//!
//! What is not: images (nothing is decoded), background images and
//! gradients, shadows, outlines, rotation and scaling, and font faces other
//! than the one embedded (bold is smeared, italic is upright).
//!
//! The focused text field gets a caret after its text, steady rather than
//! blinking, so a person watching a screencast can see where typing goes.
//! There is no focus ring: pages commonly turn it off with `:focus` rules,
//! which the cascade does not apply.
//!
//! A password field paints a bullet per character, never its value. Other
//! text is painted after the secret scrub, so a username shown on the page
//! appears as its placeholder in a screenshot, as it does in a snapshot.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::color::{self, Rgba};
use crate::dom::{Document, NodeData, NodeId};
use crate::font;
use crate::images::Image;
use crate::layout::{self, Layout, Rect};
use crate::style::{self, ComputedStyle, Styles};

// ---- the canvas -------------------------------------------------------------

/// An area in canvas pixels, `x0..x1` by `y0..y1`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Area {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Area {
    fn intersect(self, other: Area) -> Area {
        Area { x0: self.x0.max(other.x0), y0: self.y0.max(other.y0), x1: self.x1.min(other.x1), y1: self.y1.min(other.y1) }
    }

    fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    fn inset(self, top: f64, right: f64, bottom: f64, left: f64) -> Area {
        Area { x0: self.x0 + left, y0: self.y0 + top, x1: self.x1 - right, y1: self.y1 - bottom }
    }

    fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    /// Signed distance from a pixel centre to this box with corners rounded
    /// by `radius`: negative inside.
    fn distance(&self, px: f64, py: f64, radius: f64) -> f64 {
        let (cx, cy) = ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0);
        let qx = (px - cx).abs() - (self.width() / 2.0 - radius);
        let qy = (py - cy).abs() - (self.height() / 2.0 - radius);
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
        outside + qx.max(qy).min(0.0) - radius
    }

    /// The pixels this touches, clamped to the canvas.
    fn pixels(&self, canvas: &Canvas) -> (usize, usize, usize, usize) {
        let clamp = |v: f64, max: u32| v.max(0.0).min(max as f64) as usize;
        (
            clamp(self.x0.floor(), canvas.width),
            clamp(self.y0.floor(), canvas.height),
            clamp(self.x1.ceil(), canvas.width),
            clamp(self.y1.ceil(), canvas.height),
        )
    }
}

/// An opaque RGB image.
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    rgb: Vec<u8>,
}

/// Coverage of a pixel at signed distance `d` from an edge: one pixel of
/// anti-aliasing.
fn coverage(d: f64) -> f64 {
    (0.5 - d).clamp(0.0, 1.0)
}

impl Canvas {
    pub fn new(width: u32, height: u32, background: Rgba) -> Self {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for _ in 0..width as usize * height as usize {
            rgb.extend_from_slice(&[background.r, background.g, background.b]);
        }
        Canvas { width, height, rgb }
    }

    fn whole(&self) -> Area {
        Area { x0: 0.0, y0: 0.0, x1: self.width as f64, y1: self.height as f64 }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let i = (y as usize * self.width as usize + x as usize) * 3;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }

    pub fn to_png(&self) -> Vec<u8> {
        crate::png::encode(self.width, self.height, &self.rgb)
    }

    /// Baseline JPEG at `quality`, 1 to 100, as the live stream asks for.
    pub fn to_jpeg(&self, quality: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        let _ = image::ImageEncoder::write_image(encoder, &self.rgb, self.width, self.height, image::ExtendedColorType::Rgb8);
        out
    }

    fn blend(&mut self, x: usize, y: usize, color: Rgba, amount: f64) {
        let a = (color.a * amount).clamp(0.0, 1.0);
        if a <= 0.0 || x >= self.width as usize || y >= self.height as usize {
            return;
        }
        let i = (y * self.width as usize + x) * 3;
        for (k, c) in [color.r, color.g, color.b].into_iter().enumerate() {
            let dst = self.rgb[i + k] as f64;
            self.rgb[i + k] = (dst + (c as f64 - dst) * a).round() as u8;
        }
    }

    /// Fills `area` with its corners rounded by `radius`, within `clip`.
    fn fill(&mut self, area: Area, radius: f64, color: Rgba, clip: Area) {
        if !color.is_visible() {
            return;
        }
        let visible = area.intersect(clip);
        if visible.is_empty() {
            return;
        }
        let (x0, y0, x1, y1) = visible.pixels(self);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let amount = coverage(area.distance(px, py, radius)) * clip_coverage(clip, px, py);
                self.blend(x, y, color, amount);
            }
        }
    }

    /// The ring between `outer` and `inner`, each pixel in the colour of the
    /// side it belongs to: top, right, bottom, left.
    fn ring(&mut self, outer: Area, inner: Area, radius: (f64, f64), colors: [Rgba; 4], clip: Area) {
        let visible = outer.intersect(clip);
        if visible.is_empty() {
            return;
        }
        let widths = [inner.y0 - outer.y0, outer.x1 - inner.x1, outer.y1 - inner.y1, inner.x0 - outer.x0];
        let (x0, y0, x1, y1) = visible.pixels(self);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let outside_inner = if inner.is_empty() { 1.0 } else { 1.0 - coverage(inner.distance(px, py, radius.1)) };
                let amount = coverage(outer.distance(px, py, radius.0)) * outside_inner * clip_coverage(clip, px, py);
                if amount <= 0.0 {
                    continue;
                }
                // How far into each side's band the pixel is, relative to the
                // band's width; the deepest wins.
                let depth = [inner.y0 - py, px - inner.x1, py - inner.y1, inner.x0 - px];
                let side = (0..4)
                    .filter(|s| widths[*s] > 0.0)
                    .max_by(|a, b| (depth[*a] / widths[*a]).total_cmp(&(depth[*b] / widths[*b])))
                    .unwrap_or(0);
                self.blend(x, y, colors[side], amount);
            }
        }
    }

    /// A line `width` wide from `a` to `b`, with round ends.
    fn line(&mut self, a: (f64, f64), b: (f64, f64), width: f64, color: Rgba, clip: Area) {
        let pad = width / 2.0 + 1.0;
        let bounds = Area { x0: a.0.min(b.0) - pad, y0: a.1.min(b.1) - pad, x1: a.0.max(b.0) + pad, y1: a.1.max(b.1) + pad };
        let visible = bounds.intersect(clip);
        if visible.is_empty() {
            return;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length_sq = (dx * dx + dy * dy).max(1e-9);
        let (x0, y0, x1, y1) = visible.pixels(self);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let t = (((px - a.0) * dx + (py - a.1) * dy) / length_sq).clamp(0.0, 1.0);
                let (nx, ny) = (a.0 + t * dx - px, a.1 + t * dy - py);
                let d = (nx * nx + ny * ny).sqrt() - width / 2.0;
                self.blend(x, y, color, coverage(d) * clip_coverage(clip, px, py));
            }
        }
    }

    /// One character with its pen at `(x, baseline)`, in canvas pixels.
    fn glyph(&mut self, c: char, size: f64, x: f64, baseline: f64, color: Rgba, clip: Area) {
        match font::draw(c, size, x, baseline) {
            font::Drawn::Outline(glyph) => {
                let bounds = glyph.px_bounds();
                let (left, top) = (bounds.min.x as i64, bounds.min.y as i64);
                glyph.draw(|gx, gy, amount| {
                    let (x, y) = (left + gx as i64, top + gy as i64);
                    if x < 0 || y < 0 {
                        return;
                    }
                    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                    self.blend(x as usize, y as usize, color, amount as f64 * clip_coverage(clip, px, py));
                });
            }
            font::Drawn::Missing { width, height } => {
                let (ascent, _) = font::ascent_descent(size);
                let top = baseline - ascent * 0.8;
                let outer = Area { x0: x + width * 0.1, y0: top, x1: x + width * 0.1 + width, y1: top + height };
                let stroke = (size / 16.0).max(1.0);
                self.ring(outer, outer.inset(stroke, stroke, stroke, stroke), (0.0, 0.0), [color; 4], clip);
            }
            font::Drawn::Nothing => {}
        }
    }
}

/// 1 inside the clip, 0 outside. Clips are pixel-aligned in practice, so
/// there is no partial coverage at their edges.
fn clip_coverage(clip: Area, px: f64, py: f64) -> f64 {
    if px >= clip.x0 && px < clip.x1 && py >= clip.y0 && py < clip.y1 {
        1.0
    } else {
        0.0
    }
}

// ---- the document -----------------------------------------------------------

/// The part of the page a screenshot shows, in CSS pixels, and how many
/// device pixels each takes.
#[derive(Debug, Clone, Copy)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// Chrome's colours for controls the page has not styled.
const CONTROL_BORDER: Rgba = Rgba::rgb(0x76, 0x76, 0x76);
const BUTTON_FACE: Rgba = Rgba::rgb(0xef, 0xef, 0xef);
const PLACEHOLDER: Rgba = Rgba::rgb(0x75, 0x75, 0x75);
const ACCENT: Rgba = Rgba::rgb(0x00, 0x75, 0xff);

/// Paints `region` of the document, with a caret in `focus` if it takes
/// text. `scrub` sees every piece of text before it is drawn.
pub fn paint(doc: &Document, region: Region, focus: Option<NodeId>, scrub: &dyn Fn(&str) -> String) -> Canvas {
    let styles = style::document_styles(doc);
    let layout = layout::document_layout(doc);
    let width = (region.width * region.scale).round().max(1.0) as u32;
    let height = (region.height * region.scale).round().max(1.0) as u32;
    let (background, propagated) = canvas_background(doc, &styles);
    let mut painter = Painter {
        doc,
        styles: &styles,
        layout: &layout,
        region,
        canvas: Canvas::new(width, height, Rgba::WHITE),
        opacity: HashMap::new(),
        focus,
        scrub,
    };
    let whole = painter.canvas.whole();
    painter.canvas.fill(whole, 0.0, background, whole);
    for id in layout.paint_order() {
        painter.element(*id, propagated.contains(id));
    }
    painter.canvas
}

/// The canvas colour: the root's background, or the body's when the root
/// has none, as CSS propagates it. The elements it came from do not paint
/// it again.
fn canvas_background(doc: &Document, styles: &Styles) -> (Rgba, Vec<NodeId>) {
    let root = doc.children(doc.document_id()).iter().copied().find(|c| doc.is_element(*c));
    let body = root.and_then(|r| doc.children(r).iter().copied().find(|c| doc.is_tag(*c, "body")));
    let mut from = Vec::new();
    for id in [root, body].into_iter().flatten() {
        from.push(id);
        if let Some(color) = styles.get(id).and_then(|s| background_color(s)).filter(Rgba::is_visible) {
            // Behind it, the canvas is white.
            let mut canvas = Canvas::new(1, 1, Rgba::WHITE);
            canvas.blend(0, 0, color, 1.0);
            let [r, g, b] = canvas.pixel(0, 0);
            return (Rgba::rgb(r, g, b), from);
        }
    }
    (Rgba::WHITE, from)
}

fn text_color(style: &ComputedStyle) -> Rgba {
    color::parse(style.get("color"), Rgba::BLACK).unwrap_or(Rgba::BLACK)
}

fn background_color(style: &ComputedStyle) -> Option<Rgba> {
    color::parse(style.get("background-color"), text_color(style))
}

struct Painter<'a> {
    doc: &'a Document,
    styles: &'a Styles,
    layout: &'a Layout,
    region: Region,
    canvas: Canvas,
    /// Effective opacity, the product of every ancestor's, by element.
    opacity: HashMap<NodeId, f64>,
    focus: Option<NodeId>,
    scrub: &'a dyn Fn(&str) -> String,
}

/// Border widths, top, right, bottom, left, in CSS pixels.
fn border_widths(style: &ComputedStyle) -> [f64; 4] {
    ["top", "right", "bottom", "left"].map(|side| {
        if matches!(style.get(&format!("border-{side}-style")), "none" | "hidden" | "") {
            return 0.0;
        }
        match style.get(&format!("border-{side}-width")) {
            "thin" => 1.0,
            "medium" | "" => 3.0,
            "thick" => 5.0,
            other => style::length_px(other, style.font_size(), 16.0, 0.0).unwrap_or(0.0),
        }
    })
}

fn padding(style: &ComputedStyle, width: f64) -> [f64; 4] {
    ["top", "right", "bottom", "left"]
        .map(|side| style::length_px(style.get(&format!("padding-{side}")), style.font_size(), 16.0, width).unwrap_or(0.0))
}

/// The corner radius, from the first corner set; corners are painted alike.
fn radius(style: &ComputedStyle, rect: &Rect) -> f64 {
    let value = [style.get("border-radius"), style.get("border-top-left-radius")]
        .into_iter()
        .find(|v| !v.is_empty())
        .unwrap_or("");
    let first = value.split(|c: char| c.is_whitespace() || c == '/').find(|s| !s.is_empty()).unwrap_or("0");
    let px = style::length_px(first, style.font_size(), 16.0, rect.width.min(rect.height)).unwrap_or(0.0);
    px.clamp(0.0, rect.width.min(rect.height) / 2.0)
}

/// Whether the page left a control's look alone, so Chrome would draw its
/// own: no border, no background and no `appearance: none`. A page that
/// sets `border: none; background: transparent` on purpose gets Chrome's
/// look here; that cannot be told apart without knowing where a value came
/// from.
fn native_look(style: &ComputedStyle) -> bool {
    let appearance = [style.get("appearance"), style.get("-webkit-appearance")];
    !appearance.contains(&"none")
        && border_widths(style).iter().all(|w| *w == 0.0)
        && !background_color(style).is_some_and(|c| c.is_visible())
}

impl Painter<'_> {
    /// A page rectangle in canvas pixels.
    fn area(&self, rect: &Rect) -> Area {
        let r = &self.region;
        Area {
            x0: (rect.x - r.x) * r.scale,
            y0: (rect.y - r.y) * r.scale,
            x1: (rect.x + rect.width - r.x) * r.scale,
            y1: (rect.y + rect.height - r.y) * r.scale,
        }
    }

    fn effective_opacity(&mut self, id: NodeId) -> f64 {
        if let Some(o) = self.opacity.get(&id) {
            return *o;
        }
        let own = self
            .styles
            .get(id)
            .and_then(|s| s.get("opacity").trim().parse::<f64>().ok())
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        let parent = self.doc.parent(id).filter(|p| self.doc.is_element(*p)).map(|p| self.effective_opacity(p)).unwrap_or(1.0);
        self.opacity.insert(id, own * parent);
        own * parent
    }

    fn element(&mut self, id: NodeId, background_propagated: bool) {
        let (Some(style), Some(rect)) = (self.styles.get(id), self.layout.rect(id)) else { return };
        if matches!(style.get("visibility"), "hidden" | "collapse") {
            return;
        }
        let opacity = self.effective_opacity(id);
        if opacity <= 0.0 {
            return;
        }
        let mut clip = self.canvas.whole();
        for ancestor in self.layout.clips_of(id) {
            if let Some(r) = self.layout.rect(*ancestor) {
                clip = clip.intersect(self.area(&r));
            }
        }
        if clip.is_empty() {
            return;
        }
        let area = self.area(&rect);
        let scale = self.region.scale;
        let tag = self.doc.tag(id).unwrap_or("");
        let control = matches!(tag, "input" | "button" | "select" | "textarea");
        let native = control && native_look(style);
        let corner = radius(style, &rect) * scale;

        let shape = Shape { area, radius: corner };
        // A colour the canvas took over is not painted again; images are.
        if !background_propagated {
            if let Some(bg) = background_color(style) {
                self.canvas.fill(area, corner, bg.with_alpha(opacity), clip);
            }
        }
        self.background_layers(style, &rect, opacity, clip, shape);
        if tag == "img" {
            self.replaced_image(id, style, &rect, opacity, clip, shape);
        }
        let widths = border_widths(style).map(|w| w * scale);
        if widths.iter().any(|w| *w > 0.0) {
            let current = text_color(style);
            let colors = ["top", "right", "bottom", "left"].map(|side| {
                color::parse(style.get(&format!("border-{side}-color")), current).unwrap_or(current).with_alpha(opacity)
            });
            let inner = area.inset(widths[0], widths[1], widths[2], widths[3]);
            let inner_radius = (corner - widths.iter().cloned().fold(0.0, f64::max)).max(0.0);
            self.canvas.ring(area, inner, (corner, inner_radius), colors, clip);
        }

        if control {
            self.control(id, tag, style, &rect, native, opacity, clip);
        } else {
            if style.get("display") == "list-item" {
                self.list_marker(id, style, &rect, opacity, clip);
            }
            let children: Vec<NodeId> = self.doc.children(id).to_vec();
            for child in children {
                if matches!(self.doc.node(child).data, NodeData::Text) {
                    if let Some(text_rect) = self.layout.rect(child) {
                        let text = self.doc.node(child).text.clone();
                        self.text(&text, style, &text_rect, opacity, clip);
                    }
                }
            }
        }
    }

    /// A text node's lines, broken where layout broke them.
    fn text(&mut self, original: &str, style: &ComputedStyle, rect: &Rect, opacity: f64, clip: Area) {
        // The whole text is scrubbed before it is broken into lines: broken
        // first, a secret split across two lines would match in neither.
        let text = (self.scrub)(original);
        let text = match style.get("text-transform") {
            "uppercase" => text.to_uppercase(),
            "lowercase" => text.to_lowercase(),
            _ => text,
        };
        let measure = layout::measure_with(style);
        let width_of = |s: &str| s.chars().map(&measure).sum::<f64>();
        let space = measure(' ');
        // Layout measured the original; a placeholder is usually longer, and
        // gets the extra room, so it does not wrap where the value did not.
        let room = rect.width + (width_of(&text) - width_of(original)).max(0.0);
        let lines: Vec<String> = match style.get("white-space") {
            "pre" | "pre-wrap" | "break-spaces" => text.split('\n').map(str::to_string).collect(),
            white_space => {
                let words: Vec<&str> = text.split_whitespace().collect();
                if words.is_empty() {
                    return;
                }
                let widths: Vec<f64> = words.iter().map(|w| width_of(w)).collect();
                let starts = if matches!(white_space, "nowrap") { vec![0] } else { layout::line_starts(&widths, space, room) };
                starts
                    .iter()
                    .enumerate()
                    .map(|(n, start)| words[*start..*starts.get(n + 1).unwrap_or(&words.len())].join(" "))
                    .collect()
            }
        };
        let line_height = layout::line_height(style);
        for (n, line) in lines.iter().enumerate() {
            let line_width = width_of(line);
            let offset = match style.get("text-align") {
                "center" | "-webkit-center" => (rect.width - line_width) / 2.0,
                "right" | "end" | "-webkit-right" => rect.width - line_width,
                _ => 0.0,
            }
            .max(0.0);
            let top = rect.y + n as f64 * line_height;
            self.line_of_text(line, style, rect.x + offset, top, line_height, text_color(style).with_alpha(opacity), clip);
        }
    }

    /// One line of text in `style`, its line box's top-left at page `(x,
    /// top)`, with the glyphs centred in the line height as CSS does.
    #[allow(clippy::too_many_arguments)]
    fn line_of_text(&mut self, line: &str, style: &ComputedStyle, x: f64, top: f64, line_height: f64, color: Rgba, clip: Area) {
        let size = style.font_size();
        let (ascent, descent) = font::ascent_descent(size);
        let baseline = top + (line_height - (ascent + descent)) / 2.0 + ascent;
        let measure = layout::measure_with(style);
        let monospace = style.get("font-family").contains("monospace");
        let weight = style.get("font-weight");
        let bold = matches!(weight, "bold" | "bolder") || weight.parse::<u32>().is_ok_and(|w| w >= 600);
        let scale = self.region.scale;
        let (r, s) = (self.region.x, self.region.y);
        let to_canvas = |px: f64, py: f64| ((px - r) * scale, (py - s) * scale);
        let mut pen = x;
        for c in line.chars() {
            let advance = measure(c);
            // Monospace cells are 0.6em; the glyph sits in the middle of one.
            let inset = if monospace { (advance - font::advance(c, size, false)) / 2.0 } else { 0.0 };
            let (cx, cy) = to_canvas(pen + inset, baseline);
            self.canvas.glyph(c, size * scale, cx, cy, color, clip);
            if bold {
                // No bold face: the regular one, drawn again a little to the right.
                self.canvas.glyph(c, size * scale, cx + (size * scale / 24.0).max(0.6), cy, color, clip);
            }
            pen += advance;
        }
        let decoration = format!("{} {}", style.get("text-decoration"), style.get("text-decoration-line"));
        let thickness = (size / 14.0).max(1.0) * scale;
        for (wanted, y) in [("underline", baseline + descent * 0.45), ("line-through", baseline - ascent * 0.3)] {
            if decoration.contains(wanted) {
                let (a, b) = (to_canvas(x, y), to_canvas(pen, y));
                self.canvas.line(a, b, thickness, color, clip);
            }
        }
    }

    fn list_marker(&mut self, id: NodeId, style: &ComputedStyle, rect: &Rect, opacity: f64, clip: Area) {
        let kind = match style.get("list-style-type") {
            "" => {
                let parent = self.doc.parent(id);
                if parent.is_some_and(|p| self.doc.is_tag(p, "ol")) { "decimal" } else { "disc" }
            }
            other => other,
        };
        let size = style.font_size();
        let line_height = layout::line_height(style);
        let color = text_color(style).with_alpha(opacity);
        let scale = self.region.scale;
        match kind {
            "none" => {}
            "decimal" => {
                let position = self.doc.children(self.doc.parent(id).unwrap_or(id)).iter().filter(|c| self.doc.is_tag(**c, "li")).position(|c| *c == id).unwrap_or(0);
                let label = format!("{}. ", position + 1);
                let width: f64 = label.chars().map(layout::measure_with(style)).sum();
                self.line_of_text(&label, style, rect.x - width, rect.y, line_height, color, clip);
            }
            _ => {
                let d = size * 0.35;
                let (cx, cy) = (rect.x - size * 0.75, rect.y + line_height / 2.0);
                let dot = self.area(&Rect { x: cx - d / 2.0, y: cy - d / 2.0, width: d, height: d });
                if kind == "square" {
                    self.canvas.fill(dot, 0.0, color, clip);
                } else if kind == "circle" {
                    let w = (size / 16.0).max(1.0) * scale;
                    self.canvas.ring(dot, dot.inset(w, w, w, w), (d * scale / 2.0, d * scale / 2.0 - w), [color; 4], clip);
                } else {
                    self.canvas.fill(dot, d * scale / 2.0, color, clip);
                }
            }
        }
    }
}

// ---- form controls ----------------------------------------------------------

impl Painter<'_> {
    /// A control's face, when the page left it to the browser, and what it
    /// holds: a value, a placeholder, bullets for a password, a check.
    #[allow(clippy::too_many_arguments)]
    fn control(&mut self, id: NodeId, tag: &str, style: &ComputedStyle, rect: &Rect, native: bool, opacity: f64, clip: Area) {
        let kind = match tag {
            "input" => self.doc.attr(id, "type").unwrap_or("text").trim().to_ascii_lowercase(),
            other => other.to_string(),
        };
        let scale = self.region.scale;
        let area = self.area(rect);
        let face = |painter: &mut Self, fill: Rgba| {
            painter.canvas.fill(area, 2.0 * scale, fill.with_alpha(opacity), clip);
            let w = scale;
            painter.canvas.ring(area, area.inset(w, w, w, w), (2.0 * scale, scale), [CONTROL_BORDER.with_alpha(opacity); 4], clip);
        };
        let color = text_color(style).with_alpha(opacity);
        match kind.as_str() {
            "hidden" => {}
            "checkbox" | "radio" => {
                if [style.get("appearance"), style.get("-webkit-appearance")].contains(&"none") {
                    return;
                }
                self.toggle(id, kind == "radio", rect, opacity, clip);
            }
            "button" | "submit" | "reset" if tag == "input" => {
                if native {
                    face(self, BUTTON_FACE);
                }
                let label = self.doc.attr(id, "value").map(str::to_string).unwrap_or_else(|| {
                    if kind == "reset" { "Reset".into() } else if kind == "submit" { "Submit".into() } else { String::new() }
                });
                self.single_line(&label, style, rect, true, color, clip);
            }
            "button" => {
                if native {
                    face(self, BUTTON_FACE);
                }
                // Its content is laid out like any element's.
                for child in self.doc.children(id).to_vec() {
                    if matches!(self.doc.node(child).data, NodeData::Text) {
                        if let Some(text_rect) = self.layout.rect(child) {
                            let text = self.doc.node(child).text.clone();
                            self.text(&text, style, &text_rect, opacity, clip);
                        }
                    }
                }
            }
            "select" => {
                if native {
                    face(self, Rgba::WHITE);
                }
                let options: Vec<NodeId> = self.doc.descendants(id).filter(|o| self.doc.is_tag(*o, "option")).collect();
                let chosen = options
                    .iter()
                    .copied()
                    .find(|o| self.doc.control(*o).selected.unwrap_or_else(|| self.doc.has_attr(*o, "selected")))
                    .or(options.first().copied());
                let label = chosen.map(|o| self.doc.text_content(o).trim().to_string()).unwrap_or_default();
                let arrow = 20.0;
                let text_rect = Rect { width: (rect.width - arrow).max(0.0), ..*rect };
                self.single_line(&label, style, &text_rect, false, color, clip);
                // A chevron in the last 20px.
                let (cx, cy) = (rect.x + rect.width - arrow / 2.0 - 2.0, rect.y + rect.height / 2.0);
                let to = |dx: f64, dy: f64| ((cx + dx - self.region.x) * scale, (cy + dy - self.region.y) * scale);
                let (a, b, c) = (to(-3.5, -1.5), to(0.0, 2.0), to(3.5, -1.5));
                self.canvas.line(a, b, 1.5 * scale, color, clip);
                self.canvas.line(b, c, 1.5 * scale, color, clip);
            }
            "textarea" => {
                if native {
                    face(self, Rgba::WHITE);
                }
                let value = self.doc.current_value(id);
                let inner = self.content_rect(style, rect, native);
                let inner_clip = clip.intersect(self.area(&inner));
                let line_height = layout::line_height(style);
                if value.is_empty() {
                    if let Some(placeholder) = self.doc.attr(id, "placeholder").map(|p| (self.scrub)(p)) {
                        let gray = PLACEHOLDER.with_alpha(opacity);
                        for (n, line) in placeholder.split('\n').enumerate() {
                            self.line_of_text(line, style, inner.x, inner.y + n as f64 * line_height, line_height, gray, inner_clip);
                        }
                    }
                    return;
                }
                for (n, line) in value.split('\n').enumerate() {
                    let line = (self.scrub)(line);
                    self.line_of_text(&line, style, inner.x, inner.y + n as f64 * line_height, line_height, color, inner_clip);
                }
            }
            "range" => {
                let (cy, x0, x1) = (rect.y + rect.height / 2.0, rect.x + 8.0, rect.x + rect.width - 8.0);
                let to = |x: f64, y: f64| ((x - self.region.x) * scale, (y - self.region.y) * scale);
                self.canvas.line(to(x0, cy), to(x1, cy), 4.0 * scale, Rgba::rgb(0xef, 0xef, 0xef).with_alpha(opacity), clip);
                let thumb = Rect { x: (x0 + x1) / 2.0 - 8.0, y: cy - 8.0, width: 16.0, height: 16.0 };
                let thumb = self.area(&thumb);
                self.canvas.fill(thumb, 8.0 * scale, ACCENT.with_alpha(opacity), clip);
            }
            _ => {
                if native {
                    face(self, Rgba::WHITE);
                }
                let value = self.doc.current_value(id);
                let shown = if value.is_empty() {
                    if let Some(placeholder) = self.doc.attr(id, "placeholder").map(str::to_string) {
                        let placeholder = (self.scrub)(&placeholder);
                        self.single_line(&placeholder, style, rect, false, PLACEHOLDER.with_alpha(opacity), clip);
                    }
                    String::new()
                } else if kind == "password" {
                    // Never the value: a bullet for each character.
                    "\u{2022}".repeat(value.chars().count())
                } else {
                    (self.scrub)(&value)
                };
                self.single_line(&shown, style, rect, false, color, clip);
                if self.focus == Some(id) {
                    self.caret(&shown, style, rect, color, clip);
                }
            }
        }
    }

    /// The content box of a control: inside its border and padding, or
    /// inside Chrome's own when the page left them alone.
    fn content_rect(&self, style: &ComputedStyle, rect: &Rect, native: bool) -> Rect {
        let border = border_widths(style);
        let pad = padding(style, rect.width);
        let (extra_x, extra_y) = if native { (3.0, 2.0) } else { (0.0, 0.0) };
        let left = border[3] + pad[3] + extra_x;
        let top = border[0] + pad[0] + extra_y;
        Rect {
            x: rect.x + left,
            y: rect.y + top,
            width: (rect.width - left - border[1] - pad[1] - extra_x).max(0.0),
            height: (rect.height - top - border[2] - pad[2] - extra_y).max(0.0),
        }
    }

    /// One line in a control, centred vertically, clipped to the content
    /// box, and centred horizontally when `centre` is set or the page asks.
    fn single_line(&mut self, text: &str, style: &ComputedStyle, rect: &Rect, centre: bool, color: Rgba, clip: Area) {
        let (x, top, height, inner_clip) = self.line_position(text, style, rect, centre, clip);
        self.line_of_text(text, style, x, top, height, color, inner_clip);
    }

    /// Where [`single_line`](Self::single_line) sets `text`: its left edge,
    /// the top and height of its line, and the clip of the content box.
    fn line_position(&self, text: &str, style: &ComputedStyle, rect: &Rect, centre: bool, clip: Area) -> (f64, f64, f64, Area) {
        let native = native_look(style);
        let inner = self.content_rect(style, rect, native);
        let inner_clip = clip.intersect(self.area(&inner));
        let (ascent, descent) = font::ascent_descent(style.font_size());
        let width: f64 = text.chars().map(layout::measure_with(style)).sum();
        let offset = match style.get("text-align") {
            _ if centre => (inner.width - width) / 2.0,
            "center" | "-webkit-center" => (inner.width - width) / 2.0,
            "right" | "end" | "-webkit-right" => inner.width - width,
            _ => 0.0,
        }
        .max(0.0);
        let height = ascent + descent;
        (inner.x + offset, inner.y + (inner.height - height) / 2.0, height, inner_clip)
    }

    /// A caret after `text` in a single-line field.
    fn caret(&mut self, text: &str, style: &ComputedStyle, rect: &Rect, color: Rgba, clip: Area) {
        let (x, top, height, inner_clip) = self.line_position(text, style, rect, false, clip);
        let end = x + text.chars().map(layout::measure_with(style)).sum::<f64>();
        let scale = self.region.scale;
        let to = |px: f64, py: f64| ((px - self.region.x) * scale, (py - self.region.y) * scale);
        // A little inside the font's height, as Chrome draws it.
        let (a, b) = (to(end + 0.5, top + height * 0.1), to(end + 0.5, top + height * 0.9));
        self.canvas.line(a, b, scale, color, inner_clip.inset(0.0, -2.0 * scale, 0.0, 0.0));
    }

    /// A checkbox or radio button as Chrome draws it, 13px, centred in its box.
    fn toggle(&mut self, id: NodeId, radio: bool, rect: &Rect, opacity: f64, clip: Area) {
        let scale = self.region.scale;
        let side = 13.0f64.min(rect.width).min(rect.height);
        let boxed = Rect { x: rect.x + (rect.width - side) / 2.0, y: rect.y + (rect.height - side) / 2.0, width: side, height: side };
        let area = self.area(&boxed);
        let round = if radio { side * scale / 2.0 } else { 2.0 * scale };
        let checked = self.doc.current_checked(id);
        let w = scale;
        if !checked {
            self.canvas.fill(area, round, Rgba::WHITE.with_alpha(opacity), clip);
            self.canvas.ring(area, area.inset(w, w, w, w), (round, (round - w).max(0.0)), [CONTROL_BORDER.with_alpha(opacity); 4], clip);
            return;
        }
        self.canvas.fill(area, round, ACCENT.with_alpha(opacity), clip);
        if radio {
            let gap = area.inset(2.0 * w, 2.0 * w, 2.0 * w, 2.0 * w);
            self.canvas.fill(gap, gap.width() / 2.0, Rgba::WHITE.with_alpha(opacity), clip);
            let dot = area.inset(4.0 * w, 4.0 * w, 4.0 * w, 4.0 * w);
            self.canvas.fill(dot, dot.width() / 2.0, ACCENT.with_alpha(opacity), clip);
        } else {
            let to = |x: f64, y: f64| (area.x0 + x * area.width() / 13.0, area.y0 + y * area.height() / 13.0);
            let white = Rgba::WHITE.with_alpha(opacity);
            self.canvas.line(to(3.0, 6.8), to(5.6, 9.4), 1.8 * scale, white, clip);
            self.canvas.line(to(5.6, 9.4), to(10.2, 4.0), 1.8 * scale, white, clip);
        }
    }
}


// ---- backgrounds ------------------------------------------------------------

/// One layer of `background-image`.
#[derive(Debug, Clone, PartialEq)]
enum Layer {
    Url(String),
    Gradient(Gradient),
}

#[derive(Debug, Clone, PartialEq)]
struct Gradient {
    radial: bool,
    /// Direction of a linear gradient, CSS degrees: 0 is up, 90 right.
    angle: f64,
    /// Colours and where they sit, 0 to 1 along the gradient.
    stops: Vec<(Rgba, f64)>,
}

/// What `background-*` says, for the first layer's size, position and
/// repeat; further layers reuse them.
#[derive(Debug, Default)]
struct Background {
    layers: Vec<Layer>,
    size: String,
    position: String,
    repeat: String,
}

/// Splits on `sep` outside parentheses.
fn split_outside_parens(value: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(value[start..i].trim());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(value[start..].trim());
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// Whitespace-separated tokens outside parentheses.
fn tokens(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, None);
    for (i, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(&value[s..i]);
                }
                continue;
            }
            _ => {}
        }
        if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&value[s..]);
    }
    out
}

fn is_image_token(token: &str) -> bool {
    token.starts_with("url(") || token.contains("gradient(")
}

fn parse_layer(token: &str, current: Rgba) -> Option<Layer> {
    if let Some(inner) = token.strip_prefix("url(").and_then(|t| t.strip_suffix(')')) {
        let url = inner.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        return (!url.is_empty()).then_some(Layer::Url(url));
    }
    let (name, args) = token.split_once('(')?;
    let args = args.strip_suffix(')')?;
    let radial = name.contains("radial");
    if !radial && !name.contains("linear") {
        return None;
    }
    let mut parts = split_outside_parens(args, ',');
    let mut angle = 180.0;
    if let Some(first) = parts.first().copied() {
        if color::parse(tokens(first).first().copied().unwrap_or(""), current).is_none() {
            parts.remove(0);
            if !radial {
                angle = gradient_angle(first)?;
            }
        }
    }
    let mut stops: Vec<(Rgba, Option<f64>)> = Vec::new();
    for part in parts {
        let words = tokens(part);
        let color = color::parse(words.first()?, current)?;
        let positions: Vec<f64> =
            words[1..].iter().filter_map(|w| w.strip_suffix('%').and_then(|p| p.parse::<f64>().ok())).map(|p| p / 100.0).collect();
        if positions.is_empty() {
            stops.push((color, None));
        }
        for p in positions {
            stops.push((color, Some(p)));
        }
    }
    if stops.is_empty() {
        return None;
    }
    // Unplaced stops: the ends at 0 and 1, the rest spread evenly between
    // their placed neighbours.
    let n = stops.len();
    if stops[0].1.is_none() {
        stops[0].1 = Some(0.0);
    }
    if stops[n - 1].1.is_none() {
        stops[n - 1].1 = Some(1.0);
    }
    let mut i = 0;
    while i < n {
        if stops[i].1.is_some() {
            i += 1;
            continue;
        }
        let before = stops[i - 1].1.unwrap_or(0.0);
        let j = (i..n).find(|k| stops[*k].1.is_some()).unwrap_or(n - 1);
        let after = stops[j].1.unwrap_or(1.0);
        for (k, stop) in stops.iter_mut().enumerate().take(j).skip(i) {
            stop.1 = Some(before + (after - before) * (k - i + 1) as f64 / (j - i + 1) as f64);
        }
        i = j;
    }
    let mut placed: Vec<(Rgba, f64)> = Vec::new();
    for (color, at) in stops {
        let at = at.unwrap_or(0.0);
        // A stop before the previous one moves up to it.
        let at = placed.last().map(|(_, prev)| at.max(*prev)).unwrap_or(at);
        placed.push((color, at));
    }
    Some(Layer::Gradient(Gradient { radial, angle, stops: placed }))
}

/// `45deg`, `0.25turn`, `to right`, `to top left`.
fn gradient_angle(value: &str) -> Option<f64> {
    let value = value.trim();
    if let Some(side) = value.strip_prefix("to ") {
        let has = |w: &str| side.split_whitespace().any(|s| s == w);
        let x = if has("right") { 1.0 } else if has("left") { -1.0 } else { 0.0 };
        let y = if has("top") { 1.0 } else if has("bottom") { -1.0 } else { 0.0 };
        if x == 0.0 && y == 0.0 {
            return None;
        }
        return Some(f64::atan2(x, y).to_degrees());
    }
    let number = |suffix: &str| value.strip_suffix(suffix).and_then(|n| n.trim().parse::<f64>().ok());
    number("deg")
        .or_else(|| number("turn").map(|t| t * 360.0))
        .or_else(|| number("grad").map(|g| g * 0.9))
        .or_else(|| number("rad").map(f64::to_degrees))
        .or_else(|| (value == "0").then_some(0.0))
}

fn background(style: &ComputedStyle) -> Background {
    let current = text_color(style);
    let longhand = |name: &str| Some(style.get(name)).filter(|v| !v.is_empty() && *v != "initial").map(str::to_string);
    let mut out = Background::default();
    // The shorthand's layers, size, position and repeat; longhands set on
    // their own (an inline `background-image`, say) override its parts.
    for (n, layer) in split_outside_parens(style.get("background"), ',').into_iter().enumerate() {
        let words = tokens(layer);
        if let Some(found) = words.iter().find(|w| is_image_token(w)).and_then(|w| parse_layer(w, current)) {
            out.layers.push(found);
        }
        if n > 0 {
            continue;
        }
        // The first layer's size (after a slash), repeat and position.
        let mut position = Vec::new();
        let mut after_slash = false;
        for word in words {
            if word == "/" {
                after_slash = true;
            } else if let Some(size) = word.strip_prefix('/') {
                after_slash = true;
                if !size.is_empty() {
                    out.size = size.to_string();
                }
            } else if after_slash {
                out.size = if out.size.is_empty() { word.to_string() } else { format!("{} {word}", out.size) };
            } else if word.contains("repeat") || matches!(word, "space" | "round") {
                out.repeat = word.to_string();
            } else if matches!(word, "left" | "right" | "top" | "bottom" | "center")
                || word.ends_with('%')
                || word.ends_with("px")
                || word == "0"
            {
                position.push(word);
            }
        }
        out.position = position.join(" ");
    }
    if let Some(image) = longhand("background-image") {
        out.layers = if image == "none" {
            Vec::new()
        } else {
            split_outside_parens(&image, ',').into_iter().filter_map(|l| parse_layer(l, current)).collect()
        };
    }
    if let Some(size) = longhand("background-size") {
        out.size = size;
    }
    if let Some(position) = longhand("background-position") {
        out.position = position;
    }
    if let Some(repeat) = longhand("background-repeat") {
        out.repeat = repeat;
    }
    out
}

/// The background images of every element with a box, against the
/// document's URL. Stylesheets' `url()`s were made absolute when they
/// loaded, so this only matters for inline styles.
pub fn background_urls(doc: &Document) -> Vec<String> {
    let styles = style::document_styles(doc);
    let layout = layout::document_layout(doc);
    let base = url::Url::parse(doc.url()).ok();
    let mut urls = Vec::new();
    for id in layout.paint_order() {
        let Some(style) = styles.get(*id) else { continue };
        if !style.get("background").contains("url(") && !style.get("background-image").contains("url(") {
            continue;
        }
        for layer in background(style).layers {
            if let Layer::Url(url) = layer {
                let absolute = match &base {
                    Some(base) => base.join(&url).ok().map(String::from),
                    None => url::Url::parse(&url).ok().map(String::from),
                };
                urls.extend(absolute);
            }
        }
    }
    urls
}

/// A length in a `background-size` or `-position`, against `basis` for
/// percentages; `None` for `auto` and keywords.
fn bg_length(value: &str, basis: f64, style: &ComputedStyle) -> Option<f64> {
    style::length_px(value, style.font_size(), 16.0, basis)
}

/// Where a `size` image goes in an `area` for one axis's position words:
/// a keyword with an optional offset, or a length or percentage.
fn place(words: &[&str], area: f64, size: f64, style: &ComputedStyle, start_word: &str, end_word: &str) -> f64 {
    let free = area - size;
    match words {
        [] => free / 2.0,
        [w] if *w == start_word => 0.0,
        [w] if *w == end_word => free,
        [w] if *w == "center" => free / 2.0,
        [w] => match w.strip_suffix('%') {
            Some(p) => p.parse::<f64>().map(|p| free * p / 100.0).unwrap_or(free / 2.0),
            None => bg_length(w, area, style).unwrap_or(free / 2.0),
        },
        [edge, offset, ..] => {
            let offset = match offset.strip_suffix('%') {
                Some(p) => p.parse::<f64>().map(|p| area * p / 100.0).unwrap_or(0.0),
                None => bg_length(offset, area, style).unwrap_or(0.0),
            };
            if *edge == end_word { free - offset } else { offset }
        }
    }
}

/// Splits `background-position` into its horizontal and vertical words.
fn position_axes(position: &str) -> (Vec<&str>, Vec<&str>) {
    let words: Vec<&str> = position.split_whitespace().collect();
    let keyword = |w: &str| matches!(w, "left" | "right" | "top" | "bottom" | "center");
    if words.iter().all(|w| !keyword(w)) {
        // Lengths only: x then y.
        return (words.first().map(|w| vec![*w]).unwrap_or_default(), words.get(1).map(|w| vec![*w]).unwrap_or_default());
    }
    let (mut x, mut y) = (Vec::new(), Vec::new());
    let mut i = 0;
    let mut centres = Vec::new();
    while i < words.len() {
        let word = words[i];
        let offset = words.get(i + 1).filter(|w| !keyword(w)).copied();
        let group: Vec<&str> = std::iter::once(word).chain(offset).collect();
        i += group.len();
        match word {
            "left" | "right" => x = group,
            "top" | "bottom" => y = group,
            "center" => centres.push(group),
            // A bare length: x when first, else y.
            _ if x.is_empty() && y.is_empty() => x = group,
            _ => y = group,
        }
    }
    for centre in centres {
        if x.is_empty() {
            x = centre;
        } else if y.is_empty() {
            y = centre;
        }
    }
    (x, y)
}

impl Gradient {
    fn color_at(&self, t: f64) -> Rgba {
        let t = t.clamp(0.0, 1.0);
        let first = self.stops[0];
        if t <= first.1 {
            return first.0;
        }
        for pair in self.stops.windows(2) {
            let ((a, at), (b, bt)) = (pair[0], pair[1]);
            if t <= bt {
                let f = if bt > at { (t - at) / (bt - at) } else { 1.0 };
                let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * f).round() as u8;
                return Rgba { r: mix(a.r, b.r), g: mix(a.g, b.g), b: mix(a.b, b.b), a: a.a + (b.a - a.a) * f };
            }
        }
        self.stops[self.stops.len() - 1].0
    }
}



// ---- images -----------------------------------------------------------------

/// A shape to stay inside: a box with rounded corners.
#[derive(Clone, Copy)]
struct Shape {
    area: Area,
    radius: f64,
}

impl Shape {
    fn coverage(&self, px: f64, py: f64) -> f64 {
        coverage(self.area.distance(px, py, self.radius))
    }
}

impl Canvas {
    /// `image` stretched over `dest`, within `clip` and `shape`.
    fn image(&mut self, image: &Image, dest: Area, opacity: f64, clip: Area, shape: Shape) {
        let visible = dest.intersect(clip).intersect(shape.area);
        if visible.is_empty() || image.width == 0 || image.height == 0 {
            return;
        }
        let (sx, sy) = (image.width as f64 / dest.width(), image.height as f64 / dest.height());
        // Shrinking by more than half: average a few samples per pixel, so
        // detail does not alias into noise.
        let samples = sx.max(sy).ceil().clamp(1.0, 4.0) as usize;
        let (x0, y0, x1, y1) = visible.pixels(self);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let amount = clip_coverage(clip, px, py) * shape.coverage(px, py);
                if amount <= 0.0 {
                    continue;
                }
                let mut sum = [0.0f64; 4];
                for j in 0..samples {
                    for i in 0..samples {
                        let fx = px - 0.5 + (i as f64 + 0.5) / samples as f64;
                        let fy = py - 0.5 + (j as f64 + 0.5) / samples as f64;
                        let p = sample(image, (fx - dest.x0) * sx - 0.5, (fy - dest.y0) * sy - 0.5);
                        for k in 0..4 {
                            sum[k] += p[k];
                        }
                    }
                }
                let n = (samples * samples) as f64;
                let alpha = sum[3] / n;
                if alpha <= 0.0 {
                    continue;
                }
                // Back from premultiplied.
                let color = Rgba {
                    r: (sum[0] / n / alpha).round().clamp(0.0, 255.0) as u8,
                    g: (sum[1] / n / alpha).round().clamp(0.0, 255.0) as u8,
                    b: (sum[2] / n / alpha).round().clamp(0.0, 255.0) as u8,
                    a: alpha * opacity,
                };
                self.blend(x, y, color, amount);
            }
        }
    }

    /// A gradient over `area`, within `clip` and `shape`.
    fn gradient_in(&mut self, gradient: &Gradient, area: Area, opacity: f64, clip: Area, shape: Shape) {
        let visible = area.intersect(clip).intersect(shape.area);
        if visible.is_empty() {
            return;
        }
        let (w, h) = (area.width(), area.height());
        let (cx, cy) = (area.x0 + w / 2.0, area.y0 + h / 2.0);
        let radians = gradient.angle.to_radians();
        let (dx, dy) = (radians.sin(), -radians.cos());
        let length = (w * dx).abs() + (h * dy).abs();
        let (rx, ry) = (w / 2.0 * std::f64::consts::SQRT_2, h / 2.0 * std::f64::consts::SQRT_2);
        let (x0, y0, x1, y1) = visible.pixels(self);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let amount = clip_coverage(clip, px, py) * shape.coverage(px, py);
                if amount <= 0.0 {
                    continue;
                }
                let (ox, oy) = (px - cx, py - cy);
                let t = if gradient.radial {
                    ((ox / rx.max(1e-9)).powi(2) + (oy / ry.max(1e-9)).powi(2)).sqrt()
                } else {
                    (ox * dx + oy * dy) / length.max(1e-9) + 0.5
                };
                self.blend(x, y, gradient.color_at(t).with_alpha(opacity), amount);
            }
        }
    }
}

/// Premultiplied RGBA at a fractional source position, bilinearly, with
/// edges clamped.
fn sample(image: &Image, x: f64, y: f64) -> [f64; 4] {
    let (w, h) = (image.width as i64, image.height as i64);
    let (fx, fy) = (x.floor(), y.floor());
    let (tx, ty) = (x - fx, y - fy);
    let at = |ix: i64, iy: i64| {
        let p = image.pixel(ix.clamp(0, w - 1) as u32, iy.clamp(0, h - 1) as u32);
        let a = p[3] as f64 / 255.0;
        [p[0] as f64 * a, p[1] as f64 * a, p[2] as f64 * a, a]
    };
    let (x0, y0) = (fx as i64, fy as i64);
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    let mut out = [0.0; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bottom = c[k] + (d[k] - c[k]) * tx;
        out[k] = top + (bottom - top) * ty;
    }
    out
}

impl Painter<'_> {
    /// An image the page refers to, if it has arrived. Screenshots waited
    /// for the page's images before painting, so this does not wait.
    fn image_for(&self, url: &str) -> Option<Arc<Image>> {
        let absolute = match url::Url::parse(self.doc.url()) {
            Ok(base) => base.join(url).ok()?.to_string(),
            Err(_) => url.to_string(),
        };
        self.doc.images()?.pixels(&absolute, Instant::now())
    }

    /// The background's image and gradient layers, last listed painted first.
    fn background_layers(&mut self, style: &ComputedStyle, rect: &Rect, opacity: f64, clip: Area, shape: Shape) {
        let bg = background(style);
        if bg.layers.is_empty() {
            return;
        }
        let border = border_widths(style);
        // Positioned within the padding box; painted over the border box.
        let origin = Rect {
            x: rect.x + border[3],
            y: rect.y + border[0],
            width: (rect.width - border[1] - border[3]).max(0.0),
            height: (rect.height - border[0] - border[2]).max(0.0),
        };
        let origin_area = self.area(&origin);
        for layer in bg.layers.iter().rev() {
            match layer {
                Layer::Gradient(gradient) => self.canvas.gradient_in(gradient, origin_area, opacity, clip, shape),
                Layer::Url(url) => {
                    let Some(image) = self.image_for(url) else { continue };
                    let (iw, ih) = (image.width as f64, image.height as f64);
                    let (ow, oh) = (origin.width, origin.height);
                    let size_words: Vec<&str> = bg.size.split_whitespace().collect();
                    let (tw, th) = match size_words.as_slice() {
                        ["cover"] => {
                            let s = (ow / iw).max(oh / ih);
                            (iw * s, ih * s)
                        }
                        ["contain"] => {
                            let s = (ow / iw).min(oh / ih);
                            (iw * s, ih * s)
                        }
                        words => {
                            let w = words.first().and_then(|w| bg_length(w, ow, style));
                            let h = words.get(1).and_then(|h| bg_length(h, oh, style));
                            match (w, h) {
                                (Some(w), Some(h)) => (w, h),
                                (Some(w), None) => (w, w * ih / iw),
                                (None, Some(h)) => (h * iw / ih, h),
                                (None, None) => (iw, ih),
                            }
                        }
                    };
                    if tw <= 0.0 || th <= 0.0 {
                        continue;
                    }
                    let (xs, ys) = position_axes(&bg.position);
                    let x = origin.x + place(&xs, ow, tw, style, "left", "right");
                    let y = origin.y + place(&ys, oh, th, style, "top", "bottom");
                    let repeat: Vec<&str> = bg.repeat.split_whitespace().collect();
                    let (repeat_x, repeat_y) = match repeat.as_slice() {
                        ["no-repeat"] => (false, false),
                        ["repeat-x"] => (true, false),
                        ["repeat-y"] => (false, true),
                        [x, y] => (*x != "no-repeat", *y != "no-repeat"),
                        _ => (true, true),
                    };
                    // Tiles start far enough back to cover the box.
                    let span = |start: f64, size: f64, from: f64, to: f64, repeat: bool| -> Vec<f64> {
                        if !repeat {
                            return vec![start];
                        }
                        let mut first = start - ((start - from) / size).ceil() * size;
                        let mut out = Vec::new();
                        while first < to && out.len() < 10_000 {
                            out.push(first);
                            first += size;
                        }
                        out
                    };
                    let columns = span(x, tw, rect.x, rect.x + rect.width, repeat_x);
                    let rows = span(y, th, rect.y, rect.y + rect.height, repeat_y);
                    for ty in &rows {
                        for tx in &columns {
                            let dest = self.area(&Rect { x: *tx, y: *ty, width: tw, height: th });
                            self.canvas.image(&image, dest, opacity, clip, shape);
                        }
                    }
                }
            }
        }
    }

    /// An `<img>`'s picture in its content box, as `object-fit` says.
    fn replaced_image(&mut self, id: NodeId, style: &ComputedStyle, rect: &Rect, opacity: f64, clip: Area, shape: Shape) {
        let Some(url) = self.doc.image_url(id) else { return };
        let Some(image) = self.doc.images().and_then(|store| store.pixels(&url, Instant::now())) else { return };
        let border = border_widths(style);
        let pad = padding(style, rect.width);
        let content = Rect {
            x: rect.x + border[3] + pad[3],
            y: rect.y + border[0] + pad[0],
            width: (rect.width - border[1] - border[3] - pad[1] - pad[3]).max(0.0),
            height: (rect.height - border[0] - border[2] - pad[0] - pad[2]).max(0.0),
        };
        let (iw, ih) = (image.width as f64, image.height as f64);
        let (w, h) = match style.get("object-fit") {
            "contain" => {
                let s = (content.width / iw).min(content.height / ih);
                (iw * s, ih * s)
            }
            "cover" => {
                let s = (content.width / iw).max(content.height / ih);
                (iw * s, ih * s)
            }
            "none" => (iw, ih),
            "scale-down" => {
                let s = (content.width / iw).min(content.height / ih).min(1.0);
                (iw * s, ih * s)
            }
            _ => (content.width, content.height),
        };
        let dest = Rect { x: content.x + (content.width - w) / 2.0, y: content.y + (content.height - h) / 2.0, width: w, height: h };
        let inside = clip.intersect(self.area(&content));
        let dest = self.area(&dest);
        self.canvas.image(&image, dest, opacity, inside, shape);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_html;

    fn render(html: &str) -> (Document, Canvas) {
        let mut doc = parse_html(html);
        doc.set_viewport((200.0, 100.0));
        let region = Region { x: 0.0, y: 0.0, width: 200.0, height: 100.0, scale: 1.0 };
        let canvas = paint(&doc, region, None, &|t| t.to_string());
        (doc, canvas)
    }

    #[test]
    fn boxes_are_filled_and_bordered_where_layout_put_them() {
        let (_, canvas) = render(
            "<body style='margin:0'>\
             <div style='position:absolute;left:10px;top:10px;width:40px;height:20px;background:#00aaff'></div>\
             <div style='position:absolute;left:60px;top:10px;width:40px;height:20px;background:rgba(255,0,0,.5)'></div>\
             <div style='position:absolute;left:110px;top:10px;width:40px;height:20px;border:3px solid green;border-left-color:blue'></div>\
             </body>",
        );
        assert_eq!(canvas.pixel(20, 20), [0x00, 0xaa, 0xff]);
        assert_eq!(canvas.pixel(9, 9), [255, 255, 255], "outside the box");
        assert_eq!(canvas.pixel(70, 20), [255, 128, 128], "half red over white");
        assert_eq!(canvas.pixel(130, 11), [0, 128, 0], "top border");
        assert_eq!(canvas.pixel(111, 20), [0, 0, 255], "left border, its own colour");
        assert_eq!(canvas.pixel(130, 20), [255, 255, 255], "inside the border");
    }

    #[test]
    fn overflow_hidden_clips_and_the_body_colours_the_canvas() {
        let (_, canvas) = render(
            "<body style='margin:0;background:#eee'>\
             <div style='width:50px;height:50px;overflow:hidden'>\
             <div style='width:100px;height:100px;background:black'></div></div></body>",
        );
        assert_eq!(canvas.pixel(25, 25), [0, 0, 0]);
        assert_eq!(canvas.pixel(75, 25), [0xee, 0xee, 0xee], "clipped, and the canvas is the body's colour");
        assert_eq!(canvas.pixel(190, 90), [0xee, 0xee, 0xee]);
    }

    #[test]
    fn text_is_inked_inside_its_box() {
        let (doc, canvas) = render("<body style='margin:0;font-size:20px'><span id=t>로그인 Login</span></body>");
        let span = doc.find_id(doc.document_id(), "t").unwrap();
        let text = doc.children(span)[0];
        let rect = layout::document_layout(&doc).rect(text).unwrap();
        let dark = |x0: u32, x1: u32, y0: u32, y1: u32| {
            (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|(x, y)| canvas.pixel(*x, *y)[0] < 128).count()
        };
        let inside = dark(0, rect.width.ceil() as u32, 0, rect.height.ceil() as u32);
        // Glyphs may reach a pixel or two past a line box shorter than the
        // font, as they do in Chrome.
        let outside = dark(rect.width.ceil() as u32 + 2, 200, 0, 100) + dark(0, 200, rect.height.ceil() as u32 + 2, 100);
        assert!(inside > 100, "{inside} dark pixels in the text box");
        assert_eq!(outside, 0);
    }

    /// A data URL of a 2x2 PNG: red, green / blue, white.
    fn quadrants() -> String {
        use base64::Engine as _;
        let pixels = image::RgbaImage::from_raw(2, 2, vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]).unwrap();
        let mut png = Vec::new();
        pixels.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))
    }

    /// [`render`], with an image store, after waiting for the page's images
    /// as a screenshot does.
    fn render_with_images(html: &str) -> (Document, Canvas) {
        let mut doc = parse_html(html);
        doc.set_viewport((200.0, 100.0));
        let client = crate::fetch::Client::new(std::sync::Arc::new(std::sync::Mutex::new(crate::cookies::CookieJar::new())));
        let store = Arc::new(crate::images::ImageStore::new(client));
        doc.set_images(Arc::clone(&store));
        let mut urls: Vec<String> = doc.descendants(doc.document_id()).filter(|id| doc.is_tag(*id, "img")).filter_map(|id| doc.image_url(id)).collect();
        urls.extend(background_urls(&doc));
        store.wait_all(&urls);
        let region = Region { x: 0.0, y: 0.0, width: 200.0, height: 100.0, scale: 1.0 };
        let canvas = paint(&doc, region, None, &|t| t.to_string());
        (doc, canvas)
    }

    #[test]
    fn gradients_run_from_stop_to_stop() {
        let (_, canvas) = render(
            "<body style='margin:0'><div style='width:100px;height:10px;background:linear-gradient(to right, #000, #fff)'></div>\
             <div style='width:100px;height:10px;background-image:linear-gradient(90deg, red 50%, blue 50%)'></div></body>",
        );
        assert!(canvas.pixel(2, 5)[0] < 10);
        assert!(canvas.pixel(97, 5)[0] > 245);
        assert!(canvas.pixel(50, 5)[0].abs_diff(128) < 4, "{:?}", canvas.pixel(50, 5));
        assert_eq!(canvas.pixel(25, 15), [255, 0, 0]);
        assert_eq!(canvas.pixel(75, 15), [0, 0, 255]);
    }

    #[test]
    fn background_images_are_sized_placed_and_not_repeated() {
        let html = format!(
            "<body style='margin:0;background:#888'>\
             <div style='width:60px;height:60px;background:url({}) no-repeat 10px 20px;background-size:20px 20px'></div></body>",
            quadrants()
        );
        let (_, canvas) = render_with_images(&html);
        // Corners of the tile: scaled up, the quadrants blend towards each
        // other in the middle, as Chrome's smoothing does too.
        assert_eq!(canvas.pixel(11, 21), [255, 0, 0], "top-left quadrant");
        assert_eq!(canvas.pixel(28, 21), [0, 255, 0]);
        assert_eq!(canvas.pixel(11, 38), [0, 0, 255]);
        assert_eq!(canvas.pixel(28, 38), [255, 255, 255]);
        assert_eq!(canvas.pixel(5, 5), [0x88, 0x88, 0x88], "outside the one tile");
        assert_eq!(canvas.pixel(45, 25), [0x88, 0x88, 0x88]);
    }

    #[test]
    fn images_have_their_own_size_and_keep_their_ratio() {
        let url = quadrants();
        let html = format!(
            "<body style='margin:0'><img id=a src='{url}' style='position:absolute;left:0;top:0'>\
             <img id=b src='{url}' width=40 style='position:absolute;left:50px;top:0'></body>"
        );
        let (doc, canvas) = render_with_images(&html);
        let layout = layout::document_layout(&doc);
        let rect = |id: &str| layout.rect(doc.find_id(doc.document_id(), id).unwrap()).unwrap();
        assert_eq!((rect("a").width, rect("a").height), (2.0, 2.0));
        assert_eq!((rect("b").width, rect("b").height), (40.0, 40.0));
        assert_eq!(canvas.pixel(55, 5), [255, 0, 0]);
        assert_eq!(canvas.pixel(85, 35), [255, 255, 255]);
    }

    #[test]
    fn the_focused_field_has_a_caret_after_its_text() {
        let mut doc = parse_html("<body style='margin:0'><input id=f value=ab style='position:absolute;left:0;top:0'></body>");
        doc.set_viewport((200.0, 100.0));
        let field = doc.find_id(doc.document_id(), "f").unwrap();
        let region = Region { x: 0.0, y: 0.0, width: 200.0, height: 100.0, scale: 1.0 };
        let plain = paint(&doc, region, None, &|t| t.to_string());
        let focused = paint(&doc, region, Some(field), &|t| t.to_string());
        let changed: Vec<u32> = (0..200u32).filter(|x| (0..100u32).any(|y| plain.pixel(*x, y) != focused.pixel(*x, y))).collect();
        // Chrome's inset (3px) plus "ab" at 13.33px is about 18px in.
        assert!(!changed.is_empty() && changed.len() <= 3, "{changed:?}");
        assert!((15..=22).contains(&changed[0]), "{changed:?}");
    }

    #[test]
    fn a_checked_box_is_drawn_checked() {
        let html = |checked: &str| {
            format!("<body style='margin:0'><input type=checkbox {checked} style='position:absolute;left:0;top:0;margin:0'></body>")
        };
        let (_, on) = render(&html("checked"));
        let (_, off) = render(&html(""));
        assert_eq!(on.pixel(2, 2), [0x00, 0x75, 0xff]);
        assert_eq!(off.pixel(3, 3), [255, 255, 255]);
        assert_eq!(off.pixel(0, 6), [0x76, 0x76, 0x76], "its border");
    }
}

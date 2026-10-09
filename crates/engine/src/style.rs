//! Styles: the cascade and computed values.
//!
//! The author's sheets and the inline `style` attribute cascade over a small
//! user-agent sheet, with origin, `!important`, specificity and order
//! deciding between declarations, as CSS Cascade specifies. Inherited
//! properties and custom properties flow down the tree; `var()` is
//! substituted, and `font-size` is resolved to pixels because everything
//! measured in `em` depends on it. Other values stay as written: layout
//! interprets the ones it uses.
//!
//! The result is computed for the whole document at once and is cheap to
//! keep: a map from node to properties that differ from the initial value.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use crate::css::{self, Declaration, StyleRule};
use crate::dom::{Document, NodeData, NodeId};
use crate::select::{self, Context, Selector, Simple};

/// One element's computed style.
#[derive(Debug, Clone, Default)]
pub struct ComputedStyle {
    props: HashMap<String, String>,
}

impl ComputedStyle {
    /// The computed value, or the property's initial value.
    pub fn get(&self, name: &str) -> &str {
        self.props.get(name).map(String::as_str).unwrap_or_else(|| initial_value(name))
    }

    /// Every property set on or inherited by this element.
    pub fn properties(&self) -> impl Iterator<Item = (&str, &str)> {
        self.props.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// `font-size` in pixels.
    pub fn font_size(&self) -> f64 {
        self.get("font-size").trim_end_matches("px").parse().unwrap_or(16.0)
    }
}

/// Computed styles for every element of a document.
#[derive(Debug, Default)]
pub struct Styles {
    styles: HashMap<NodeId, ComputedStyle>,
}

impl Styles {
    pub fn get(&self, id: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(&id)
    }

    /// Whether the element is not rendered: it or an ancestor has
    /// `display: none`. Elements not in the document count as not rendered.
    pub fn display_none(&self, doc: &Document, id: NodeId) -> bool {
        std::iter::successors(Some(id), |n| doc.parent(*n))
            .filter(|n| doc.is_element(*n))
            .any(|n| self.styles.get(&n).is_none_or(|s| s.get("display") == "none"))
    }

    /// Whether the element itself is invisible: not rendered, or
    /// `visibility: hidden`/`collapse` (which its children may override).
    pub fn invisible(&self, doc: &Document, id: NodeId) -> bool {
        self.display_none(doc, id) || self.styles.get(&id).is_some_and(|s| matches!(s.get("visibility"), "hidden" | "collapse"))
    }
}

/// The parsed sheets of a document, user-agent sheet first, each with the
/// rules it contributes.
pub type Sheets = Vec<Arc<Vec<StyleRule>>>;

/// Computes every element's style from explicit sheets. The engine uses
/// [`document_styles`], which finds the sheets and caches; this is for tests.
pub fn compute(doc: &Document, author: &Sheets, viewport: (f64, f64), focus: Option<NodeId>) -> Styles {
    compute_with(doc, &RuleIndex::build(author, viewport), focus)
}

/// What a document keeps between style computations: each sheet parsed once
/// per text, the rule index for the current sheets and viewport, and the
/// computed styles for the current version of the tree.
#[derive(Default)]
pub struct StyleCache(Mutex<CacheState>);

#[derive(Default)]
struct CacheState {
    parsed: HashMap<NodeId, (u64, Arc<Vec<StyleRule>>)>,
    index: Option<(u64, Arc<RuleIndex>)>,
    styles: Option<(u64, Arc<Styles>)>,
}

impl Clone for StyleCache {
    /// A copy of a document starts with nothing cached.
    fn clone(&self) -> Self {
        StyleCache::default()
    }
}

impl std::fmt::Debug for StyleCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StyleCache")
    }
}

/// Every element's computed style for the document as it is now. Cheap when
/// nothing changed since the last call; when the tree changed, styles are
/// recomputed, and sheets are parsed again only if their text changed.
///
/// `:focus` does not take part: the focused element lives with the script
/// state, not the document.
pub fn document_styles(doc: &Document) -> Arc<Styles> {
    let mut cache = doc.style_cache.0.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((version, styles)) = &cache.styles {
        if *version == doc.version() {
            return Arc::clone(styles);
        }
    }

    let viewport = doc.viewport();
    let texts = author_sheet_texts(doc, viewport);
    let mut fingerprint = DefaultHasher::new();
    viewport.0.to_bits().hash(&mut fingerprint);
    viewport.1.to_bits().hash(&mut fingerprint);
    let mut sheets: Sheets = Vec::with_capacity(texts.len());
    let mut parsed = HashMap::new();
    for (owner, text) in &texts {
        let hash = hash_text(text);
        (owner, hash).hash(&mut fingerprint);
        let rules = match cache.parsed.get(owner) {
            Some((h, rules)) if *h == hash => Arc::clone(rules),
            _ => Arc::new(css::parse_stylesheet(text)),
        };
        parsed.insert(*owner, (hash, Arc::clone(&rules)));
        sheets.push(rules);
    }
    cache.parsed = parsed;
    let fingerprint = fingerprint.finish();
    let index = match &cache.index {
        Some((f, index)) if *f == fingerprint => Arc::clone(index),
        _ => {
            let index = Arc::new(RuleIndex::build(&sheets, viewport));
            cache.index = Some((fingerprint, Arc::clone(&index)));
            index
        }
    };
    let styles = Arc::new(compute_with(doc, &index, None));
    cache.styles = Some((doc.version(), Arc::clone(&styles)));
    styles
}

fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// The author sheets of a document, in document order, by owner: each
/// `<style>`'s text and each stylesheet `<link>`'s fetched text, followed by
/// whatever script inserted through the CSSOM. A sheet whose `media`
/// attribute does not match the viewport, or that is disabled, is left out.
fn author_sheet_texts(doc: &Document, viewport: (f64, f64)) -> Vec<(NodeId, String)> {
    let mut out = Vec::new();
    for id in doc.descendants(doc.document_id()) {
        let base = match doc.tag(id) {
            Some("style") => doc.text_content(id),
            Some("link") => {
                let rel = doc.attr(id, "rel").unwrap_or("").to_ascii_lowercase();
                if !rel.split_ascii_whitespace().any(|r| r == "stylesheet") || rel.contains("alternate") {
                    continue;
                }
                match doc.stylesheet(id) {
                    Some(text) => text.to_string(),
                    None => String::new(),
                }
            }
            _ => continue,
        };
        if doc.has_attr(id, "disabled") {
            continue;
        }
        if let Some(media) = doc.attr(id, "media") {
            if !css::media_matches(media, viewport.0, viewport.1) {
                continue;
            }
        }
        let text = match doc.cssom(id) {
            Some(extra) if !extra.is_empty() => format!("{base}\n{extra}"),
            _ => base,
        };
        if !text.trim().is_empty() {
            out.push((id, text));
        }
    }
    out
}

fn compute_with(doc: &Document, index: &RuleIndex, focus: Option<NodeId>) -> Styles {
    let mut styles = Styles::default();
    let root_font = std::cell::Cell::new(16.0);
    let ctx = Context { scope: None, focus };
    let mut stack = vec![(doc.document_id(), None::<NodeId>)];
    while let Some((id, parent)) = stack.pop() {
        if doc.is_element(id) {
            let parent_style = parent.and_then(|p| styles.styles.get(&p)).cloned();
            let style = cascade(doc, id, &index, ctx, parent_style.as_ref(), &root_font);
            if parent.is_none() {
                root_font.set(style.font_size());
            }
            styles.styles.insert(id, style);
            for child in doc.children(id).iter().rev() {
                stack.push((*child, Some(id)));
            }
        } else if matches!(doc.node(id).data, NodeData::Document | NodeData::Fragment) {
            for child in doc.children(id).iter().rev() {
                stack.push((*child, parent));
            }
        }
    }
    styles
}

/// A rule's selector, ready to be matched, with what orders it.
struct IndexedRule {
    selector: Selector,
    specificity: u32,
    order: u32,
    user_agent: bool,
    declarations: Arc<Vec<Declaration>>,
}

/// Rules bucketed by what their rightmost compound requires, so an element
/// is matched only against rules that could apply to it.
#[derive(Default)]
struct RuleIndex {
    rules: Vec<IndexedRule>,
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
}

impl RuleIndex {
    fn build(author: &Sheets, viewport: (f64, f64)) -> Self {
        let mut index = RuleIndex::default();
        let ua = user_agent_sheet();
        let sheets = std::iter::once((true, &ua)).chain(author.iter().map(|s| (false, s)));
        for (user_agent, sheet) in sheets {
            for rule in sheet.iter() {
                if !rule.media.iter().all(|m| css::media_matches(m, viewport.0, viewport.1)) {
                    continue;
                }
                let declarations = Arc::new(rule.declarations.clone());
                for selector in &rule.selectors {
                    if selector.pseudo_element {
                        continue;
                    }
                    let at = index.rules.len();
                    let order = at as u32;
                    index.bucket(selector, at);
                    index.rules.push(IndexedRule {
                        selector: selector.clone(),
                        specificity: selector.specificity(),
                        order,
                        user_agent,
                        declarations: Arc::clone(&declarations),
                    });
                }
            }
        }
        index
    }

    fn bucket(&mut self, selector: &Selector, at: usize) {
        let Some((_, last)) = selector.parts.last() else { return };
        for simple in &last.simple {
            if let Simple::Id(id) = simple {
                self.by_id.entry(id.clone()).or_default().push(at);
                return;
            }
        }
        for simple in &last.simple {
            if let Simple::Class(class) = simple {
                self.by_class.entry(class.clone()).or_default().push(at);
                return;
            }
        }
        match &last.tag {
            Some(tag) => self.by_tag.entry(tag.clone()).or_default().push(at),
            None => self.universal.push(at),
        }
    }

    /// Rules that might match `id`, in no particular order.
    fn candidates(&self, doc: &Document, id: NodeId) -> Vec<usize> {
        let mut out = self.universal.clone();
        if let Some(element_id) = doc.attr(id, "id") {
            if let Some(list) = self.by_id.get(element_id) {
                out.extend(list);
            }
        }
        if let Some(classes) = doc.attr(id, "class") {
            for class in classes.split_ascii_whitespace() {
                if let Some(list) = self.by_class.get(class) {
                    out.extend(list);
                }
            }
        }
        if let Some(tag) = doc.tag(id) {
            if let Some(list) = self.by_tag.get(&tag.to_ascii_lowercase()) {
                out.extend(list);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Where a declaration sits in the cascade: higher wins.
fn precedence(user_agent: bool, inline: bool, important: bool) -> u8 {
    match (important, inline, user_agent) {
        (false, _, true) => 0,
        (false, false, false) => 1,
        (false, true, _) => 2,
        (true, false, false) => 3,
        (true, true, _) => 4,
        (true, false, true) => 5,
    }
}

fn cascade(
    doc: &Document,
    id: NodeId,
    index: &RuleIndex,
    ctx: Context,
    parent: Option<&ComputedStyle>,
    root_font: &std::cell::Cell<f64>,
) -> ComputedStyle {
    // (precedence, specificity, order) for each winning candidate.
    let mut applied: Vec<((u8, u32, u32), &Declaration)> = Vec::new();
    for at in index.candidates(doc, id) {
        let rule = &index.rules[at];
        if !select::matches_selector(doc, id, &rule.selector, ctx) {
            continue;
        }
        for declaration in rule.declarations.iter() {
            let key = (precedence(rule.user_agent, false, declaration.important), rule.specificity, rule.order);
            applied.push((key, declaration));
        }
    }
    let inline = doc.attr(id, "style").map(css::parse_declarations).unwrap_or_default();
    for (order, declaration) in inline.iter().enumerate() {
        applied.push(((precedence(false, true, declaration.important), u32::MAX, order as u32), declaration));
    }
    applied.sort_by_key(|(key, _)| *key);

    // Specified values, longhands only, the last applied winning.
    let mut specified: HashMap<String, String> = HashMap::new();
    for (_, declaration) in &applied {
        for (name, value) in expand_shorthand(&declaration.name, &declaration.value) {
            specified.insert(name, value);
        }
    }

    // Inherited properties and custom properties start from the parent.
    let mut props: HashMap<String, String> = HashMap::new();
    if let Some(parent) = parent {
        for (name, value) in parent.properties() {
            if name.starts_with("--") || INHERITED.contains(&name) {
                props.insert(name.to_string(), value.to_string());
            }
        }
    }

    // Custom properties first, so other values can refer to them.
    let mut custom: Vec<(&String, &String)> = specified.iter().filter(|(n, _)| n.starts_with("--")).collect();
    custom.sort();
    for (name, value) in custom {
        let resolved = substitute_vars(value, &props, 0);
        props.insert(name.clone(), resolved);
    }

    let parent_font = parent.map(ComputedStyle::font_size).unwrap_or(16.0);
    for (name, value) in specified.iter().filter(|(n, _)| !n.starts_with("--")) {
        let value = substitute_vars(value, &props, 0);
        let keyword = value.trim().to_ascii_lowercase();
        let inherits = INHERITED.contains(&name.as_str());
        match keyword.as_str() {
            "inherit" => match parent.map(|p| p.get(name).to_string()) {
                Some(v) => {
                    props.insert(name.clone(), v);
                }
                None => {
                    props.remove(name);
                }
            },
            "initial" => {
                props.remove(name);
            }
            "unset" | "revert" | "revert-layer" => {
                if !inherits {
                    props.remove(name);
                }
            }
            _ if name == "font-size" => {
                if let Some(px) = font_size_px(&keyword, parent_font, root_font.get()) {
                    props.insert(name.clone(), format!("{px}px"));
                }
            }
            _ => {
                props.insert(name.clone(), value.trim().to_string());
            }
        }
    }
    ComputedStyle { props }
}

/// Replaces `var(--name, fallback)` with the custom property's value.
fn substitute_vars(value: &str, props: &HashMap<String, String>, depth: u8) -> String {
    if depth > 8 || !value.contains("var(") {
        return value.to_string();
    }
    let mut out = String::new();
    let mut rest = value;
    while let Some(at) = rest.find("var(") {
        out.push_str(&rest[..at]);
        let inner_start = at + 4;
        let mut depth_paren = 1;
        let mut end = inner_start;
        for (i, c) in rest[inner_start..].char_indices() {
            match c {
                '(' => depth_paren += 1,
                ')' => {
                    depth_paren -= 1;
                    if depth_paren == 0 {
                        end = inner_start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth_paren != 0 {
            out.push_str(&rest[at..]);
            return out;
        }
        let inner = &rest[inner_start..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let replacement = match props.get(name) {
            Some(v) if !v.trim().is_empty() => v.clone(),
            _ => fallback.map(|f| substitute_vars(f, props, depth + 1)).unwrap_or_default(),
        };
        out.push_str(&replacement);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    substitute_vars(&out, props, depth + 1)
}

/// `font-size` in px: lengths, percentages, `em`/`rem`, and the keywords.
fn font_size_px(value: &str, parent: f64, root: f64) -> Option<f64> {
    let keyword = match value {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "xxx-large" => Some(48.0),
        "smaller" => Some(parent / 1.2),
        "larger" => Some(parent * 1.2),
        _ => None,
    };
    if keyword.is_some() {
        return keyword;
    }
    length_px(value, parent, root, parent)
}

/// A length in px. `em` is relative to `em_base`, `%` to `percent_base`.
pub fn length_px(value: &str, em_base: f64, root: f64, percent_base: f64) -> Option<f64> {
    let value = value.trim();
    let split = value.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+')).unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let n: f64 = number.parse().ok()?;
    Some(match unit.trim().to_ascii_lowercase().as_str() {
        "" | "px" => n,
        "em" => n * em_base,
        "rem" => n * root,
        "%" => n / 100.0 * percent_base,
        "pt" => n * 4.0 / 3.0,
        "pc" => n * 16.0,
        "in" => n * 96.0,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "q" => n * 96.0 / 101.6,
        "ex" | "ch" => n * em_base / 2.0,
        _ => return None,
    })
}

/// Properties whose computed value children inherit.
const INHERITED: &[&str] = &[
    "color", "cursor", "direction", "font-family", "font-size", "font-style", "font-variant", "font-weight",
    "letter-spacing", "line-height", "list-style-type", "list-style-position", "pointer-events", "quotes",
    "text-align", "text-indent", "text-transform", "visibility", "white-space", "word-break", "word-spacing",
    "overflow-wrap", "word-wrap", "caret-color", "writing-mode", "user-select", "-webkit-user-select",
    "tab-size", "hyphens", "orphans", "widows", "border-collapse", "border-spacing", "caption-side",
    "empty-cells", "text-shadow", "font-stretch", "line-break", "-webkit-text-fill-color",
];

/// A property's initial value, for the ones layout and clients read.
pub fn initial_value(name: &str) -> &'static str {
    match name {
        "display" => "inline",
        "visibility" => "visible",
        "position" => "static",
        "opacity" => "1",
        "cursor" => "auto",
        "pointer-events" => "auto",
        "z-index" | "width" | "height" | "top" | "right" | "bottom" | "left" | "flex-basis" => "auto",
        "min-width" | "min-height" => "auto",
        "max-width" | "max-height" => "none",
        "overflow" | "overflow-x" | "overflow-y" => "visible",
        "box-sizing" => "content-box",
        "float" | "clear" | "transform" => "none",
        "flex-direction" => "row",
        "flex-wrap" => "nowrap",
        "flex-grow" | "order" => "0",
        "flex-shrink" => "1",
        "justify-content" | "align-content" => "normal",
        "align-items" | "align-self" | "justify-items" | "justify-self" => "normal",
        "font-size" => "16px",
        "font-weight" => "400",
        "font-style" => "normal",
        "font-family" => "sans-serif",
        "line-height" => "normal",
        "white-space" => "normal",
        "text-align" => "start",
        "color" => "rgb(0, 0, 0)",
        "background-color" => "rgba(0, 0, 0, 0)",
        "direction" => "ltr",
        "column-gap" | "row-gap" => "normal",
        n if n.starts_with("margin-") || n.starts_with("padding-") => "0px",
        n if n.starts_with("border-") && n.ends_with("-width") => "0px",
        n if n.starts_with("border-") && n.ends_with("-style") => "none",
        n if n.starts_with("border-") && n.ends_with("-color") => "currentcolor",
        _ => "",
    }
}

/// A declaration as the longhands it sets. Shorthands layout depends on are
/// expanded; anything else passes through under its own name.
fn expand_shorthand(name: &str, value: &str) -> Vec<(String, String)> {
    let parts: Vec<&str> = split_values(value);
    let four = |prefix: &str, suffix: &str| -> Vec<(String, String)> {
        let pick = |i: usize| -> String {
            match parts.len() {
                0 => String::new(),
                1 => parts[0].to_string(),
                2 => parts[[0, 1, 0, 1][i]].to_string(),
                3 => parts[[0, 1, 2, 1][i]].to_string(),
                _ => parts[i].to_string(),
            }
        };
        ["top", "right", "bottom", "left"]
            .iter()
            .enumerate()
            .map(|(i, side)| (format!("{prefix}{side}{suffix}"), pick(i)))
            .collect()
    };
    let is_keyword = |v: &str| matches!(v, "inherit" | "initial" | "unset" | "revert" | "revert-layer");
    match name {
        "margin" => four("margin-", ""),
        "padding" => four("padding-", ""),
        "inset" => four("", ""),
        "border-width" => four("border-", "-width"),
        "border-style" => four("border-", "-style"),
        "border-color" => four("border-", "-color"),
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let sides: Vec<&str> = match name {
                "border" => vec!["top", "right", "bottom", "left"],
                other => vec![other.trim_start_matches("border-")],
            };
            let (mut width, mut style, mut color) = ("medium".to_string(), "none".to_string(), "currentcolor".to_string());
            if is_keyword(value.trim()) {
                width = value.trim().to_string();
                style = value.trim().to_string();
                color = value.trim().to_string();
            } else {
                for part in &parts {
                    if is_border_style(part) {
                        style = part.to_string();
                    } else if part.starts_with(|c: char| c.is_ascii_digit() || c == '.') || matches!(*part, "thin" | "medium" | "thick") {
                        width = part.to_string();
                    } else {
                        color = part.to_string();
                    }
                }
            }
            sides
                .iter()
                .flat_map(|side| {
                    [
                        (format!("border-{side}-width"), width.clone()),
                        (format!("border-{side}-style"), style.clone()),
                        (format!("border-{side}-color"), color.clone()),
                    ]
                })
                .collect()
        }
        "flex" => {
            let (grow, shrink, basis) = match parts.as_slice() {
                [k] if is_keyword(k) => (k.to_string(), k.to_string(), k.to_string()),
                ["none"] => ("0".into(), "0".into(), "auto".into()),
                ["auto"] => ("1".into(), "1".into(), "auto".into()),
                [n] if n.parse::<f64>().is_ok() => (n.to_string(), "1".into(), "0%".into()),
                [b] => ("1".into(), "1".into(), b.to_string()),
                [g, s] if s.parse::<f64>().is_ok() => (g.to_string(), s.to_string(), "0%".into()),
                [g, b] => (g.to_string(), "1".into(), b.to_string()),
                [g, s, b, ..] => (g.to_string(), s.to_string(), b.to_string()),
                [] => return Vec::new(),
            };
            vec![("flex-grow".into(), grow), ("flex-shrink".into(), shrink), ("flex-basis".into(), basis)]
        }
        "flex-flow" => parts
            .iter()
            .map(|p| {
                let key = if matches!(*p, "wrap" | "nowrap" | "wrap-reverse") { "flex-wrap" } else { "flex-direction" };
                (key.to_string(), p.to_string())
            })
            .collect(),
        "gap" | "grid-gap" => {
            let row = parts.first().map(|s| s.to_string()).unwrap_or_default();
            let column = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| row.clone());
            vec![("row-gap".into(), row), ("column-gap".into(), column)]
        }
        "overflow" => {
            let x = parts.first().map(|s| s.to_string()).unwrap_or_default();
            let y = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| x.clone());
            vec![("overflow-x".into(), x), ("overflow-y".into(), y), ("overflow".into(), value.trim().to_string())]
        }
        "place-items" => {
            let a = parts.first().map(|s| s.to_string()).unwrap_or_default();
            let j = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| a.clone());
            vec![("align-items".into(), a), ("justify-items".into(), j)]
        }
        "place-content" => {
            let a = parts.first().map(|s| s.to_string()).unwrap_or_default();
            let j = parts.get(1).map(|s| s.to_string()).unwrap_or_else(|| a.clone());
            vec![("align-content".into(), a), ("justify-content".into(), j)]
        }
        "background" => {
            // Only the colour matters without a rasterizer; a colour is the
            // last layer's lone colour-looking part.
            let color = parts.iter().rev().find(|p| p.starts_with('#') || p.starts_with("rgb") || p.starts_with("hsl") || p.chars().all(|c| c.is_ascii_alphabetic()) && !matches!(**p, "none" | "repeat" | "no-repeat" | "center" | "top" | "left" | "right" | "bottom" | "fixed" | "scroll" | "cover" | "contain"));
            match color {
                Some(c) => vec![("background-color".into(), c.to_string()), ("background".into(), value.trim().to_string())],
                None => vec![("background".into(), value.trim().to_string())],
            }
        }
        _ => vec![(name.to_string(), value.to_string())],
    }
}

fn is_border_style(value: &str) -> bool {
    matches!(value, "none" | "hidden" | "dotted" | "dashed" | "solid" | "double" | "groove" | "ridge" | "inset" | "outset")
}

/// Splits a value on whitespace outside parentheses.
fn split_values(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = None;
    for (i, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if let Some(s) = start.take() {
                    parts.push(&value[s..i]);
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
        parts.push(&value[s..]);
    }
    parts
}

/// What a browser styles before any author sheet: which elements are
/// blocks, which are hidden, default margins and font sizes.
const USER_AGENT_CSS: &str = r#"
html, address, blockquote, body, center, dialog, div, figure, figcaption, footer, form, header, hr,
legend, listing, main, p, plaintext, pre, search, xmp, article, aside, h1, h2, h3, h4, h5, h6, hgroup,
nav, section, dir, dd, dl, dt, menu, ol, ul, fieldset, details, summary, optgroup { display: block }
head, script, style, link, meta, title, template, noscript, datalist, param, area, base, basefont,
rp, [hidden]:not([hidden=until-found i]), input[type=hidden i], dialog:not([open]) { display: none }
li { display: list-item }
table { display: table } caption { display: table-caption } colgroup { display: table-column-group }
col { display: table-column } thead { display: table-header-group } tbody { display: table-row-group }
tfoot { display: table-footer-group } tr { display: table-row } td, th { display: table-cell }
button, input, select, textarea, meter, progress, img, video, canvas, iframe, embed, object, svg { display: inline-block }
body { margin: 8px }
p, blockquote, figure, dl, ul, ol, menu, pre { margin-top: 1em; margin-bottom: 1em }
h1 { font-size: 2em; margin-top: 0.67em; margin-bottom: 0.67em; font-weight: bold }
h2 { font-size: 1.5em; margin-top: 0.83em; margin-bottom: 0.83em; font-weight: bold }
h3 { font-size: 1.17em; margin-top: 1em; margin-bottom: 1em; font-weight: bold }
h4 { margin-top: 1.33em; margin-bottom: 1.33em; font-weight: bold }
h5 { font-size: 0.83em; margin-top: 1.67em; margin-bottom: 1.67em; font-weight: bold }
h6 { font-size: 0.67em; margin-top: 2.33em; margin-bottom: 2.33em; font-weight: bold }
ul, ol, menu, dir { padding-left: 40px }
b, strong, th { font-weight: bold }
a:any-link { cursor: pointer; color: #0000ee; text-decoration: underline }
input, select, textarea, button { font-size: 13.3333px }
pre, code, kbd, samp, tt { font-family: monospace }
dialog[open] { position: absolute; margin: auto }
fieldset { margin-left: 2px; margin-right: 2px; padding: 0.35em 0.75em 0.625em; border: 2px groove }
"#;

fn user_agent_sheet() -> Arc<Vec<StyleRule>> {
    static SHEET: std::sync::OnceLock<Arc<Vec<StyleRule>>> = std::sync::OnceLock::new();
    Arc::clone(SHEET.get_or_init(|| Arc::new(css::parse_stylesheet(USER_AGENT_CSS))))
}

// ---- the inline attribute alone ---------------------------------------------
//
// For places that have no document-wide styles to hand.

/// The declarations of the element's `style` attribute, in order, with names
/// lowercased and `!important` dropped. Later declarations win.
pub fn inline_declarations(doc: &Document, id: NodeId) -> Vec<(String, String)> {
    doc.attr(id, "style")
        .map(css::parse_declarations)
        .unwrap_or_default()
        .into_iter()
        .map(|d| (d.name, d.value))
        .collect()
}

/// The last inline value given for `name`.
pub fn inline_value(doc: &Document, id: NodeId, name: &str) -> Option<String> {
    inline_declarations(doc, id).into_iter().rev().find(|(n, _)| n == name).map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_html;

    fn styles(html: &str, css: &str) -> (Document, Styles) {
        let doc = parse_html(html);
        let sheets = vec![Arc::new(css::parse_stylesheet(css))];
        let styles = compute(&doc, &sheets, (1024.0, 768.0), None);
        (doc, styles)
    }

    fn by_id(doc: &Document, id: &str) -> NodeId {
        doc.find_id(doc.document_id(), id).unwrap()
    }

    #[test]
    fn the_cascade_orders_by_importance_specificity_and_order() {
        let (doc, s) = styles(
            "<div id='d' class='a b' style='color: green'><p id='p' class='x'>t</p></div>",
            "#d { color: red } .a.b { color: blue } div { color: black !important }\
             p.x { margin: 1px 2px } .x { margin-left: 9px } p { margin-left: 5px }",
        );
        assert_eq!(s.get(by_id(&doc, "d")).unwrap().get("color"), "black");
        let p = s.get(by_id(&doc, "p")).unwrap();
        assert_eq!(p.get("margin-top"), "1px");
        assert_eq!(p.get("margin-left"), "2px");
        assert_eq!(p.get("color"), "black", "colour is inherited");
    }

    #[test]
    fn user_agent_defaults_and_hidden() {
        let (doc, s) = styles(
            "<div id='d'>x</div><span id='s'>y</span><p id='h' hidden>z</p><ul><li id='l'>i</li></ul>",
            "",
        );
        assert_eq!(s.get(by_id(&doc, "d")).unwrap().get("display"), "block");
        assert_eq!(s.get(by_id(&doc, "s")).unwrap().get("display"), "inline");
        assert_eq!(s.get(by_id(&doc, "l")).unwrap().get("display"), "list-item");
        assert!(s.display_none(&doc, by_id(&doc, "h")));
    }

    #[test]
    fn display_and_visibility_reach_descendants() {
        let (doc, s) = styles(
            "<div class='off'><button id='b'>x</button></div>\
             <div class='ghost'><p id='g'>g</p><p id='v' class='back'>v</p></div>",
            ".off { display: none } .ghost { visibility: hidden } .back { visibility: visible }",
        );
        assert!(s.display_none(&doc, by_id(&doc, "b")));
        assert!(s.invisible(&doc, by_id(&doc, "g")));
        assert!(!s.invisible(&doc, by_id(&doc, "v")));
    }

    #[test]
    fn media_queries_custom_properties_and_font_sizes() {
        let (doc, s) = styles(
            "<html style='font-size: 10px'><body><div id='d'><p id='p'>t</p></div></body></html>",
            ":root { --gap: 4px } #d { font-size: 2em; --c: var(--missing, teal) }\
             #p { margin-top: var(--gap); color: var(--c); font-size: 1.5rem }\
             @media (max-width: 600px) { #p { display: none } }\
             @media (min-width: 1000px) { #p { cursor: pointer } }",
        );
        let d = s.get(by_id(&doc, "d")).unwrap();
        assert_eq!(d.get("font-size"), "20px");
        let p = s.get(by_id(&doc, "p")).unwrap();
        assert_eq!(p.get("margin-top"), "4px");
        assert_eq!(p.get("color"), "teal");
        assert_eq!(p.get("font-size"), "15px");
        assert_eq!(p.get("display"), "block");
        assert_eq!(p.get("cursor"), "pointer");
    }

    #[test]
    fn shorthands_expand_and_keywords_apply() {
        let (doc, s) = styles(
            "<div id='d'><span id='s'>x</span></div>",
            "#d { flex: 1; border: 2px solid red; padding: 1px 2px 3px; color: red }\
             #s { color: initial; display: inherit }",
        );
        let d = s.get(by_id(&doc, "d")).unwrap();
        assert_eq!((d.get("flex-grow"), d.get("flex-shrink"), d.get("flex-basis")), ("1", "1", "0%"));
        assert_eq!((d.get("border-left-width"), d.get("border-left-style")), ("2px", "solid"));
        assert_eq!((d.get("padding-top"), d.get("padding-right"), d.get("padding-bottom"), d.get("padding-left")), ("1px", "2px", "3px", "2px"));
        let span = s.get(by_id(&doc, "s")).unwrap();
        assert_eq!(span.get("color"), "rgb(0, 0, 0)");
        assert_eq!(span.get("display"), "block");
    }
}

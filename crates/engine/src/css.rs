//! Stylesheet parsing.
//!
//! Enough of CSS Syntax to read real bundles: comments, strings, nested
//! blocks, `!important`, and the grouping at-rules. `@media` keeps its
//! condition for the cascade to evaluate against the viewport; `@supports`,
//! `@layer` and `@container` contribute their rules unconditionally, which
//! is right for the common case and wrong only for fallbacks. At-rules that
//! do not style elements (`@font-face`, `@keyframes`, `@page`) are skipped,
//! as is `@import`, whose sheet is not fetched yet.
//!
//! Errors are recovered from the way CSS specifies: a bad declaration is
//! dropped, a rule whose selector does not parse is dropped, and parsing
//! carries on after them.

use crate::select::{self, SelectorList};

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    /// Lowercase, except custom properties (`--x`), which keep their case.
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone)]
pub struct StyleRule {
    pub selectors: SelectorList,
    pub declarations: Vec<Declaration>,
    /// `@media` conditions this rule sits inside, outermost first.
    pub media: Vec<String>,
}

/// Every style rule in the sheet, flattened, in source order.
pub fn parse_stylesheet(text: &str) -> Vec<StyleRule> {
    let chars: Vec<char> = strip_comments(text).chars().collect();
    let mut out = Vec::new();
    parse_rules(&chars, &mut Vec::new(), &mut out);
    out
}

/// The declarations of a block or a `style` attribute.
pub fn parse_declarations(text: &str) -> Vec<Declaration> {
    let chars: Vec<char> = strip_comments(text).chars().collect();
    split_top_level(&chars, ';')
        .into_iter()
        .filter_map(|part| {
            let part: String = part.iter().collect();
            let (name, value) = part.split_once(':')?;
            let name = name.trim();
            if name.is_empty() || name.contains(char::is_whitespace) || name.contains('{') {
                return None;
            }
            let name = if name.starts_with("--") { name.to_string() } else { name.to_ascii_lowercase() };
            let mut value = value.trim().to_string();
            let mut important = false;
            if let Some(bang) = value.rfind('!') {
                if value[bang + 1..].trim().eq_ignore_ascii_case("important") {
                    important = true;
                    value = value[..bang].trim_end().to_string();
                }
            }
            if value.is_empty() && !name.starts_with("--") {
                return None;
            }
            Some(Declaration { name, value, important })
        })
        .collect()
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                out.push(c);
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '/' && chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = '\0';
                for d in chars.by_ref() {
                    if previous == '*' && d == '/' {
                        break;
                    }
                    previous = d;
                }
                out.push(' ');
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// Parses rules from `chars` until it runs out.
fn parse_rules(chars: &[char], media: &mut Vec<String>, out: &mut Vec<StyleRule>) {
    let mut pos = 0;
    while pos < chars.len() {
        // The prelude runs to the next top-level `{` or `;`.
        let start = pos;
        let mut depth = 0i32;
        let mut quote: Option<char> = None;
        while pos < chars.len() {
            let c = chars[pos];
            match quote {
                Some(q) => {
                    if c == '\\' {
                        pos += 1;
                    } else if c == q {
                        quote = None;
                    }
                }
                None => match c {
                    '"' | '\'' => quote = Some(c),
                    '(' | '[' => depth += 1,
                    ')' | ']' => depth -= 1,
                    '{' | ';' if depth <= 0 => break,
                    '}' if depth <= 0 => break,
                    _ => {}
                },
            }
            pos += 1;
        }
        let prelude: String = chars[start..pos.min(chars.len())].iter().collect();
        let prelude = prelude.trim().to_string();
        if pos >= chars.len() {
            break;
        }
        match chars[pos] {
            ';' | '}' => {
                // A statement at-rule (`@import`, `@charset`, `@layer a, b;`) or
                // a stray token. Either way, nothing to style with.
                pos += 1;
                continue;
            }
            _ => {}
        }
        // The block: from `{` to its matching `}`.
        let block_start = pos + 1;
        let block_end = matching_brace(chars, pos);
        let block = &chars[block_start..block_end.min(chars.len())];
        pos = block_end + 1;

        if let Some(at) = prelude.strip_prefix('@') {
            let name: String = at.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
            let condition = at[name.len()..].trim().to_string();
            match name.to_ascii_lowercase().as_str() {
                "media" => {
                    media.push(condition);
                    parse_rules(block, media, out);
                    media.pop();
                }
                "supports" | "layer" | "container" | "document" | "-moz-document" | "scope" | "starting-style" => {
                    parse_rules(block, media, out);
                }
                _ => {}
            }
            continue;
        }
        if prelude.is_empty() {
            continue;
        }
        let Some(selectors) = select::parse_list(&prelude) else { continue };
        let declarations = parse_declarations(&block.iter().collect::<String>());
        if declarations.is_empty() {
            continue;
        }
        out.push(StyleRule { selectors, declarations, media: media.clone() });
    }
}

/// The index of the `}` closing the block opened at `open`, or the end.
fn matching_brace(chars: &[char], open: usize) -> usize {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut pos = open;
    while pos < chars.len() {
        let c = chars[pos];
        match quote {
            Some(q) => {
                if c == '\\' {
                    pos += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return pos;
                    }
                }
                _ => {}
            },
        }
        pos += 1;
    }
    chars.len()
}

/// Splits on `separator` outside strings, parentheses and nested blocks.
/// A nested block (CSS nesting) is dropped along with its selector.
fn split_top_level(chars: &[char], separator: char) -> Vec<Vec<char>> {
    let mut parts = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0i32;
    let mut braces = 0i32;
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                current.push(c);
                if c == '\\' && i + 1 < chars.len() {
                    i += 1;
                    current.push(chars[i]);
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => {
                    quote = Some(c);
                    current.push(c);
                }
                '(' => {
                    depth += 1;
                    current.push(c);
                }
                ')' => {
                    depth -= 1;
                    current.push(c);
                }
                '{' => {
                    braces += 1;
                }
                '}' => {
                    braces -= 1;
                    if braces == 0 {
                        // The nested rule ends; drop what it was attached to.
                        current.clear();
                    }
                }
                c if c == separator && depth <= 0 && braces == 0 => {
                    parts.push(std::mem::take(&mut current));
                }
                c => {
                    if braces == 0 {
                        current.push(c);
                    }
                }
            },
        }
        i += 1;
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Whether a media query list matches a screen of this size. Media types,
/// width and height with min-/max- and range syntax, orientation, and the
/// common preferences are understood; anything else does not match, which
/// is what a browser does with a feature it lacks.
pub fn media_matches(query: &str, width: f64, height: f64) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    query.split(',').any(|part| media_part_matches(&part.trim().to_ascii_lowercase(), width, height))
}

fn media_part_matches(part: &str, width: f64, height: f64) -> bool {
    let mut part = part.trim();
    let negate = part.starts_with("not ");
    if negate {
        part = part[4..].trim();
    }
    if let Some(rest) = part.strip_prefix("only ") {
        part = rest.trim();
    }
    let mut ok = true;
    for term in part.split(" and ") {
        let term = term.trim();
        match term {
            "" | "all" | "screen" => continue,
            "print" | "speech" | "tty" | "tv" | "projection" | "handheld" | "braille" | "embossed" => {
                ok = false;
                continue;
            }
            _ => {}
        }
        let Some(inner) = term.strip_prefix('(').and_then(|t| t.strip_suffix(')')) else {
            ok = false;
            continue;
        };
        ok &= feature_matches(inner.trim(), width, height);
    }
    ok != negate
}

fn feature_matches(feature: &str, width: f64, height: f64) -> bool {
    // Range syntax: `width >= 600px`, `400px < width`.
    for op in [">=", "<=", ">", "<", "="] {
        if let Some((left, right)) = feature.split_once(op) {
            let (left, right) = (left.trim(), right.trim());
            let (name, value, op) = if left.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                (left, right, op.to_string())
            } else {
                let flipped = match op {
                    ">=" => "<=",
                    "<=" => ">=",
                    ">" => "<",
                    "<" => ">",
                    other => other,
                };
                (right, left, flipped.to_string())
            };
            let actual = match name {
                "width" => width,
                "height" => height,
                _ => return false,
            };
            let Some(value) = length_px(value) else { return false };
            return match op.as_str() {
                ">=" => actual >= value,
                "<=" => actual <= value,
                ">" => actual > value,
                "<" => actual < value,
                _ => actual == value,
            };
        }
    }
    let (name, value) = match feature.split_once(':') {
        Some((n, v)) => (n.trim(), Some(v.trim())),
        None => (feature, None),
    };
    let px = value.and_then(length_px);
    match (name, value) {
        ("min-width", _) => px.is_some_and(|v| width >= v),
        ("max-width", _) => px.is_some_and(|v| width <= v),
        ("width", _) => px.is_some_and(|v| width == v),
        ("min-height", _) => px.is_some_and(|v| height >= v),
        ("max-height", _) => px.is_some_and(|v| height <= v),
        ("height", _) => px.is_some_and(|v| height == v),
        ("orientation", Some(v)) => v == if width >= height { "landscape" } else { "portrait" },
        ("prefers-color-scheme", Some(v)) => v == "light",
        ("prefers-reduced-motion" | "prefers-contrast" | "prefers-reduced-transparency", Some(v)) => v == "no-preference",
        ("hover" | "any-hover", v) => v.is_none_or(|v| v == "hover"),
        ("pointer" | "any-pointer", v) => v.is_none_or(|v| v == "fine"),
        ("min-resolution", Some(v)) => v.trim_end_matches("dppx").trim_end_matches('x').parse::<f64>().is_ok_and(|r| r <= 1.0)
            || v.trim_end_matches("dpi").parse::<f64>().is_ok_and(|r| r <= 96.0),
        ("-webkit-min-device-pixel-ratio" | "min-device-pixel-ratio", Some(v)) => v.parse::<f64>().is_ok_and(|r| r <= 1.0),
        ("color", _) => true,
        _ => false,
    }
}

/// A length in px, from px, em or rem (16px).
fn length_px(value: &str) -> Option<f64> {
    let value = value.trim();
    if let Some(n) = value.strip_suffix("px") {
        return n.trim().parse().ok();
    }
    if let Some(n) = value.strip_suffix("rem").or_else(|| value.strip_suffix("em")) {
        return n.trim().parse::<f64>().ok().map(|n| n * 16.0);
    }
    value.parse().ok()
}


/// Rewrites a sheet's relative `url()`s against `base`, the sheet's own URL,
/// so they still mean the same once the sheet is applied to a document at
/// another URL. `data:` URLs, absolute ones and fragments are left alone.
pub fn absolutize_urls(text: &str, base: &str) -> String {
    let Ok(base) = url::Url::parse(base) else { return text.to_string() };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("url(") {
        out.push_str(&rest[..start + 4]);
        rest = &rest[start + 4..];
        let Some(end) = rest.find(')') else { break };
        let inner = rest[..end].trim();
        let quote = inner.chars().next().filter(|c| *c == '"' || *c == '\'');
        let target = match quote {
            Some(q) => inner.trim_matches(q),
            None => inner,
        };
        let keep = target.is_empty()
            || target.starts_with('#')
            || target.starts_with("data:")
            || url::Url::parse(target).is_ok();
        match base.join(target) {
            Ok(absolute) if !keep => {
                out.push('"');
                out.push_str(absolute.as_str());
                out.push('"');
            }
            _ => out.push_str(&rest[..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn relative_urls_are_made_absolute_against_the_sheet() {
        let sheet = r#"a { background: url(img/a.png) } b { background: url( "../b.jpg" ) } c { background: url('/c.gif') }
            d { background: url(data:image/png;base64,AAA=) } e { background: url(https://cdn.test/e.png) } f { mask: url(#m) }"#;
        let out = absolutize_urls(sheet, "https://site.test/static/css/main.css");
        assert!(out.contains(r#"url("https://site.test/static/css/img/a.png")"#), "{out}");
        assert!(out.contains(r#"url("https://site.test/static/b.jpg")"#), "{out}");
        assert!(out.contains(r#"url("https://site.test/c.gif")"#), "{out}");
        assert!(out.contains("url(data:image/png;base64,AAA=)"), "{out}");
        assert!(out.contains("url(https://cdn.test/e.png)"), "{out}");
        assert!(out.contains("url(#m)"), "{out}");
    }

    use super::*;

    #[test]
    fn reads_rules_declarations_and_importance() {
        let rules = parse_stylesheet(
            "/* c */ a, .b > p { color: red; display:NONE !important; --Brand: #fff }\
             div{} .x{width:calc(1px + 2px);background:url('a;b.png')}",
        );
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].selectors.len(), 2);
        assert_eq!(
            rules[0].declarations,
            vec![
                Declaration { name: "color".into(), value: "red".into(), important: false },
                Declaration { name: "display".into(), value: "NONE".into(), important: true },
                Declaration { name: "--Brand".into(), value: "#fff".into(), important: false },
            ]
        );
        assert_eq!(rules[1].declarations[1].value, "url('a;b.png')");
    }

    #[test]
    fn groups_flatten_and_media_is_kept() {
        let rules = parse_stylesheet(
            "@charset 'utf-8'; @import url(x.css);\
             @media (max-width: 600px) { @supports (display:grid) { .m { display: grid } } }\
             @font-face { font-family: X; src: url(x.woff) }\
             @keyframes spin { from { opacity: 0 } to { opacity: 1 } }\
             @layer base { .l { color: blue } } .after { color: green }",
        );
        let selectors: Vec<usize> = rules.iter().map(|r| r.declarations.len()).collect();
        assert_eq!(selectors, [1, 1, 1]);
        assert_eq!(rules[0].media, ["(max-width: 600px)"]);
        assert!(rules[1].media.is_empty());
    }

    #[test]
    fn bad_parts_are_dropped_and_parsing_recovers() {
        let rules = parse_stylesheet("a:::bad { color: red } p { color } q { color: blue; ; nested { x: y } }");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].declarations, vec![Declaration { name: "color".into(), value: "blue".into(), important: false }]);
    }

    #[test]
    fn media_queries_evaluate_against_the_viewport() {
        assert!(media_matches("screen and (min-width: 768px)", 800.0, 600.0));
        assert!(!media_matches("(max-width: 767.98px)", 800.0, 600.0));
        assert!(media_matches("print, (orientation: landscape)", 800.0, 600.0));
        assert!(!media_matches("print", 800.0, 600.0));
        assert!(media_matches("not print", 800.0, 600.0));
        assert!(media_matches("(width >= 40em)", 800.0, 600.0));
        assert!(!media_matches("(prefers-color-scheme: dark)", 800.0, 600.0));
    }
}

//! SVG, for screenshots: an `<img>` or background whose image is SVG, and
//! inline `<svg>` elements, parsed by usvg and rasterised by resvg.
//!
//! An inline `<svg>` is painted from its markup, so what the SVG says for
//! itself (attributes, `style` attributes, its own `<style>`) is honoured.
//! The page's stylesheets reach only the root: its computed `color`, which
//! `currentColor` icons depend on, and `fill` and `stroke` when set. Text in
//! SVG is not drawn (it needs font loading and shaping), nor are `<use>`
//! references to elements outside the `<svg>`, nor raster images inside it.

use crate::color::Rgba;
use crate::images::Image;

/// Whether `bytes` are SVG markup rather than a raster format.
pub fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(2048)];
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('<') && text.contains("<svg")
}

fn parse(bytes: &[u8]) -> Option<resvg::usvg::Tree> {
    resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()
}

/// The size an SVG gives itself: its `width` and `height`, else its
/// `viewBox`, else 100 by 100, as usvg reads it.
pub fn size(bytes: &[u8]) -> Option<(u32, u32)> {
    let size = parse(bytes)?.size();
    Some((size.width().round().max(1.0) as u32, size.height().round().max(1.0) as u32))
}

/// The SVG drawn at `width` by `height` pixels, as straight-alpha RGBA.
pub fn rasterize(bytes: &[u8], width: u32, height: u32) -> Option<Image> {
    let tree = parse(bytes)?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width.max(1), height.max(1))?;
    let size = tree.size();
    let transform =
        resvg::tiny_skia::Transform::from_scale(width as f32 / size.width(), height as f32 / size.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // tiny-skia keeps premultiplied alpha; images here are straight.
    let mut rgba = pixmap.take();
    for pixel in rgba.chunks_exact_mut(4) {
        let a = pixel[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut pixel[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    let natural = (size.width() as f64, size.height() as f64);
    Some(Image { width: width.max(1), height: height.max(1), rgba, natural })
}

/// Inline `<svg>` markup made into a document usvg reads: the SVG
/// namespace declared, and the root given the page's `color`, `fill` and
/// `stroke` for it.
pub fn standalone(markup: &str, color: Rgba, fill: Option<&str>, stroke: Option<&str>) -> String {
    let Some(open_end) = markup.find('>') else { return markup.to_string() };
    let (open, rest) = markup.split_at(open_end);
    let mut open = open.to_string();
    let self_closing = open.ends_with('/');
    if self_closing {
        open.pop();
    }
    if !open.contains("xmlns=") {
        open.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
    }
    if open.contains("xlink:") || rest.contains("xlink:") {
        if !open.contains("xmlns:xlink") {
            open.push_str(" xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
        }
    }
    let rgb = format!("rgb({}, {}, {})", color.r, color.g, color.b);
    let mut computed = format!("color:{rgb} !important");
    for (name, value) in [("fill", fill), ("stroke", stroke)] {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            let value = if value.eq_ignore_ascii_case("currentcolor") { rgb.clone() } else { value.to_string() };
            computed.push_str(&format!(";{name}:{value} !important"));
        }
    }
    computed.push(';');
    // Computed CSS wins over presentation attributes, even `fill="none"`,
    // and over the original inline declaration it was computed from. usvg
    // keeps the first important declaration, so prepend the computed ones.
    // Keep unrelated inline properties rather than replacing the style.
    if let Some(at) = open.find(" style=") {
        let start = at + " style=".len();
        if let Some(quote @ ('\"' | '\'')) = open[start..].chars().next() {
            if open[start + 1..].contains(quote) {
                let escaped = escape_attribute(&computed, quote);
                open.insert_str(start + 1, &escaped);
            }
        }
    } else {
        open.push_str(&format!(" style=\"{}\"", escape_attribute(&computed, '"')));
    }
    if self_closing {
        open.push('/');
    }
    format!("{open}{rest}")
}

fn escape_attribute(value: &str, quote: char) -> String {
    value.replace('&', "&amp;").replace(quote, if quote == '"' { "&quot;" } else { "&apos;" })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED_SQUARE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20" viewBox="0 0 10 20"><rect width="10" height="10" fill="red"/></svg>"#;

    #[test]
    fn svg_is_recognised_measured_and_drawn() {
        assert!(is_svg(RED_SQUARE));
        assert!(!is_svg(b"\x89PNG\r\n"));
        assert_eq!(size(RED_SQUARE), Some((10, 20)));
        let image = rasterize(RED_SQUARE, 20, 40).unwrap();
        assert_eq!(image.natural, (10.0, 20.0));
        assert_eq!(image.pixel(5, 5), [255, 0, 0, 255], "scaled up");
        assert_eq!(image.pixel(5, 30)[3], 0, "transparent below the square");
    }

    #[test]
    fn computed_root_style_overrides_presentation_and_inline_values() {
        for attributes in [
            r#"fill="none""#,
            r#"fill="none" style="fill:red !important;opacity:0.5""#,
            "fill='none' style='fill:red !important;opacity:0.5'",
        ] {
            let markup = format!(r#"<svg viewBox="0 0 4 4" {attributes}><path d="M0 0h4v4H0z"/></svg>"#);
            let document = standalone(&markup, Rgba::rgb(0, 0, 0), Some("#03c75a"), None);
            let image = rasterize(document.as_bytes(), 4, 4).unwrap();
            for (actual, expected) in image.pixel(2, 2)[..3].iter().zip([3, 199, 90]) {
                assert!(actual.abs_diff(expected) <= 1, "{document}");
            }
            if attributes.contains("opacity") {
                assert!(image.pixel(2, 2)[3].abs_diff(128) <= 1, "keep the unrelated inline opacity");
            }
        }
    }

    #[test]
    fn inline_markup_gets_its_namespace_and_the_pages_colour() {
        let markup = r#"<svg viewBox="0 0 4 4"><path d="M0 0h4v4H0z" fill="currentColor"/></svg>"#;
        let document = standalone(markup, Rgba::rgb(0, 0, 255), None, None);
        assert!(document.contains(r#"xmlns="http://www.w3.org/2000/svg""#), "{document}");
        assert!(document.contains("color:rgb(0, 0, 255) !important"), "{document}");
        let image = rasterize(document.as_bytes(), 4, 4).unwrap();
        assert_eq!(image.pixel(2, 2), [0, 0, 255, 255]);
    }
}

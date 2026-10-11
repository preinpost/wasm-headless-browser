//! The one font the engine has: Noto Sans KR, cut down to Latin, symbols and
//! every Hangul syllable (`fonts/`, rebuilt by `scripts/make-font.sh`).
//!
//! A sandbox has no system fonts, so whatever a page asks for, text is
//! measured and painted with this. Layout and painting use the same advances,
//! so the boxes a client is told about hold exactly the glyphs a screenshot
//! shows. Noto Sans KR is also what Korean sites most often load as a web
//! font, which keeps measurements close to Chrome's there.
//!
//! What it does not do: other faces (bold is drawn by smearing the regular
//! one, italic is upright), monospace (glyphs are centred in 0.6em cells),
//! kerning, ligatures or any shaping, and characters outside the subset,
//! which are measured by an estimate and painted as an empty box.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, GlyphId, OutlinedGlyph, PxScale, ScaleFont};

static DATA: &[u8] = include_bytes!("../fonts/NotoSansKR-subset.otf");

fn face() -> &'static FontRef<'static> {
    static FACE: OnceLock<FontRef<'static>> = OnceLock::new();
    FACE.get_or_init(|| FontRef::try_from_slice(DATA).expect("the embedded font parses"))
}

/// The glyph for `c`, or `None` when the font lacks it.
fn glyph_id(c: char) -> Option<GlyphId> {
    let id = face().glyph_id(c);
    (id.0 != 0).then_some(id)
}

/// Whether `c` is set full-width when the font lacks it: CJK, Hangul and
/// emoji.
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x9FFF | 0xA960..=0xA97F | 0xAC00..=0xD7FF
        | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF)
}

/// How far the pen moves after `c` at `size` px.
pub fn advance(c: char, size: f64, monospace: bool) -> f64 {
    if monospace {
        return size * 0.6;
    }
    match glyph_id(c) {
        Some(id) => face().as_scaled(PxScale::from(1.0)).h_advance(id) as f64 * size / units_scale(),
        None if is_wide(c) => size,
        None => size * 0.5,
    }
}

/// `as_scaled(1.0)` scales to a height of one pixel, ascent to descent,
/// not to one em; this is the factor back to em units.
fn units_scale() -> f64 {
    let font = face();
    let height = font.height_unscaled() as f64;
    let em = font.units_per_em().unwrap_or(1000.0) as f64;
    // h_advance at scale 1.0 is advance_units / height.
    em / height
}

/// Ascent and descent below the baseline at `size` px, both positive.
pub fn ascent_descent(size: f64) -> (f64, f64) {
    let font = face();
    let em = font.units_per_em().unwrap_or(1000.0) as f64;
    (font.ascent_unscaled() as f64 * size / em, -font.descent_unscaled() as f64 * size / em)
}

/// One character drawn at a pen position.
pub enum Drawn {
    /// Coverage per pixel, from the glyph's outline.
    Outline(OutlinedGlyph),
    /// The font lacks it: an empty box of this size, from the pen position's
    /// top-left at the ascent.
    Missing { width: f64, height: f64 },
    /// Whitespace and the like: nothing to draw.
    Nothing,
}

/// `c` at `size` px with its pen at `(x, baseline)`.
pub fn draw(c: char, size: f64, x: f64, baseline: f64) -> Drawn {
    if c.is_whitespace() {
        return Drawn::Nothing;
    }
    let Some(id) = glyph_id(c) else {
        let width = advance(c, size, false);
        return Drawn::Missing { width: width * 0.8, height: size * 0.8 };
    };
    // ab_glyph's scale is the font's height, ascent to descent, in pixels.
    let scale = PxScale::from((size / units_scale()) as f32);
    let glyph = id.with_scale_and_position(scale, ab_glyph::point(x as f32, baseline as f32));
    match face().outline_glyph(glyph) {
        Some(outlined) => Drawn::Outline(outlined),
        None => Drawn::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_come_from_the_font() {
        // Noto Sans KR: Hangul 920 units, `A` 608, space 224, per 1000.
        assert!((advance('가', 10.0, false) - 9.2).abs() < 0.01);
        assert!((advance('A', 10.0, false) - 6.08).abs() < 0.01);
        assert!((advance(' ', 10.0, false) - 2.24).abs() < 0.01);
        assert_eq!(advance('A', 10.0, true), 6.0);
        // Hanja are not in the subset: a full-width estimate.
        assert_eq!(advance('漢', 10.0, false), 10.0);
        let (ascent, descent) = ascent_descent(10.0);
        assert!((ascent - 11.6).abs() < 0.01 && (descent - 2.88).abs() < 0.01, "{ascent} {descent}");
    }

    #[test]
    fn glyphs_have_outlines_and_missing_ones_say_so() {
        let Drawn::Outline(glyph) = draw('가', 20.0, 0.0, 20.0) else { panic!("no outline") };
        let bounds = glyph.px_bounds();
        assert!(bounds.width() > 10.0 && bounds.height() > 10.0, "{bounds:?}");
        assert!(matches!(draw('漢', 20.0, 0.0, 20.0), Drawn::Missing { .. }));
        assert!(matches!(draw(' ', 20.0, 0.0, 20.0), Drawn::Nothing));
    }
}

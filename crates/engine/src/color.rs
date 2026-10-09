//! CSS colours, for painting: hex, `rgb()`/`rgba()`, `hsl()`/`hsla()`, both
//! the comma and the space syntax, the named colours, `transparent`, and
//! `currentcolor` against the element's `color`. Anything else (`color-mix()`,
//! `lab()`, system colours) is `None`, which paints nothing.

/// Straight (not premultiplied) RGBA, alpha 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f64,
}

impl Rgba {
    pub const BLACK: Rgba = Rgba { r: 0, g: 0, b: 0, a: 1.0 };
    pub const WHITE: Rgba = Rgba { r: 255, g: 255, b: 255, a: 1.0 };
    pub const TRANSPARENT: Rgba = Rgba { r: 0, g: 0, b: 0, a: 0.0 };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba { r, g, b, a: 1.0 }
    }

    pub fn is_visible(&self) -> bool {
        self.a > 0.0
    }

    pub fn with_alpha(self, factor: f64) -> Rgba {
        Rgba { a: self.a * factor, ..self }
    }
}

/// Parses a computed colour. `current` is the element's `color`, for
/// `currentcolor`.
pub fn parse(value: &str, current: Rgba) -> Option<Rgba> {
    let value = value.trim().to_ascii_lowercase();
    let value = value.trim_end_matches("!important").trim();
    if value == "currentcolor" {
        return Some(current);
    }
    if value == "transparent" {
        return Some(Rgba::TRANSPARENT);
    }
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Some((name, args)) = value.split_once('(') {
        let args = args.strip_suffix(')')?;
        return match name.trim() {
            "rgb" | "rgba" => parse_rgb(args),
            "hsl" | "hsla" => parse_hsl(args),
            _ => None,
        };
    }
    named(value)
}

fn parse_hex(hex: &str) -> Option<Rgba> {
    let digit = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|d| d * 17);
    let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    if !hex.is_ascii() {
        return None;
    }
    match hex.len() {
        3 => Some(Rgba::rgb(digit(0)?, digit(1)?, digit(2)?)),
        4 => Some(Rgba { a: digit(3)? as f64 / 255.0, ..Rgba::rgb(digit(0)?, digit(1)?, digit(2)?) }),
        6 => Some(Rgba::rgb(pair(0)?, pair(2)?, pair(4)?)),
        8 => Some(Rgba { a: pair(6)? as f64 / 255.0, ..Rgba::rgb(pair(0)?, pair(2)?, pair(4)?) }),
        _ => None,
    }
}

/// `1, 2, 3, 0.5`, `1 2 3 / 50%`, and percentages for the channels.
fn arguments(args: &str) -> Vec<&str> {
    args.split(|c: char| c == ',' || c == '/' || c.is_whitespace()).filter(|s| !s.is_empty()).collect()
}

fn alpha(part: Option<&&str>) -> Option<f64> {
    match part {
        None => Some(1.0),
        Some(p) => Some(match p.strip_suffix('%') {
            Some(percent) => percent.parse::<f64>().ok()? / 100.0,
            None => p.parse::<f64>().ok()?,
        }
        .clamp(0.0, 1.0)),
    }
}

fn parse_rgb(args: &str) -> Option<Rgba> {
    let parts = arguments(args);
    if parts.len() < 3 {
        return None;
    }
    let channel = |p: &str| -> Option<u8> {
        let v = match p.strip_suffix('%') {
            Some(percent) => percent.parse::<f64>().ok()? / 100.0 * 255.0,
            None => p.parse::<f64>().ok()?,
        };
        Some(v.round().clamp(0.0, 255.0) as u8)
    };
    Some(Rgba { r: channel(parts[0])?, g: channel(parts[1])?, b: channel(parts[2])?, a: alpha(parts.get(3))? })
}

fn parse_hsl(args: &str) -> Option<Rgba> {
    let parts = arguments(args);
    if parts.len() < 3 {
        return None;
    }
    let hue = parts[0].trim_end_matches("deg").parse::<f64>().ok()?.rem_euclid(360.0) / 360.0;
    let percent = |p: &str| p.trim_end_matches('%').parse::<f64>().ok().map(|v| (v / 100.0).clamp(0.0, 1.0));
    let (s, l) = (percent(parts[1])?, percent(parts[2])?);
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |mut t: f64| {
        t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round() as u8
    };
    Some(Rgba { r: channel(hue + 1.0 / 3.0), g: channel(hue), b: channel(hue - 1.0 / 3.0), a: alpha(parts.get(3))? })
}

fn named(name: &str) -> Option<Rgba> {
    let hex = NAMED.iter().find(|(n, _)| *n == name)?.1;
    Some(Rgba::rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8))
}

/// CSS Color 4's named colours.
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff), ("antiquewhite", 0xfaebd7), ("aqua", 0x00ffff), ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff), ("beige", 0xf5f5dc), ("bisque", 0xffe4c4), ("black", 0x000000),
    ("blanchedalmond", 0xffebcd), ("blue", 0x0000ff), ("blueviolet", 0x8a2be2), ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887), ("cadetblue", 0x5f9ea0), ("chartreuse", 0x7fff00), ("chocolate", 0xd2691e),
    ("coral", 0xff7f50), ("cornflowerblue", 0x6495ed), ("cornsilk", 0xfff8dc), ("crimson", 0xdc143c),
    ("cyan", 0x00ffff), ("darkblue", 0x00008b), ("darkcyan", 0x008b8b), ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9), ("darkgreen", 0x006400), ("darkgrey", 0xa9a9a9), ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b), ("darkolivegreen", 0x556b2f), ("darkorange", 0xff8c00), ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000), ("darksalmon", 0xe9967a), ("darkseagreen", 0x8fbc8f), ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f), ("darkslategrey", 0x2f4f4f), ("darkturquoise", 0x00ced1), ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493), ("deepskyblue", 0x00bfff), ("dimgray", 0x696969), ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff), ("firebrick", 0xb22222), ("floralwhite", 0xfffaf0), ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff), ("gainsboro", 0xdcdcdc), ("ghostwhite", 0xf8f8ff), ("gold", 0xffd700),
    ("goldenrod", 0xdaa520), ("gray", 0x808080), ("green", 0x008000), ("greenyellow", 0xadff2f),
    ("grey", 0x808080), ("honeydew", 0xf0fff0), ("hotpink", 0xff69b4), ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082), ("ivory", 0xfffff0), ("khaki", 0xf0e68c), ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5), ("lawngreen", 0x7cfc00), ("lemonchiffon", 0xfffacd), ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080), ("lightcyan", 0xe0ffff), ("lightgoldenrodyellow", 0xfafad2), ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90), ("lightgrey", 0xd3d3d3), ("lightpink", 0xffb6c1), ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa), ("lightskyblue", 0x87cefa), ("lightslategray", 0x778899), ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de), ("lightyellow", 0xffffe0), ("lime", 0x00ff00), ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6), ("magenta", 0xff00ff), ("maroon", 0x800000), ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd), ("mediumorchid", 0xba55d3), ("mediumpurple", 0x9370db), ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee), ("mediumspringgreen", 0x00fa9a), ("mediumturquoise", 0x48d1cc), ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970), ("mintcream", 0xf5fffa), ("mistyrose", 0xffe4e1), ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead), ("navy", 0x000080), ("oldlace", 0xfdf5e6), ("olive", 0x808000),
    ("olivedrab", 0x6b8e23), ("orange", 0xffa500), ("orangered", 0xff4500), ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa), ("palegreen", 0x98fb98), ("paleturquoise", 0xafeeee), ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5), ("peachpuff", 0xffdab9), ("peru", 0xcd853f), ("pink", 0xffc0cb),
    ("plum", 0xdda0dd), ("powderblue", 0xb0e0e6), ("purple", 0x800080), ("rebeccapurple", 0x663399),
    ("red", 0xff0000), ("rosybrown", 0xbc8f8f), ("royalblue", 0x4169e1), ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072), ("sandybrown", 0xf4a460), ("seagreen", 0x2e8b57), ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d), ("silver", 0xc0c0c0), ("skyblue", 0x87ceeb), ("slateblue", 0x6a5acd),
    ("slategray", 0x708090), ("slategrey", 0x708090), ("snow", 0xfffafa), ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4), ("tan", 0xd2b48c), ("teal", 0x008080), ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347), ("turquoise", 0x40e0d0), ("violet", 0xee82ee), ("wheat", 0xf5deb3),
    ("white", 0xffffff), ("whitesmoke", 0xf5f5f5), ("yellow", 0xffff00), ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_stylesheets_write() {
        let black = Rgba::BLACK;
        assert_eq!(parse("#0af", black), Some(Rgba::rgb(0, 0xaa, 0xff)));
        assert_eq!(parse("#00AAFF", black), Some(Rgba::rgb(0, 0xaa, 0xff)));
        assert_eq!(parse("#00aaff80", black).unwrap().a, 128.0 / 255.0);
        assert_eq!(parse("rgb(1, 2, 3)", black), Some(Rgba::rgb(1, 2, 3)));
        assert_eq!(parse("rgba(1, 2, 3, 0.5)", black).unwrap().a, 0.5);
        assert_eq!(parse("rgb(1 2 3 / 25%)", black).unwrap().a, 0.25);
        assert_eq!(parse("rgb(100%, 0%, 50%)", black), Some(Rgba::rgb(255, 0, 128)));
        assert_eq!(parse("hsl(120, 100%, 25%)", black), Some(Rgba::rgb(0, 128, 0)));
        assert_eq!(parse("RebeccaPurple", black), Some(Rgba::rgb(0x66, 0x33, 0x99)));
        assert_eq!(parse("transparent", black).unwrap().a, 0.0);
        assert_eq!(parse("currentColor", Rgba::WHITE), Some(Rgba::WHITE));
        assert_eq!(parse("red !important", black), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse("color-mix(in srgb, red, blue)", black), None);
        assert_eq!(parse("#12", black), None);
    }
}

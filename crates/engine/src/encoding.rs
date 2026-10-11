//! Character encodings: which one a page or a response is in, and decoding
//! it, as the WHATWG Encoding standard and HTML define them, through
//! `encoding_rs`.
//!
//! A page's encoding is decided as a browser decides it: a byte order mark,
//! then the `charset` of its `Content-Type`, then a `<meta charset>` or
//! `<meta http-equiv=content-type>` in its first 1024 bytes. A page that
//! declares nothing is UTF-8 if it decodes as UTF-8, and EUC-KR (Windows
//! code page 949, which the standard's `euc-kr` is) otherwise, which is what
//! Chrome falls back to in a Korean locale. Chrome also guesses from the
//! bytes; this does not.

use encoding_rs::{Encoding, EUC_KR, REPLACEMENT, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252, X_USER_DEFINED};

/// The encoding a WHATWG label names, if any: `utf8`, `ks_c_5601-1987`,
/// `latin1` and the rest.
pub fn for_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.trim().as_bytes())
}

/// The `charset` parameter of a `Content-Type`.
pub fn charset_of(content_type: Option<&str>) -> Option<&'static Encoding> {
    let content_type = content_type?;
    content_type.split(';').skip(1).find_map(|param| {
        let (key, value) = param.split_once('=')?;
        key.trim().eq_ignore_ascii_case("charset").then(|| for_label(value.trim().trim_matches('"')))?
    })
}

/// An encoding as HTML lets a document declare it: UTF-16 in a `<meta>`
/// means UTF-8, since the bytes it was found in were ASCII, and
/// `x-user-defined` means windows-1252.
fn for_document(encoding: &'static Encoding) -> &'static Encoding {
    if encoding == UTF_16LE || encoding == UTF_16BE {
        UTF_8
    } else if encoding == X_USER_DEFINED {
        WINDOWS_1252
    } else {
        encoding
    }
}

/// A `<meta>` declaration in the first 1024 bytes: `charset=...` in a
/// `charset` attribute or in an `http-equiv` content. A simplification of
/// HTML's prescan: comments and attribute order are not considered.
fn prescan(bytes: &[u8]) -> Option<&'static Encoding> {
    let head = &bytes[..bytes.len().min(1024)];
    let text: String = head.iter().map(|b| (b.to_ascii_lowercase()) as char).collect();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("<meta") {
        let tag = &rest[start..];
        let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
        if let Some(at) = tag.find("charset") {
            let value = tag[at + "charset".len()..].trim_start();
            if let Some(value) = value.strip_prefix('=') {
                let value = value.trim_start().trim_start_matches(['"', '\'']);
                let end = value.find(|c: char| !(c.is_ascii_alphanumeric() || "-_.:".contains(c))).unwrap_or(value.len());
                if let Some(encoding) = for_label(&value[..end]) {
                    return Some(for_document(encoding));
                }
            }
        }
        rest = &rest[start + 5..];
    }
    None
}

/// The encoding a page's bytes are in, given its `Content-Type`.
pub fn sniff_document(bytes: &[u8], content_type: Option<&str>) -> &'static Encoding {
    if let Some((encoding, _)) = Encoding::for_bom(bytes) {
        return encoding;
    }
    if let Some(encoding) = charset_of(content_type).or_else(|| prescan(bytes)) {
        // The replacement encoding is for labels that are unsafe to honour.
        return if encoding == REPLACEMENT { REPLACEMENT } else { encoding };
    }
    if std::str::from_utf8(bytes).is_ok() {
        UTF_8
    } else {
        EUC_KR
    }
}

/// A subresource's text: a byte order mark, then its `Content-Type`
/// charset, then `fallback`, which is the document's encoding for scripts
/// and stylesheets and UTF-8 for everything else.
pub fn decode_body(bytes: &[u8], content_type: Option<&str>, fallback: &'static Encoding) -> String {
    let encoding = charset_of(content_type).unwrap_or(fallback);
    // `decode` honours a BOM over the encoding it is given.
    encoding.decode(bytes).0.into_owned()
}

/// `TextDecoder.decode`: `None` when `fatal` and the bytes are malformed.
pub fn decode_text(bytes: &[u8], encoding: &'static Encoding, fatal: bool, ignore_bom: bool) -> Option<String> {
    let bytes = if ignore_bom {
        bytes
    } else {
        // Only a BOM of the decoder's own encoding is removed.
        match Encoding::for_bom(bytes) {
            Some((found, length)) if found == encoding => &bytes[length..],
            _ => bytes,
        }
    };
    if fatal {
        encoding.decode_without_bom_handling_and_without_replacement(bytes).map(|text| text.into_owned())
    } else {
        Some(encoding.decode_without_bom_handling(bytes).0.into_owned())
    }
}

/// An encoding's name as `TextDecoder.encoding` gives it: lower case.
pub fn decoder_name(encoding: &'static Encoding) -> String {
    encoding.name().to_ascii_lowercase()
}

/// An encoding's name as `document.characterSet` gives it.
pub fn document_name(encoding: &'static Encoding) -> &'static str {
    encoding.name()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_says_what_it_is_in_or_is_guessed() {
        let euc_kr = EUC_KR.encode("한글").0.into_owned();
        assert_eq!(sniff_document(b"\xef\xbb\xbfhi", Some("text/html; charset=euc-kr")), UTF_8, "a BOM wins");
        assert_eq!(sniff_document(&euc_kr, Some("text/html; charset=\"EUC-KR\"")), EUC_KR);
        assert_eq!(sniff_document(b"<meta charset=ks_c_5601-1987><p>x", None), EUC_KR);
        assert_eq!(
            sniff_document(b"<META HTTP-EQUIV='Content-Type' CONTENT='text/html; charset=euc-kr'>", None),
            EUC_KR
        );
        assert_eq!(sniff_document(b"<meta charset='utf-16'>", None), UTF_8, "UTF-16 in a meta means UTF-8");
        assert_eq!(sniff_document("<p>한글".as_bytes(), Some("text/html")), UTF_8);
        assert_eq!(sniff_document(&euc_kr, None), EUC_KR, "undeclared and not UTF-8");
    }

    #[test]
    fn subresources_and_text_decoders_decode() {
        let euc_kr = EUC_KR.encode("한글").0.into_owned();
        assert_eq!(decode_body(&euc_kr, Some("text/javascript; charset=euc-kr"), UTF_8), "한글");
        assert_eq!(decode_body(&euc_kr, None, EUC_KR), "한글", "the document's encoding");
        assert_eq!(decode_body(b"\xef\xbb\xbfx", None, EUC_KR), "x", "a BOM wins");

        let utf16 = [0xff, 0xfe, b'h', 0, b'i', 0];
        assert_eq!(decode_text(&utf16, UTF_16LE, false, false).unwrap(), "hi");
        assert_eq!(decode_text(&utf16, UTF_16LE, false, true).unwrap(), "\u{feff}hi");
        assert_eq!(decode_text(&[0xff], UTF_8, true, false), None);
        assert_eq!(decoder_name(for_label("unicode-1-1-utf-8").unwrap()), "utf-8");
        assert_eq!(decoder_name(for_label("ks_c_5601-1987").unwrap()), "euc-kr");
        assert!(for_label("no-such-encoding").is_none());
    }
}

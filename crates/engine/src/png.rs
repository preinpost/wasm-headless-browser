//! PNG encoding for screenshots: 8-bit RGB, every row filtered with `Sub`,
//! which suits flat UI colours, and deflated with miniz_oxide.

/// Encodes `rgb`, three bytes a pixel, row by row.
pub fn encode(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let stride = width as usize * 3;
    let mut filtered = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgb.chunks(stride).take(height as usize) {
        filtered.push(1); // Sub: each byte less the one a pixel to its left.
        for (i, byte) in row.iter().enumerate() {
            let left = if i >= 3 { row[i - 3] } else { 0 };
            filtered.push(byte.wrapping_sub(left));
        }
    }
    let data = miniz_oxide::deflate::compress_to_vec_zlib(&filtered, 6);

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    // Bit depth 8, colour type 2 (RGB), deflate, adaptive filtering, no interlace.
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &header);
    chunk(&mut png, b"IDAT", &data);
    chunk(&mut png, b"IEND", &[]);
    png
}

fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc = Crc::new();
    crc.update(kind);
    crc.update(data);
    png.extend_from_slice(&crc.finish().to_be_bytes());
}

/// CRC-32 as PNG uses it (ISO 3309).
struct Crc(u32);

impl Crc {
    fn new() -> Self {
        Crc(0xffff_ffff)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            let mut c = (self.0 ^ *byte as u32) & 0xff;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            self.0 = c ^ (self.0 >> 8);
        }
    }

    fn finish(self) -> u32 {
        self.0 ^ 0xffff_ffff
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_png_that_decodes_back() {
        // A 2x2 image: red, green / blue, white.
        let rgb = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
        let png = encode(2, 2, &rgb);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        // IHDR's CRC, checked against zlib's crc32.
        assert_eq!(&png[29..33], &[0xfd, 0xd4, 0x9a, 0x73]);
        let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        let inflated = miniz_oxide::inflate::decompress_to_vec_zlib(&png[41..41 + idat_len]).unwrap();
        assert_eq!(inflated, [1, 255, 0, 0, 1, 255, 0, 1, 0, 0, 255, 255, 255, 0]);
    }
}

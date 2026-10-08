//! A PNG encoder small enough to keep in the repository.
//!
//! M0 renders the fake block layout so that `Page.captureScreenshot` returns a
//! real image. That also exercises the transport with a megabyte-scale frame,
//! which is worth knowing works before the real engine lands.
//!
//! Deflate is written with stored (uncompressed) blocks. The output is large
//! but valid, and it costs no dependencies.

use crate::dom::Document;

pub const WIDTH: u32 = 480;
pub const HEIGHT: u32 = 320;

pub fn render(doc: &Document) -> Vec<u8> {
    let mut canvas = Canvas::new(WIDTH, HEIGHT, [255, 255, 255]);

    let (vw, vh) = doc.viewport;
    let scale = (WIDTH as f64 / vw).min(HEIGHT as f64 / vh);

    for node in &doc.nodes {
        if !node.is_element() || node.tag == "html" || node.tag == "head" || node.tag == "title" {
            continue;
        }
        let r = node.rect;
        if r.w <= 0.0 || r.h <= 0.0 {
            continue;
        }
        let x = (r.x * scale) as i32;
        let y = (r.y * scale) as i32;
        let w = (r.w * scale) as i32;
        let h = (r.h * scale) as i32;
        canvas.fill_rect(x, y, w, h, [226, 232, 240]);
        canvas.stroke_rect(x, y, w, h, [148, 163, 184]);
    }

    canvas.encode()
}

struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32, fill: [u8; 3]) -> Self {
        let mut px = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..(w * h) {
            px.extend_from_slice(&fill);
        }
        Canvas { w, h, px }
    }

    fn put(&mut self, x: i32, y: i32, color: [u8; 3]) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let i = ((y as u32 * self.w + x as u32) * 3) as usize;
        self.px[i..i + 3].copy_from_slice(&color);
    }

    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 3]) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.put(xx, yy, color);
            }
        }
    }

    fn stroke_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 3]) {
        for xx in x..x + w {
            self.put(xx, y, color);
            self.put(xx, y + h - 1, color);
        }
        for yy in y..y + h {
            self.put(x, yy, color);
            self.put(x + w - 1, yy, color);
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity((self.h * (1 + self.w * 3)) as usize);
        for y in 0..self.h {
            raw.push(0); // filter: none
            let start = (y * self.w * 3) as usize;
            raw.extend_from_slice(&self.px[start..start + (self.w * 3) as usize]);
        }

        let mut png = Vec::new();
        png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&self.w.to_be_bytes());
        ihdr.extend_from_slice(&self.h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB, no interlace
        png.extend_from_slice(&chunk(b"IHDR", &ihdr));
        png.extend_from_slice(&chunk(b"IDAT", &zlib_stored(&raw)));
        png.extend_from_slice(&chunk(b"IEND", &[]));
        png
    }
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);

    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    if data.is_empty() {
        out.push(1);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    }
    let mut i = 0;
    while i < data.len() {
        let len = (data.len() - i).min(65535);
        let last = i + len >= data.len();
        out.push(if last { 1 } else { 0 });
        out.extend_from_slice(&(len as u16).to_le_bytes());
        out.extend_from_slice(&(!(len as u16)).to_le_bytes());
        out.extend_from_slice(&data[i..i + len]);
        i += len;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
        }
    }
    crc ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

pub fn to_base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for group in data.chunks(3) {
        let b0 = group[0] as u32;
        let b1 = *group.get(1).unwrap_or(&0) as u32;
        let b2 = *group.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if group.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if group.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

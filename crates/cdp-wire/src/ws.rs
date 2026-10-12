//! RFC 6455 handshake and frame framing.
//!
//! CDP clients always mask their frames, and CDP messages are small enough
//! that we never need to fragment our own output.

use std::io::{self, Read, Write};

use base64::Engine as _;

use crate::sha1::sha1;

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub mod opcode {
    pub const CONTINUATION: u8 = 0x0;
    pub const TEXT: u8 = 0x1;
    pub const BINARY: u8 = 0x2;
    pub const CLOSE: u8 = 0x8;
    pub const PING: u8 = 0x9;
    pub const PONG: u8 = 0xA;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opcode(pub u8);

#[derive(Debug, Clone)]
pub struct Frame {
    pub fin: bool,
    pub opcode: u8,
    pub payload: Vec<u8>,
}

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    let mut input = Vec::with_capacity(key.len() + WS_GUID.len());
    input.extend_from_slice(key.as_bytes());
    input.extend_from_slice(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(sha1(&input))
}

/// Writes the 101 response that completes the upgrade.
pub fn write_handshake(w: &mut impl Write, key: &str) -> io::Result<()> {
    write!(
        w,
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        accept_key(key)
    )?;
    w.flush()
}

/// Reads one frame. `Ok(None)` means the peer closed the connection.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 2];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }

    let fin = head[0] & 0x80 != 0;
    let opcode = head[0] & 0x0f;
    let masked = head[1] & 0x80 != 0;
    let len7 = (head[1] & 0x7f) as usize;

    let len = match len7 {
        126 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            u16::from_be_bytes(b) as usize
        }
        127 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            u64::from_be_bytes(b) as usize
        }
        n => n,
    };

    let mask = if masked {
        let mut m = [0u8; 4];
        r.read_exact(&mut m)?;
        Some(m)
    } else {
        None
    };

    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    if let Some(m) = mask {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= m[i % 4];
        }
    }

    Ok(Some(Frame { fin, opcode, payload }))
}

/// Writes one unmasked frame.
pub fn write_frame(w: &mut impl Write, opcode: u8, payload: &[u8]) -> io::Result<()> {
    write_frame_with(w, opcode, payload, None)
}

/// Writes one frame masked with `mask`, as RFC 6455 requires of a client.
pub fn write_masked_frame(w: &mut impl Write, opcode: u8, payload: &[u8], mask: [u8; 4]) -> io::Result<()> {
    write_frame_with(w, opcode, payload, Some(mask))
}

fn write_frame_with(w: &mut impl Write, opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> io::Result<()> {
    let mut head = Vec::with_capacity(14);
    head.push(0x80 | opcode);
    let mask_bit = if mask.is_some() { 0x80 } else { 0 };
    let n = payload.len();
    if n < 126 {
        head.push(mask_bit | n as u8);
    } else if n <= u16::MAX as usize {
        head.push(mask_bit | 126);
        head.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        head.push(mask_bit | 127);
        head.extend_from_slice(&(n as u64).to_be_bytes());
    }
    match mask {
        Some(m) => {
            head.extend_from_slice(&m);
            w.write_all(&head)?;
            let masked: Vec<u8> = payload.iter().enumerate().map(|(i, b)| b ^ m[i % 4]).collect();
            w.write_all(&masked)?;
        }
        None => {
            w.write_all(&head)?;
            w.write_all(payload)?;
        }
    }
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::{accept_key, opcode, read_frame, write_masked_frame};

    /// The worked example from RFC 6455 section 1.3.
    #[test]
    fn computes_the_rfc_example_accept_key() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn masked_frames_read_back_at_every_length_encoding() {
        for len in [0usize, 5, 125, 126, 65535, 65536] {
            let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let mut wire = Vec::new();
            write_masked_frame(&mut wire, opcode::TEXT, &payload, [0x37, 0xfa, 0x21, 0x3d]).unwrap();
            assert_eq!(wire[1] & 0x80, 0x80, "the mask bit is set");
            let frame = read_frame(&mut wire.as_slice()).unwrap().unwrap();
            assert!(frame.fin);
            assert_eq!(frame.opcode, opcode::TEXT);
            assert_eq!(frame.payload, payload, "length {len}");
        }
    }
}

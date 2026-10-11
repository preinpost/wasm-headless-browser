//! The accept loop and per-connection plumbing.
//!
//! One thread per connection. CDP clients open a single browser-level socket
//! and multiplex targets over it with `sessionId`, so this stays simple.
//! What is written goes through a [`Sender`], which other threads can hold
//! too, so a message can go out while the connection's own thread waits.

use std::io::{self, BufReader};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::http::{Request, Response};
use crate::ws::{self, opcode};

pub trait Handler: Send + Sync + 'static {
    /// Serves a plain HTTP request, such as `/json/version`.
    fn http(&self, req: &Request) -> Response;

    /// Serves an upgraded connection until the peer goes away.
    fn websocket(&self, conn: &mut Connection, path: &str) -> io::Result<()>;
}

/// A live WebSocket connection. Text messages in, text messages out.
pub struct Connection {
    reader: BufReader<TcpStream>,
    writer: Sender,
    assembler: Vec<u8>,
    fragmented_opcode: Option<u8>,
}

/// The writing half of a connection, for any thread. Each message is written
/// whole under a lock, so messages from several threads never interleave.
#[derive(Clone)]
pub struct Sender(Arc<Mutex<TcpStream>>);

impl Sender {
    fn frame(&self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut stream = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        ws::write_frame(&mut *stream, opcode, payload)
    }

    pub fn send_text(&self, s: &str) -> io::Result<()> {
        self.frame(opcode::TEXT, s.as_bytes())
    }

    pub fn send_json(&self, v: &Value) -> io::Result<()> {
        self.send_text(&serde_json::to_string(v).unwrap_or_else(|_| "{}".into()))
    }
}

impl Connection {
    fn new(stream: TcpStream) -> io::Result<Self> {
        let writer = stream.try_clone()?;
        writer.set_nodelay(true).ok();
        Ok(Connection {
            reader: BufReader::new(stream),
            writer: Sender(Arc::new(Mutex::new(writer))),
            assembler: Vec::new(),
            fragmented_opcode: None,
        })
    }

    /// A handle that writes to this connection from another thread.
    pub fn sender(&self) -> Sender {
        self.writer.clone()
    }

    fn raw_writer(&self) -> io::Result<TcpStream> {
        self.writer.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).try_clone()
    }

    /// Blocks until the next whole text message. `Ok(None)` on close.
    pub fn recv_text(&mut self) -> io::Result<Option<String>> {
        loop {
            let frame = match ws::read_frame(&mut self.reader)? {
                Some(f) => f,
                None => return Ok(None),
            };

            match frame.opcode {
                opcode::PING => {
                    self.writer.frame(opcode::PONG, &frame.payload)?;
                }
                opcode::PONG => {}
                opcode::CLOSE => {
                    let _ = self.writer.frame(opcode::CLOSE, &[]);
                    return Ok(None);
                }
                opcode::TEXT | opcode::BINARY => {
                    if frame.fin {
                        return Ok(Some(String::from_utf8_lossy(&frame.payload).into_owned()));
                    }
                    self.assembler = frame.payload;
                    self.fragmented_opcode = Some(frame.opcode);
                }
                opcode::CONTINUATION => {
                    self.assembler.extend_from_slice(&frame.payload);
                    if frame.fin {
                        let payload = std::mem::take(&mut self.assembler);
                        self.fragmented_opcode = None;
                        return Ok(Some(String::from_utf8_lossy(&payload).into_owned()));
                    }
                }
                _ => {}
            }
        }
    }

    pub fn send_text(&mut self, s: &str) -> io::Result<()> {
        self.writer.send_text(s)
    }

    pub fn send_json(&mut self, v: &Value) -> io::Result<()> {
        self.writer.send_json(v)
    }
}

/// Blocks forever, handling one connection per thread.
pub fn serve<H: Handler + Clone>(listener: TcpListener, handler: H) -> io::Result<()> {
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(s) => s,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };
        let handler = handler.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_connection(stream, handler) {
                eprintln!("connection ended: {e}");
            }
        });
    }
    Ok(())
}

fn handle_connection<H: Handler>(stream: TcpStream, handler: H) -> io::Result<()> {
    let mut conn = Connection::new(stream)?;

    let req = match Request::read(&mut conn.reader)? {
        Some(r) => r,
        None => return Ok(()),
    };

    let is_upgrade = req.header("upgrade").map(|v| v.eq_ignore_ascii_case("websocket")).unwrap_or(false);
    if !is_upgrade {
        let resp = handler.http(&req);
        let mut writer = conn.raw_writer()?;
        return resp.write(&mut writer);
    }

    let key = match req.header("sec-websocket-key") {
        Some(k) => k.to_string(),
        None => {
            let mut writer = conn.raw_writer()?;
            return Response::text(400, "missing Sec-WebSocket-Key").write(&mut writer);
        }
    };

    ws::write_handshake(&mut conn.raw_writer()?, &key)?;
    handler.websocket(&mut conn, &req.path)
}

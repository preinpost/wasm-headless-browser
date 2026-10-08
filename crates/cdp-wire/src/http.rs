//! Just enough HTTP/1.1 to serve the CDP discovery endpoints and to perform
//! the WebSocket upgrade.

use std::io::{self, BufRead};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Reads one request. Returns `Ok(None)` on a clean end of stream.
    pub fn read(r: &mut impl BufRead) -> io::Result<Option<Self>> {
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            // Chromium clients may emit stray CRLF before the request line.
            if line != "\r\n" && !line.is_empty() {
                break;
            }
        }

        let mut parts = line.trim_end().split(' ');
        let method = parts.next().unwrap_or_default().to_string();
        let target = parts.next().unwrap_or("/").to_string();
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target, String::new()),
        };

        let mut headers = Vec::new();
        loop {
            line.clear();
            if r.read_line(&mut line)? == 0 {
                break;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((k, v)) = trimmed.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }

        let mut body = Vec::new();
        if let Some(len) = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse::<usize>().ok())
        {
            body.resize(len, 0);
            r.read_exact(&mut body)?;
        }

        Ok(Some(Request { method, path, query, headers, body }))
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(body: impl Into<Vec<u8>>) -> Self {
        Response { status: 200, content_type: "application/json; charset=UTF-8", body: body.into() }
    }

    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Response { status, content_type: "text/plain; charset=UTF-8", body: body.into().into_bytes() }
    }

    pub fn reason(&self) -> &'static str {
        match self.status {
            101 => "Switching Protocols",
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            500 => "Internal Server Error",
            _ => "OK",
        }
    }

    pub fn write(&self, w: &mut impl io::Write) -> io::Result<()> {
        write!(
            w,
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.status,
            self.reason(),
            self.content_type,
            self.body.len()
        )?;
        w.write_all(&self.body)?;
        w.flush()
    }
}

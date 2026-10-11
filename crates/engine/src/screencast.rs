//! `Page.startScreencast`: the page as a stream of frames, which is how
//! agent-browser's dashboard and `stream` show a browser live and let a
//! person click and type into it.
//!
//! Chrome sends a frame when its compositor produces one. There is no
//! compositor here, so the engine asks after every command whether what a
//! frame would show has changed: the document's version, the images that
//! have arrived, the viewport, the URL and the focus, which moves the caret. Only then is the page painted,
//! so commands that only read cost nothing. As in Chrome, a frame is not
//! sent until the client has acknowledged the previous one, which keeps a
//! slow viewer from queueing stale frames.
//!
//! Frames are painted by `paint.rs`, so they show what screenshots show:
//! bullets for a password and placeholders for other secrets.

use serde_json::{json, Value};

use crate::dom::Document;
use crate::paint::{self, Region};

/// The image format a frame or screenshot is encoded in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Png,
    Jpeg { quality: u8 },
}

impl Format {
    /// From CDP's `format` and `quality`. WebP is not encoded here.
    pub fn parse(format: Option<&str>, quality: Option<u64>) -> Result<Format, String> {
        match format.unwrap_or("png") {
            "png" => Ok(Format::Png),
            "jpeg" => Ok(Format::Jpeg { quality: quality.unwrap_or(80).clamp(1, 100) as u8 }),
            other => Err(format!("{other} is not supported here, only png and jpeg")),
        }
    }

    pub fn encode(self, canvas: &paint::Canvas) -> Vec<u8> {
        match self {
            Format::Png => canvas.to_png(),
            Format::Jpeg { quality } => canvas.to_jpeg(quality),
        }
    }
}

/// What a frame showed, to tell whether the next one would differ.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    version: u64,
    generation: u64,
    viewport: (u64, u64),
    url: String,
    focus: Option<usize>,
}

pub struct Screencast {
    format: Format,
    max: (f64, f64),
    /// The id of the last frame sent, which its acknowledgement echoes.
    session: u64,
    awaiting_ack: bool,
    shown: Option<Shown>,
}

impl Screencast {
    pub fn start(params: &Value) -> Result<Screencast, String> {
        let format = Format::parse(params.get("format").and_then(Value::as_str), params.get("quality").and_then(Value::as_u64))?;
        let limit = |name: &str| params.get(name).and_then(Value::as_f64).filter(|v| *v > 0.0).unwrap_or(f64::INFINITY);
        Ok(Screencast { format, max: (limit("maxWidth"), limit("maxHeight")), session: 0, awaiting_ack: false, shown: None })
    }

    /// The client has the frame `session`: the next may be sent.
    pub fn ack(&mut self, session: u64) {
        if session == self.session {
            self.awaiting_ack = false;
        }
    }

    /// The `Page.screencastFrame` parameters for a new frame, if the page
    /// changed since the last one and the client is ready for it.
    pub fn frame(
        &mut self,
        doc: &Document,
        url: &str,
        viewport: (f64, f64),
        focus: Option<usize>,
        scrub: &dyn Fn(&str) -> String,
    ) -> Option<Value> {
        if self.awaiting_ack {
            return None;
        }
        let shown = Shown {
            version: doc.version(),
            generation: doc.images().map(|i| i.generation()).unwrap_or(0),
            viewport: (viewport.0.to_bits(), viewport.1.to_bits()),
            url: url.to_string(),
            focus,
        };
        if self.shown.as_ref() == Some(&shown) {
            return None;
        }
        // Scaled down to the client's limits, never up.
        let scale = (self.max.0 / viewport.0).min(self.max.1 / viewport.1).min(1.0);
        let region = Region { x: 0.0, y: 0.0, width: viewport.0, height: viewport.1, scale };
        let canvas = paint::paint(doc, region, focus, scrub);
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(self.format.encode(&canvas));
        self.session += 1;
        self.awaiting_ack = true;
        self.shown = Some(shown);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        Some(json!({
            "data": data,
            "metadata": {
                "offsetTop": 0,
                "pageScaleFactor": 1,
                "deviceWidth": viewport.0,
                "deviceHeight": viewport.1,
                "scrollOffsetX": 0,
                "scrollOffsetY": 0,
                "timestamp": timestamp,
            },
            "sessionId": self.session,
        }))
    }
}

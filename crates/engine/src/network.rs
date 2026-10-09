//! What `Network.*` tells a client: the requests a page makes, as events,
//! and their response bodies, for `Network.getResponseBody`.
//!
//! The client records here as it goes, from whichever thread sent the
//! request; the engine drains the events and sends them, through the same
//! exit as everything else it says, so secrets in a login `POST` come out as
//! placeholders. Nothing is recorded until `Network.enable`, as in Chrome.
//!
//! Chrome also sends `requestWillBeSentExtraInfo` and
//! `responseReceivedExtraInfo`, which carry the raw headers including
//! cookies. They are not sent, and every event says `hasExtraInfo: false`, so
//! a client does not wait for them.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::fetch::{FetchError, Response};

/// What kind of resource a request fetches, in CDP's `ResourceType` terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResourceType {
    Document,
    Stylesheet,
    Script,
    Fetch,
    Xhr,
    #[default]
    Other,
}

impl ResourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceType::Document => "Document",
            ResourceType::Stylesheet => "Stylesheet",
            ResourceType::Script => "Script",
            ResourceType::Fetch => "Fetch",
            ResourceType::Xhr => "XHR",
            ResourceType::Other => "Other",
        }
    }

    /// The name the prelude passes, which is the CDP name.
    pub fn parse(name: &str) -> Self {
        match name {
            "Document" => ResourceType::Document,
            "Stylesheet" => ResourceType::Stylesheet,
            "Script" => ResourceType::Script,
            "Fetch" => ResourceType::Fetch,
            "XHR" => ResourceType::Xhr,
            _ => ResourceType::Other,
        }
    }
}

/// Chrome's defaults when `Network.enable` names no limits.
const DEFAULT_TOTAL_BUFFER: usize = 100 * 1024 * 1024;
const DEFAULT_RESOURCE_BUFFER: usize = 10 * 1024 * 1024;

/// The network log of one target.
#[derive(Default)]
pub struct NetLog {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    enabled: bool,
    next_id: u64,
    events: Vec<(String, Value)>,
    /// Response bodies, oldest first, within `max_total` bytes. The oldest
    /// go first when it is exceeded, as Chrome evicts them.
    bodies: VecDeque<(String, Vec<u8>)>,
    body_bytes: usize,
    max_total: usize,
    max_resource: usize,
    frame_id: String,
    /// The current document's request id, which CDP calls its loader id,
    /// and its URL.
    loader_id: String,
    document_url: String,
}

/// A request being recorded. `None` from [`NetLog::begin`] when the log is
/// off.
pub struct Recording {
    id: String,
    kind: ResourceType,
}

impl NetLog {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn enable(&self, max_total: Option<usize>, max_resource: Option<usize>) {
        let mut log = self.lock();
        log.enabled = true;
        log.max_total = max_total.unwrap_or(DEFAULT_TOTAL_BUFFER);
        log.max_resource = max_resource.unwrap_or(DEFAULT_RESOURCE_BUFFER).min(log.max_total);
    }

    /// Stops recording and forgets what was kept.
    pub fn disable(&self) {
        let mut log = self.lock();
        log.enabled = false;
        log.events.clear();
        log.bodies.clear();
        log.body_bytes = 0;
    }

    pub fn set_frame(&self, frame_id: &str) {
        self.lock().frame_id = frame_id.to_string();
    }

    /// The events recorded since the last call, in the order they happened.
    pub fn take_events(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut self.lock().events)
    }

    /// A response body, if it was kept.
    pub fn body(&self, request_id: &str) -> Option<Vec<u8>> {
        self.lock().bodies.iter().find(|(id, _)| id == request_id).map(|(_, body)| body.clone())
    }

    /// A request about to go out. `headers` are the ones that will be sent,
    /// less those Chrome keeps for its extra-info events.
    pub fn begin(
        &self,
        kind: ResourceType,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Option<Recording> {
        let mut log = self.lock();
        if !log.enabled {
            return None;
        }
        log.next_id += 1;
        let id = format!("whb.{}", log.next_id);
        if kind == ResourceType::Document {
            log.loader_id = id.clone();
            log.document_url = url.to_string();
        }
        let mut params = log.request_params(&id, kind, method, url, headers, body);
        params["redirectHasExtraInfo"] = json!(false);
        log.events.push(("Network.requestWillBeSent".into(), params));
        Some(Recording { id, kind })
    }

    /// `response` redirected the request to `url`, which is sent next under
    /// the same id, as Chrome reports it.
    pub fn redirect(
        &self,
        recording: &Recording,
        response: &Response,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) {
        let mut log = self.lock();
        if recording.kind == ResourceType::Document {
            log.document_url = url.to_string();
        }
        let mut params = log.request_params(&recording.id, recording.kind, method, url, headers, body);
        params["redirectResponse"] = response_json(response);
        params["redirectHasExtraInfo"] = json!(false);
        log.events.push(("Network.requestWillBeSent".into(), params));
    }

    /// The final response arrived, whole.
    pub fn finish(&self, recording: Recording, response: &Response) {
        let mut log = self.lock();
        let now = timestamp();
        let length = response.body.len();
        let frame = log.frame_id.clone();
        let loader = log.loader_id.clone();
        log.events.push((
            "Network.responseReceived".into(),
            json!({
                "requestId": recording.id,
                "loaderId": loader,
                "timestamp": now,
                "type": recording.kind.as_str(),
                "response": response_json(response),
                "hasExtraInfo": false,
                "frameId": frame,
            }),
        ));
        log.events.push((
            "Network.dataReceived".into(),
            json!({ "requestId": recording.id, "timestamp": now, "dataLength": length, "encodedDataLength": length }),
        ));
        log.events.push((
            "Network.loadingFinished".into(),
            json!({ "requestId": recording.id, "timestamp": now, "encodedDataLength": length }),
        ));
        log.keep_body(recording.id, &response.body);
    }

    pub fn fail(&self, recording: Recording, error: &FetchError) {
        let mut log = self.lock();
        log.events.push((
            "Network.loadingFailed".into(),
            json!({
                "requestId": recording.id,
                "timestamp": timestamp(),
                "type": recording.kind.as_str(),
                "errorText": error.to_string(),
                "canceled": false,
            }),
        ));
    }
}

impl Inner {
    fn request_params(
        &self,
        id: &str,
        kind: ResourceType,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Value {
        let mut request = json!({
            "url": url,
            "method": method,
            "headers": header_object(headers.iter().map(|(k, v)| (k.as_str(), v.as_str()))),
            "initialPriority": if kind == ResourceType::Document { "VeryHigh" } else { "High" },
            "referrerPolicy": "strict-origin-when-cross-origin",
        });
        if !body.is_empty() {
            request["hasPostData"] = json!(true);
            // Text only: a binary body cannot go out as a string, and the
            // `postDataEntries` Chrome would send it in are not produced.
            if let Ok(text) = std::str::from_utf8(body) {
                request["postData"] = json!(text);
            }
        }
        json!({
            "requestId": id,
            "loaderId": self.loader_id,
            "documentURL": self.document_url,
            "request": request,
            "timestamp": timestamp(),
            "wallTime": wall_time(),
            "initiator": { "type": if kind == ResourceType::Document { "other" } else { "script" } },
            "type": kind.as_str(),
            "frameId": self.frame_id,
            "hasUserGesture": false,
        })
    }

    fn keep_body(&mut self, id: String, body: &[u8]) {
        if body.len() > self.max_resource {
            return;
        }
        self.body_bytes += body.len();
        self.bodies.push_back((id, body.to_vec()));
        while self.body_bytes > self.max_total {
            let Some((_, oldest)) = self.bodies.pop_front() else { break };
            self.body_bytes -= oldest.len();
        }
    }
}

/// A `Network.Response`. `Set-Cookie` is left out, as Chrome leaves it to
/// the extra-info event.
fn response_json(response: &Response) -> Value {
    let content_type = response.header("content-type").unwrap_or("");
    let mime = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    json!({
        "url": response.url,
        "status": response.status,
        "statusText": response.reason,
        "headers": header_object(
            response
                .headers
                .iter()
                .filter(|(k, _)| !k.eq_ignore_ascii_case("set-cookie"))
                .map(|(k, v)| (k.as_str(), v.as_str())),
        ),
        "mimeType": mime,
        "charset": response.charset().unwrap_or_default(),
        "connectionReused": false,
        "connectionId": 0,
        "fromDiskCache": false,
        "fromServiceWorker": false,
        "fromPrefetchCache": false,
        "encodedDataLength": response.body.len(),
        "responseTime": wall_time() * 1000.0,
        "protocol": "http/1.1",
        "securityState": if response.url.starts_with("https:") { "secure" } else { "insecure" },
    })
}

/// Headers as CDP's `Headers` object. Repeated names are joined with a
/// newline, as Chrome joins them.
fn header_object<'a>(headers: impl Iterator<Item = (&'a str, &'a str)>) -> Value {
    let mut object = Map::new();
    for (name, value) in headers {
        match object.get_mut(name) {
            Some(Value::String(existing)) => {
                existing.push('\n');
                existing.push_str(value);
            }
            _ => {
                object.insert(name.to_string(), json!(value));
            }
        }
    }
    Value::Object(object)
}

/// Seconds on a monotonic clock, which is what CDP's `MonotonicTime` is.
fn timestamp() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn wall_time() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

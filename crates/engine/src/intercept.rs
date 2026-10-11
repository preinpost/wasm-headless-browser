//! Request interception: CDP's `Fetch` domain, which agent-browser's
//! `network route` drives.
//!
//! A client names URL patterns with `Fetch.enable`. A request matching one
//! stops before it is sent, `Fetch.requestPaused` goes out at once, and the
//! request waits for the client's answer: `continueRequest`, perhaps with a
//! changed URL, method, headers or body; `fulfillRequest`, with a response of
//! the client's own; or `failRequest`. Each redirect hop is paused again, as
//! Chrome does.
//!
//! The thread that sends the request does the waiting, and that is often the
//! one running the page load a client's command started. So the answer
//! cannot queue behind that command: these commands are [`Concurrent`], and
//! the server answers them as they arrive. Disabling the domain, closing the
//! target or the client going away lets every paused request go on as it
//! was.
//!
//! Only the request stage is supported. Pausing at the response stage
//! (`requestStage: "Response"`, `interceptResponse`) is refused rather than
//! ignored. `continueWithAuth` is refused too: the engine does no HTTP
//! authentication, so `Fetch.authRequired` never comes and a 401 reaches the
//! page as it is.

use std::collections::HashMap;
use std::sync::{mpsc, Mutex};

use base64::Engine as _;
use browser_core::{CdpError, Concurrent, LiveEmit};
use serde_json::{json, Value};

use crate::fetch::Response;
use crate::network::{header_object, ResourceType};

/// What the client decided for a paused request.
pub enum Decision {
    Continue(Changes),
    Fulfill(Response),
    /// Fail it with this `net::` error, as `Network.loadingFailed` reports it.
    Fail(String),
}

/// What `continueRequest` changes; `None` keeps the request's own.
#[derive(Default)]
pub struct Changes {
    pub url: Option<String>,
    pub method: Option<String>,
    pub headers: Option<Vec<(String, String)>>,
    pub body: Option<Vec<u8>>,
}

/// A request about to go out, as `Fetch.requestPaused` describes it.
pub struct Outgoing<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
    pub kind: ResourceType,
    /// The `Network.*` request id, when the network log records it.
    pub network_id: Option<&'a str>,
    pub frame_id: &'a str,
    /// Which redirect hop of the request this is, from 0.
    pub hop: usize,
}

/// One target's interception: its patterns and the requests waiting.
#[derive(Default)]
pub struct Interceptor {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    enabled: bool,
    patterns: Vec<Pattern>,
    emit: Option<LiveEmit>,
    jobs: u64,
    /// Paused requests by `requestId`. Dropping a sender lets its request
    /// continue unchanged.
    paused: HashMap<String, mpsc::Sender<Decision>>,
}

struct Pattern {
    url: Vec<Token>,
    kind: Option<String>,
}

#[derive(PartialEq)]
enum Token {
    Char(char),
    /// `*`: any run of characters, none included.
    Any,
    /// `?`: exactly one character.
    One,
}

impl Interceptor {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// How `Fetch.requestPaused` reaches the client.
    pub fn set_emit(&self, emit: LiveEmit) {
        self.lock().emit = Some(emit);
    }

    /// Pauses `request` if a pattern matches it and waits for the client.
    /// `job` numbers the request across its redirect hops; it starts `None`.
    pub fn pause(&self, request: &Outgoing, job: &mut Option<u64>) -> Decision {
        let kind = paused_type(request.kind);
        let (id, answer, emit) = {
            let mut inner = self.lock();
            if !inner.enabled || !inner.patterns.iter().any(|p| p.matches(request.url, kind)) {
                return Decision::Continue(Changes::default());
            }
            let Some(emit) = inner.emit.clone() else { return Decision::Continue(Changes::default()) };
            let job = match *job {
                Some(job) => job,
                None => {
                    inner.jobs += 1;
                    *job = Some(inner.jobs);
                    inner.jobs
                }
            };
            let id = format!("interception-job-{job}.{}", request.hop);
            let (tx, rx) = mpsc::channel();
            inner.paused.insert(id.clone(), tx);
            (id, rx, emit)
        };

        let mut described = json!({
            "url": request.url,
            "method": request.method,
            "headers": header_object(request.headers.iter().map(|(k, v)| (k.as_str(), v.as_str()))),
            "initialPriority": if request.kind == ResourceType::Document { "VeryHigh" } else { "High" },
            "referrerPolicy": "strict-origin-when-cross-origin",
        });
        if !request.body.is_empty() {
            described["hasPostData"] = json!(true);
            if let Ok(text) = std::str::from_utf8(request.body) {
                described["postData"] = json!(text);
            }
        }
        let mut params = json!({
            "requestId": id,
            "request": described,
            "frameId": request.frame_id,
            "resourceType": kind,
        });
        if let Some(network_id) = request.network_id {
            params["networkId"] = json!(network_id);
        }

        // No client to answer: nothing would ever release it.
        if !emit("Fetch.requestPaused", params) {
            self.lock().paused.remove(&id);
            return Decision::Continue(Changes::default());
        }
        answer.recv().unwrap_or(Decision::Continue(Changes::default()))
    }

    fn enable(&self, params: &Value) -> Result<Value, CdpError> {
        // No patterns at all means every request, as in Chrome; an empty
        // list means none.
        let patterns = match params.get("patterns").and_then(Value::as_array) {
            None => vec![Pattern { url: vec![Token::Any], kind: None }],
            Some(list) => {
                let mut patterns = Vec::new();
                for p in list {
                    if p.get("requestStage").and_then(Value::as_str).is_some_and(|stage| stage != "Request") {
                        return Err(CdpError::internal(
                            "Fetch: pausing at the response stage is not supported, only requestStage \"Request\"",
                        ));
                    }
                    patterns.push(Pattern {
                        url: tokens(p.get("urlPattern").and_then(Value::as_str).unwrap_or("*")),
                        kind: p.get("resourceType").and_then(Value::as_str).map(str::to_string),
                    });
                }
                patterns
            }
        };
        let mut inner = self.lock();
        inner.enabled = true;
        inner.patterns = patterns;
        Ok(json!({}))
    }

    /// Stops pausing, and lets every paused request go on as it was.
    pub fn disable(&self) {
        let mut inner = self.lock();
        inner.enabled = false;
        inner.patterns.clear();
        inner.paused.clear();
    }

    /// Releases the request `params` names with what `decide` makes of the
    /// parameters. Parameters that do not parse leave it paused.
    fn answer(&self, params: &Value, decide: fn(&Value) -> Result<Decision, CdpError>) -> Result<Value, CdpError> {
        let mut inner = self.lock();
        if !inner.enabled {
            return Err(CdpError::internal("Fetch domain is not enabled"));
        }
        let id = params.get("requestId").and_then(Value::as_str).unwrap_or("");
        if !inner.paused.contains_key(id) {
            return Err(invalid("Invalid InterceptionId."));
        }
        let decision = decide(params)?;
        if let Some(waiting) = inner.paused.remove(id) {
            let _ = waiting.send(decision);
        }
        Ok(json!({}))
    }
}

impl Concurrent for Interceptor {
    fn handle(&self, method: &str, params: &Value) -> Option<Result<Value, CdpError>> {
        Some(match method {
            "Fetch.enable" => self.enable(params),
            "Fetch.disable" => {
                self.disable();
                Ok(json!({}))
            }
            "Fetch.continueRequest" => self.answer(params, continued),
            "Fetch.fulfillRequest" => self.answer(params, fulfilled),
            "Fetch.failRequest" => self.answer(params, failed),
            "Fetch.continueResponse" | "Fetch.getResponseBody" | "Fetch.takeResponseBodyAsStream" => {
                Err(CdpError::internal(format!("{method}: pausing at the response stage is not supported")))
            }
            "Fetch.continueWithAuth" => {
                Err(CdpError::internal("Fetch.continueWithAuth: the engine does no HTTP authentication"))
            }
            _ => return None,
        })
    }

    fn disconnected(&self) {
        self.disable();
    }
}

impl Pattern {
    fn matches(&self, url: &str, kind: &str) -> bool {
        self.kind.as_deref().is_none_or(|k| k == kind) && glob(&self.url, url)
    }
}

/// The resource type `Fetch.requestPaused` reports. A `fetch()` is `XHR`
/// there, though `Network.*` calls it `Fetch`: Chrome 149 does, and
/// agent-browser's `--resource-type` filters on it.
fn paused_type(kind: ResourceType) -> &'static str {
    match kind {
        ResourceType::Fetch => "XHR",
        other => other.as_str(),
    }
}

/// A `urlPattern`: `*` is any run, `?` one character, and a backslash
/// makes the next character literal.
fn tokens(pattern: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        out.push(match c {
            '*' => Token::Any,
            '?' => Token::One,
            '\\' => Token::Char(chars.next().unwrap_or('\\')),
            c => Token::Char(c),
        });
    }
    out
}

fn glob(pattern: &[Token], text: &str) -> bool {
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    // The last `*` and where in the text it was tried, to widen it by one
    // when what follows fails.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(Token::Any) => {
                star = Some((p, t));
                p += 1;
            }
            Some(Token::One) => {
                p += 1;
                t += 1;
            }
            Some(Token::Char(c)) if *c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|token| *token == Token::Any)
}

fn invalid(message: &str) -> CdpError {
    CdpError { code: -32602, message: message.to_string() }
}

fn base64_param(params: &Value, name: &str) -> Result<Option<Vec<u8>>, CdpError> {
    match params.get(name).and_then(Value::as_str) {
        None => Ok(None),
        Some(text) => base64::engine::general_purpose::STANDARD
            .decode(text)
            .map(Some)
            .map_err(|_| invalid(&format!("Invalid {name}: not base64"))),
    }
}

/// `[{name, value}]`, as `continueRequest` and `fulfillRequest` take
/// headers.
fn header_entries(value: Option<&Value>) -> Option<Vec<(String, String)>> {
    let entries = value?.as_array()?;
    Some(
        entries
            .iter()
            .filter_map(|e| Some((e.get("name")?.as_str()?.to_string(), e.get("value")?.as_str()?.to_string())))
            .collect(),
    )
}

fn continued(params: &Value) -> Result<Decision, CdpError> {
    if params.get("interceptResponse").and_then(Value::as_bool) == Some(true) {
        return Err(CdpError::internal(
            "Fetch.continueRequest: interceptResponse is not supported, the response stage is not paused",
        ));
    }
    Ok(Decision::Continue(Changes {
        url: params.get("url").and_then(Value::as_str).map(str::to_string),
        method: params.get("method").and_then(Value::as_str).map(str::to_string),
        headers: header_entries(params.get("headers")),
        body: base64_param(params, "postData")?,
    }))
}

fn fulfilled(params: &Value) -> Result<Decision, CdpError> {
    let status = params
        .get("responseCode")
        .and_then(Value::as_u64)
        .filter(|code| (100..1000).contains(code))
        .ok_or_else(|| invalid("Invalid responseCode"))? as u16;
    // Lowercased, as responses from the network are, which is how the
    // client looks for `set-cookie` and `location`.
    let mut headers: Vec<(String, String)> = header_entries(params.get("responseHeaders"))
        .unwrap_or_default()
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect();
    // `binaryResponseHeaders`: `name: value` lines, NUL-separated.
    if let Some(raw) = base64_param(params, "binaryResponseHeaders")? {
        for line in String::from_utf8_lossy(&raw).split('\0') {
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
            }
        }
    }
    let reason = match params.get("responsePhrase").and_then(Value::as_str) {
        Some(phrase) => phrase.to_string(),
        None => reason_phrase(status).to_string(),
    };
    let body = base64_param(params, "body")?.unwrap_or_default();
    Ok(Decision::Fulfill(Response { status, reason, headers, body, url: String::new() }))
}

fn failed(params: &Value) -> Result<Decision, CdpError> {
    let error = match params.get("errorReason").and_then(Value::as_str).unwrap_or("") {
        "Failed" => "net::ERR_FAILED",
        "Aborted" => "net::ERR_ABORTED",
        "TimedOut" => "net::ERR_TIMED_OUT",
        "AccessDenied" => "net::ERR_ACCESS_DENIED",
        "ConnectionClosed" => "net::ERR_CONNECTION_CLOSED",
        "ConnectionReset" => "net::ERR_CONNECTION_RESET",
        "ConnectionRefused" => "net::ERR_CONNECTION_REFUSED",
        "ConnectionAborted" => "net::ERR_CONNECTION_ABORTED",
        "ConnectionFailed" => "net::ERR_CONNECTION_FAILED",
        "NameNotResolved" => "net::ERR_NAME_NOT_RESOLVED",
        "InternetDisconnected" => "net::ERR_INTERNET_DISCONNECTED",
        "AddressUnreachable" => "net::ERR_ADDRESS_UNREACHABLE",
        "BlockedByClient" => "net::ERR_BLOCKED_BY_CLIENT",
        "BlockedByResponse" => "net::ERR_BLOCKED_BY_RESPONSE",
        _ => return Err(invalid("Invalid errorReason")),
    };
    Ok(Decision::Fail(error.to_string()))
}

/// The phrase Chrome gives a fulfilled response that names none.
fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn matches(pattern: &str, url: &str) -> bool {
        glob(&tokens(pattern), url)
    }

    #[test]
    fn url_patterns_are_globs() {
        assert!(matches("*", "https://a.test/x"));
        assert!(matches("*/pic.png", "http://127.0.0.1:8765/pic.png"));
        assert!(!matches("*/pic.png", "http://127.0.0.1:8765/pic.png?v=2"));
        assert!(matches("*/api/*", "https://a.test/api/data.json"));
        assert!(matches("*://a.test/*.js", "https://a.test/b/c.js"));
        assert!(!matches("*://a.test/*.js", "https://b.test/c.js"));
        assert!(matches("https://a.test/?", "https://a.test/x"));
        assert!(!matches("https://a.test/?", "https://a.test/xy"));
        assert!(matches("*\\*", "https://a.test/*"));
        assert!(!matches("*\\*", "https://a.test/x"));
        assert!(matches("*a*b*", "xxaxxbxx"));
        assert!(!matches("", "x"));
    }

    /// An interceptor whose client answers each pause with `answer`, from
    /// another thread, as the server does.
    fn paused(answer: impl Fn(&Interceptor, &Value) + Send + Sync + 'static) -> Arc<Interceptor> {
        let interceptor = Arc::new(Interceptor::default());
        let me = Arc::downgrade(&interceptor);
        let answer = Arc::new(answer);
        interceptor.set_emit(Arc::new(move |_method: &str, params: Value| {
            let (me, answer) = (me.clone(), Arc::clone(&answer));
            std::thread::spawn(move || answer(&me.upgrade().unwrap(), &params));
            true
        }));
        interceptor
    }

    fn outgoing(url: &str, kind: ResourceType) -> Outgoing<'_> {
        Outgoing { method: "GET", url, headers: &[], body: &[], kind, network_id: Some("whb.1"), frame_id: "F", hop: 0 }
    }

    #[test]
    fn a_matching_request_waits_for_the_client_and_takes_its_answer() {
        let interceptor = paused(|me, params| {
            let id = params["requestId"].as_str().unwrap();
            let body = base64::engine::general_purpose::STANDARD.encode(b"{\"mock\": true}");
            let answer = me.handle("Fetch.fulfillRequest", &json!({ "requestId": id, "responseCode": 200, "body": body }));
            assert!(answer.unwrap().is_ok());
        });
        assert!(interceptor.handle("Fetch.enable", &json!({ "patterns": [{ "urlPattern": "*/api/*" }] })).unwrap().is_ok());

        let mut job = None;
        match interceptor.pause(&outgoing("https://a.test/api/x", ResourceType::Fetch), &mut job) {
            Decision::Fulfill(response) => {
                assert_eq!((response.status, response.reason.as_str()), (200, "OK"));
                assert_eq!(response.body, b"{\"mock\": true}");
                assert!(response.headers.is_empty());
            }
            _ => panic!("expected the mock"),
        }
        assert_eq!(job, Some(1));
        let mut job = None;
        assert!(matches!(
            interceptor.pause(&outgoing("https://a.test/other", ResourceType::Fetch), &mut job),
            Decision::Continue(_)
        ));
        assert_eq!(job, None, "an unmatched request is not numbered");
    }

    #[test]
    fn resource_types_filter_and_a_fetch_is_xhr_as_chrome_reports_it() {
        let interceptor = Interceptor::default();
        interceptor.enable(&json!({ "patterns": [{ "urlPattern": "*", "resourceType": "XHR" }] })).unwrap();
        let inner = interceptor.lock();
        assert!(inner.patterns[0].matches("https://a.test/", paused_type(ResourceType::Fetch)));
        assert!(!inner.patterns[0].matches("https://a.test/", paused_type(ResourceType::Image)));
    }

    #[test]
    fn answers_are_checked_like_chromes() {
        let interceptor = Interceptor::default();
        let err = |r: Option<Result<Value, CdpError>>| r.unwrap().unwrap_err();
        let e = err(interceptor.handle("Fetch.continueRequest", &json!({ "requestId": "x" })));
        assert_eq!((e.code, e.message.as_str()), (-32000, "Fetch domain is not enabled"));
        interceptor.handle("Fetch.enable", &json!({})).unwrap().unwrap();
        let e = err(interceptor.handle("Fetch.continueRequest", &json!({ "requestId": "x" })));
        assert_eq!((e.code, e.message.as_str()), (-32602, "Invalid InterceptionId."));
        assert!(interceptor.handle("Fetch.enable", &json!({ "patterns": [{ "requestStage": "Response" }] })).unwrap().is_err());
        assert!(interceptor.handle("Network.enable", &json!({})).is_none(), "not ours");
        assert_eq!(failed(&json!({ "errorReason": "BlockedByClient" })).ok().map(|d| match d {
            Decision::Fail(e) => e,
            _ => String::new(),
        }), Some("net::ERR_BLOCKED_BY_CLIENT".into()));
        assert!(failed(&json!({ "errorReason": "Bogus" })).is_err());
    }

    #[test]
    fn disabling_lets_paused_requests_go_on() {
        let interceptor = paused(|me, _| {
            me.handle("Fetch.disable", &json!({})).unwrap().unwrap();
        });
        interceptor.handle("Fetch.enable", &json!({})).unwrap().unwrap();
        let mut job = None;
        assert!(matches!(
            interceptor.pause(&outgoing("https://a.test/", ResourceType::Document), &mut job),
            Decision::Continue(_)
        ));
    }

    #[test]
    fn with_no_client_to_answer_nothing_waits() {
        let interceptor = Interceptor::default();
        interceptor.set_emit(Arc::new(|_: &str, _: Value| false));
        interceptor.handle("Fetch.enable", &json!({})).unwrap().unwrap();
        let mut job = None;
        assert!(matches!(
            interceptor.pause(&outgoing("https://a.test/", ResourceType::Document), &mut job),
            Decision::Continue(_)
        ));
        assert!(interceptor.lock().paused.is_empty());
    }
}

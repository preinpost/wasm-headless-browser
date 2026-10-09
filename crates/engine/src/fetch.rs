//! Fetching a page.
//!
//! HTTP over `std::net`, by hand. No async runtime and no HTTP client
//! dependency, because the WASIX socket path is the one that matters and it is
//! exercised more directly this way.
//!
//! HTTPS terminates inside the guest, with rustls and the ring provider. ring
//! is the one crypto backend the WASIX registry carries a fork of, and the
//! probe in `experiments/tls` showed it handshaking TLS 1.3 under Wasmer.
//! Trust comes from the Mozilla roots in `webpki-roots`, plus any PEM files
//! listed in `WHB_EXTRA_CA_CERTS`, for internal sites signed by a private CA.
//!
//! `data:` URLs are supported because they let the engine be pointed at inline
//! markup, which keeps tests and the harness free of a network.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::cookies::{self, CookieJar, Source};

const MAX_REDIRECTS: usize = 5;
const MAX_BODY: usize = 16 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The URL the body actually came from, after any redirects.
    pub url: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The declared character set, lowercased.
    pub fn charset(&self) -> Option<String> {
        let ctype = self.header("content-type")?;
        let params = ctype.split(';').skip(1);
        for param in params {
            if let Some((key, value)) = param.split_once('=') {
                if key.trim().eq_ignore_ascii_case("charset") {
                    return Some(value.trim().trim_matches('"').to_ascii_lowercase());
                }
            }
        }
        None
    }

    pub fn is_html(&self) -> bool {
        match self.header("content-type") {
            Some(ctype) => ctype.to_ascii_lowercase().contains("html"),
            // A missing content type on a text-ish response is usually HTML in
            // practice, and the parser is forgiving.
            None => true,
        }
    }
}

#[derive(Debug, Clone)]
pub enum FetchError {
    /// The scheme is recognised but not implemented yet.
    Unsupported(String),
    /// The URL could not be understood.
    BadUrl(String),
    /// The connection or the conversation failed.
    Io(String),
    /// The server answered, but with something we will not show.
    Status(u16, String),
    TooManyRedirects,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Unsupported(msg) => write!(f, "{msg}"),
            FetchError::BadUrl(url) => write!(f, "cannot parse url: {url}"),
            FetchError::Io(msg) => write!(f, "{msg}"),
            FetchError::Status(code, reason) => write!(f, "HTTP {code} {reason}"),
            FetchError::TooManyRedirects => write!(f, "too many redirects"),
        }
    }
}

impl std::error::Error for FetchError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn parse(input: &str) -> Result<Url, FetchError> {
        let input = input.trim();
        let Some((scheme, rest)) = input.split_once("://") else {
            return Err(FetchError::BadUrl(input.to_string()));
        };
        let scheme = scheme.to_ascii_lowercase();
        let Some(default_port) = default_port(&scheme) else {
            return Err(FetchError::Unsupported(format!("unsupported scheme: {scheme}")));
        };

        let (authority, path) = match rest.find('/') {
            Some(pos) => (&rest[..pos], &rest[pos..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(FetchError::BadUrl(input.to_string()));
        }

        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => match p.parse::<u16>() {
                Ok(port) => (h.to_string(), port),
                Err(_) => (authority.to_string(), default_port),
            },
            None => (authority.to_string(), default_port),
        };
        if host.is_empty() {
            return Err(FetchError::BadUrl(input.to_string()));
        }

        Ok(Url { scheme, host, port, path: path.to_string() })
    }

    /// Resolves a `Location` header against this URL.
    pub fn join(&self, location: &str) -> Result<Url, FetchError> {
        let location = location.trim();
        if location.contains("://") {
            return Url::parse(location);
        }
        if let Some(path) = location.strip_prefix('/') {
            return Ok(Url {
                scheme: self.scheme.clone(),
                host: self.host.clone(),
                port: self.port,
                path: format!("/{path}"),
            });
        }
        // Relative to the current directory.
        let base = match self.path.rfind('/') {
            Some(pos) => &self.path[..=pos],
            None => "/",
        };
        Ok(Url {
            scheme: self.scheme.clone(),
            host: self.host.clone(),
            port: self.port,
            path: format!("{base}{location}"),
        })
    }

    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.authority())
    }

    /// `host`, or `host:port` when the port is not the scheme's default. This
    /// is also the `Host` header.
    pub fn authority(&self) -> String {
        if Some(self.port) == default_port(&self.scheme) {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    pub fn to_string_with_path(&self) -> String {
        format!("{}{}", self.origin(), self.path)
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

/// One request. `GET` with no body is the common case; a form submission is a
/// `POST` with an encoded body.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub url: String,
    /// Extra request headers. `Host`, `Connection`, `Content-Length` and
    /// `Cookie` are the client's to set and are ignored here.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn get(url: &str) -> Self {
        Request { method: "GET".into(), url: url.to_string(), headers: Vec::new(), body: Vec::new() }
    }

    pub fn post(url: &str, content_type: &str, body: Vec<u8>) -> Self {
        Request {
            method: "POST".into(),
            url: url.to_string(),
            headers: vec![("Content-Type".into(), content_type.to_string())],
            body,
        }
    }
}

/// Fetches with a cookie jar: cookies matching each request are sent, and
/// every `Set-Cookie` received is stored, including those on redirects, which
/// is where login flows usually set their session.
#[derive(Clone)]
pub struct Client {
    tls: Arc<ClientConfig>,
    cookies: Arc<Mutex<CookieJar>>,
    /// Idle keep-alive connections, by `scheme://host:port`, shared by every
    /// clone. A page load is dozens of requests to one host; a new TCP and
    /// TLS handshake for each cost about 100 ms apiece on the reference site.
    idle: Arc<Mutex<HashMap<String, Vec<Connection>>>>,
}

/// Idle connections kept per host, as browsers bound theirs.
const MAX_IDLE_PER_HOST: usize = 6;

impl Client {
    pub fn new(cookies: Arc<Mutex<CookieJar>>) -> Self {
        Self::with_tls(cookies, tls_config())
    }

    pub fn with_tls(cookies: Arc<Mutex<CookieJar>>, tls: Arc<ClientConfig>) -> Self {
        Client { tls, cookies, idle: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub fn cookies(&self) -> &Arc<Mutex<CookieJar>> {
        &self.cookies
    }

    pub fn get(&self, url: &str) -> Result<Response, FetchError> {
        self.send(Request::get(url))
    }

    pub fn send(&self, request: Request) -> Result<Response, FetchError> {
        if let Some(response) = local_response(&request.url)? {
            return Ok(response);
        }

        let mut method = request.method.to_ascii_uppercase();
        let mut headers = request.headers;
        let mut body = request.body;
        let mut current = Url::parse(&request.url)?;
        for _ in 0..=MAX_REDIRECTS {
            let cookie = self.jar().header(&current, cookies::now());
            let response = self.send_once(&current, &method, &headers, cookie.as_deref(), &body)?;
            {
                let mut jar = self.jar();
                let now = cookies::now();
                for (name, value) in &response.headers {
                    if name == "set-cookie" {
                        jar.store(&current, value, Source::Http, now);
                    }
                }
            }
            if (300..400).contains(&response.status) {
                if let Some(location) = response.header("location") {
                    current = current.join(location)?;
                    // What browsers do, not what RFC 7231 first said: 303
                    // always becomes GET, and so do 301 and 302 after a POST.
                    // 307 and 308 repeat the request as it was.
                    let becomes_get = response.status == 303
                        || (matches!(response.status, 301 | 302) && method == "POST");
                    if becomes_get && method != "HEAD" {
                        method = "GET".into();
                        body.clear();
                        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
                    }
                    continue;
                }
            }
            return Ok(response);
        }
        Err(FetchError::TooManyRedirects)
    }

    /// One request and its response, on an idle connection when there is
    /// one. A connection the server closed while it sat idle fails before any
    /// of the response arrives; the request is then sent again on a fresh
    /// connection, as browsers do.
    fn send_once(
        &self,
        url: &Url,
        method: &str,
        headers: &[(String, String)],
        cookie: Option<&str>,
        body: &[u8],
    ) -> Result<Response, FetchError> {
        let key = format!("{}://{}:{}", url.scheme, url.host, url.port);
        let request = request_bytes(url, method, headers, cookie, body);
        let idle = self.idle_pool().get_mut(&key).and_then(Vec::pop);
        if let Some(mut connection) = idle {
            match exchange(&mut connection, &request, method, url) {
                Ok((response, reusable)) => {
                    if reusable {
                        self.keep(key, connection);
                    }
                    return Ok(response);
                }
                Err(Exchange::Stale(_)) => {}
                Err(Exchange::Failed(e)) => return Err(e),
            }
        }
        let mut connection = connect(url, &self.tls)?;
        match exchange(&mut connection, &request, method, url) {
            Ok((response, reusable)) => {
                if reusable {
                    self.keep(key, connection);
                }
                Ok(response)
            }
            Err(Exchange::Stale(e) | Exchange::Failed(e)) => Err(e),
        }
    }

    fn keep(&self, key: String, connection: Connection) {
        let mut idle = self.idle_pool();
        let slot = idle.entry(key).or_default();
        if slot.len() < MAX_IDLE_PER_HOST {
            slot.push(connection);
        }
    }

    fn idle_pool(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<Connection>>> {
        self.idle.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn jar(&self) -> std::sync::MutexGuard<'_, CookieJar> {
        self.cookies.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Retrieves a URL with a jar of its own. `data:` URLs never touch the
/// network.
pub fn get(url: &str) -> Result<Response, FetchError> {
    get_with(url, &tls_config())
}

/// [`get`], trusting the roots in `tls` for `https`.
pub fn get_with(url: &str, tls: &Arc<ClientConfig>) -> Result<Response, FetchError> {
    Client::with_tls(Arc::new(Mutex::new(CookieJar::new())), Arc::clone(tls)).get(url)
}

/// `data:` and `about:` URLs, answered without a network.
fn local_response(url: &str) -> Result<Option<Response>, FetchError> {
    if url.trim_start().starts_with("data:") {
        return data_url(url).map(Some);
    }
    if let Some(rest) = url.trim_start().strip_prefix("about:") {
        // about:blank and friends are a blank document, not an error.
        return Ok(Some(Response {
            status: 200,
            reason: "OK".to_string(),
            headers: vec![("content-type".into(), "text/html; charset=utf-8".into())],
            body: Vec::new(),
            url: format!("about:{rest}"),
        }));
    }
    Ok(None)
}

/// The client configuration every `https` fetch shares. Built once, because
/// parsing the root store is not free.
pub fn tls_config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            // A file that cannot be read is skipped rather than fatal: the
            // public roots still work, and a site that needed the missing CA
            // fails its handshake with a message that names the problem.
            if let Some(paths) = std::env::var_os("WHB_EXTRA_CA_CERTS") {
                for path in std::env::split_paths(&paths) {
                    let Ok(certs) = CertificateDer::pem_file_iter(&path) else { continue };
                    for cert in certs.flatten() {
                        let _ = roots.add(cert);
                    }
                }
            }
            Arc::new(client_config(roots))
        })
        .clone()
}

/// A client configuration trusting exactly `roots`.
pub fn client_config(roots: RootCertStore) -> ClientConfig {
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// A plain or TLS connection, read and written the same way.
enum Connection {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Connection::Plain(s) => s.read(buf),
            Connection::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Connection::Plain(s) => s.write(buf),
            Connection::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Connection::Plain(s) => s.flush(),
            Connection::Tls(s) => s.flush(),
        }
    }
}

fn connect(url: &Url, tls: &Arc<ClientConfig>) -> Result<Connection, FetchError> {
    let address = format!("{}:{}", url.host, url.port);
    let mut stream = TcpStream::connect(&address)
        .map_err(|e| FetchError::Io(format!("connect to {address} failed: {e}")))?;
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    if url.scheme != "https" {
        return Ok(Connection::Plain(stream));
    }
    let name = ServerName::try_from(url.host.clone())
        .map_err(|e| FetchError::BadUrl(format!("{}: {e}", url.host)))?;
    let mut session = ClientConnection::new(Arc::clone(tls), name)
        .map_err(|e| FetchError::Io(format!("TLS setup for {address} failed: {e}")))?;
    // Done here rather than on the first write, so that a certificate the
    // engine does not trust is reported as that and not as a failed write.
    while session.is_handshaking() {
        session
            .complete_io(&mut stream)
            .map_err(|e| FetchError::Io(format!("TLS handshake with {address} failed: {e}")))?;
    }
    Ok(Connection::Tls(Box::new(StreamOwned::new(session, stream))))
}

/// Headers a caller may not set, because the client owns them.
const RESERVED: [&str; 4] = ["host", "connection", "content-length", "cookie"];

fn request_bytes(
    url: &Url,
    method: &str,
    headers: &[(String, String)],
    cookie: Option<&str>,
    body: &[u8],
) -> Vec<u8> {
    let mut head = format!("{method} {} HTTP/1.1\r\nHost: {}\r\n", url.path, url.authority());
    let has = |name: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if !has("user-agent") {
        head.push_str("User-Agent: wasm-headless-browser/0.0.1\r\n");
    }
    if !has("accept") {
        head.push_str("Accept: text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8\r\n");
    }
    head.push_str("Accept-Encoding: identity\r\n");
    if !has("accept-language") {
        head.push_str("Accept-Language: en\r\n");
    }
    for (name, value) in headers {
        if RESERVED.iter().any(|r| name.eq_ignore_ascii_case(r))
            || name.eq_ignore_ascii_case("accept-encoding")
        {
            continue;
        }
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(cookie) = cookie {
        head.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: keep-alive\r\n\r\n");
    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    request
}

/// How one exchange on a connection failed.
enum Exchange {
    /// Nothing of the response arrived. On a reused connection this means
    /// the server had closed it, and the request is safe to send again.
    Stale(FetchError),
    Failed(FetchError),
}

/// Writes `request` and reads one response. Returns the response and whether
/// the connection can carry another.
fn exchange(
    connection: &mut Connection,
    request: &[u8],
    method: &str,
    url: &Url,
) -> Result<(Response, bool), Exchange> {
    let address = format!("{}:{}", url.host, url.port);
    if let Err(e) = connection.write_all(request).and_then(|()| connection.flush()) {
        return Err(Exchange::Stale(FetchError::Io(format!("write to {address} failed: {e}"))));
    }
    let mut raw = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = match connection.read(&mut buffer) {
            Ok(n) => n,
            // Many servers close a TLS connection without close_notify.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
            Err(e) => {
                let error = FetchError::Io(format!("read from {address} failed: {e}"));
                return Err(if raw.is_empty() { Exchange::Stale(error) } else { Exchange::Failed(error) });
            }
        };
        if read == 0 {
            if raw.is_empty() {
                return Err(Exchange::Stale(FetchError::Io(format!(
                    "{address} closed the connection without responding"
                ))));
            }
            // The close delimited the response, and ended the connection.
            let response = parse_response(&raw, &url.to_string_with_path()).map_err(Exchange::Failed)?;
            return Ok((response, false));
        }
        raw.extend_from_slice(&buffer[..read]);
        if let Some(reusable) = response_complete(&raw, method) {
            let response = parse_response(&raw, &url.to_string_with_path()).map_err(Exchange::Failed)?;
            return Ok((response, reusable));
        }
        if raw.len() > MAX_BODY + 64 * 1024 {
            return Err(Exchange::Failed(FetchError::Io(format!("the response from {address} is too large"))));
        }
    }
}

/// Whether `raw` holds a whole response, and if so whether the connection
/// can carry another. `None` while more is needed, which includes a
/// response that ends when the connection closes.
fn response_complete(raw: &[u8], method: &str) -> Option<bool> {
    let (head_end, body_start) = find_header_end(raw)?;
    let head = String::from_utf8_lossy(&raw[..head_end]).to_ascii_lowercase();
    let mut lines = head.lines();
    let status_line = lines.next()?;
    let status: u16 = status_line.split(' ').nth(1)?.trim().parse().ok()?;
    let fields: Vec<(&str, &str)> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim(), v.trim())).collect();
    let field = |name: &str| fields.iter().find(|(k, _)| *k == name).map(|(_, v)| *v);
    let reusable = status_line.starts_with("http/1.1") && !field("connection").is_some_and(|v| v.contains("close"));
    let body = &raw[body_start..];
    if method.eq_ignore_ascii_case("HEAD") || status == 204 || status == 304 {
        return Some(reusable);
    }
    if field("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        return chunked_complete(body).then_some(reusable);
    }
    let length = field("content-length")?.parse::<usize>().ok()?;
    (body.len() >= length).then_some(reusable)
}

/// Whether a chunked body has reached its last chunk and trailer.
fn chunked_complete(mut body: &[u8]) -> bool {
    loop {
        let Some(line_end) = body.iter().position(|b| *b == b'\n') else { return false };
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let Ok(size) = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16) else {
            return false;
        };
        body = &body[line_end + 1..];
        if size == 0 {
            // Trailer fields, then an empty line.
            loop {
                let Some(end) = body.iter().position(|b| *b == b'\n') else { return false };
                let line = &body[..end];
                body = &body[end + 1..];
                if line.is_empty() || line == b"\r" {
                    return true;
                }
            }
        }
        if body.len() < size + 1 {
            return false;
        }
        body = &body[size..];
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        } else if body.starts_with(b"\n") {
            body = &body[1..];
        } else {
            return false;
        }
    }
}

/// Splits the status line, headers and body, then decodes the body.
pub fn parse_response(raw: &[u8], url: &str) -> Result<Response, FetchError> {
    let split = find_header_end(raw)
        .ok_or_else(|| FetchError::Io("response had no header terminator".to_string()))?;
    let head = &raw[..split.0];
    let rest = &raw[split.1..];

    let head = String::from_utf8_lossy(head);
    let mut lines = head.split("\r\n").flat_map(|l| l.split('\n'));
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts.next().unwrap_or("");
    let status: u16 = parts
        .next()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| FetchError::Io(format!("unparseable status line: {status_line:?}")))?;
    let reason = parts.next().unwrap_or("").trim().to_string();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    let header = |name: &str| -> Option<String> {
        headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    };

    let body = if header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        decode_chunked(rest)?
    } else if let Some(length) = header("content-length").and_then(|v| v.trim().parse::<usize>().ok()) {
        let take = length.min(rest.len());
        rest[..take].to_vec()
    } else {
        rest.to_vec()
    };

    Ok(Response { status, reason, headers, body, url: url.to_string() })
}

/// Returns `(start of the terminator, index just past it)`.
pub(crate) fn find_header_end(raw: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < raw.len() {
        if raw[i] == b'\n' && raw[i + 1] == b'\n' {
            return Some((i, i + 2));
        }
        if i + 3 < raw.len()
            && raw[i] == b'\r'
            && raw[i + 1] == b'\n'
            && raw[i + 2] == b'\r'
            && raw[i + 3] == b'\n'
        {
            return Some((i, i + 4));
        }
        i += 1;
    }
    None
}

fn decode_chunked(mut rest: &[u8]) -> Result<Vec<u8>, FetchError> {
    let mut out = Vec::new();
    loop {
        // Chunk size line, possibly with an extension after a semicolon.
        let Some(line_end) = rest.iter().position(|b| *b == b'\n') else {
            break;
        };
        let line = String::from_utf8_lossy(&rest[..line_end]);
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).unwrap_or(0);
        rest = &rest[line_end + 1..];
        if size == 0 {
            break;
        }
        if rest.len() < size {
            out.extend_from_slice(rest);
            break;
        }
        out.extend_from_slice(&rest[..size]);
        // Skip the chunk data and its trailing CRLF.
        rest = &rest[size..];
        if rest.starts_with(b"\r\n") {
            rest = &rest[2..];
        } else if rest.starts_with(b"\n") {
            rest = &rest[1..];
        }
        if out.len() > MAX_BODY {
            break;
        }
    }
    Ok(out)
}

/// `data:[<mediatype>][;base64],<data>`
fn data_url(url: &str) -> Result<Response, FetchError> {
    let rest = &url.trim_start()[5..];
    let (meta, payload) = rest
        .split_once(',')
        .ok_or_else(|| FetchError::BadUrl(url.to_string()))?;

    let base64_encoded = meta.split(';').any(|part| part.eq_ignore_ascii_case("base64"));
    let content_type = {
        let first = meta.split(';').next().unwrap_or("");
        if first.is_empty() {
            "text/plain;charset=US-ASCII".to_string()
        } else {
            first.to_string()
        }
    };

    let body = if base64_encoded {
        decode_base64(payload)?
    } else {
        percent_decode(payload)
    };

    Ok(Response {
        status: 200,
        reason: "OK".to_string(),
        headers: vec![("content-type".into(), content_type)],
        body,
        url: url.to_string(),
    })
}

fn decode_base64(input: &str) -> Result<Vec<u8>, FetchError> {
    use base64::Engine as _;
    let cleaned: String = input.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(cleaned.as_bytes())
        .map_err(|e| FetchError::BadUrl(format!("bad base64 payload: {e}")))
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serves one canned response and returns the URL to fetch it from.
    fn serve_once(response: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    fn client() -> Client {
        Client::new(Arc::new(Mutex::new(CookieJar::new())))
    }

    use crate::test_server::serve_script;

    #[test]
    fn a_login_post_sets_a_cookie_on_the_redirect_and_the_next_request_sends_it() {
        let (base, requests) = serve_script(vec![
            "HTTP/1.1 302 Found\r\nLocation: /home\r\n\
             Set-Cookie: sid=abc123; Path=/; HttpOnly\r\nContent-Length: 0\r\n\r\n"
                .to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 7\r\n\r\nwelcome".to_string(),
        ]);
        let client = client();
        let got = client
            .send(Request::post(
                &format!("{base}/login"),
                "application/x-www-form-urlencoded",
                b"user=kim&pass=pw".to_vec(),
            ))
            .expect("login");
        assert_eq!(got.body, b"welcome");
        assert!(got.url.ends_with("/home"), "final url: {}", got.url);

        let login = requests.recv().unwrap();
        assert!(login.starts_with("POST /login HTTP/1.1\r\n"), "{login}");
        assert!(login.contains("Content-Type: application/x-www-form-urlencoded\r\n"), "{login}");
        assert!(login.contains("Content-Length: 16\r\n"), "{login}");
        assert!(login.ends_with("\r\n\r\nuser=kim&pass=pw"), "{login}");

        // 302 after POST becomes GET, drops the body, and carries the cookie.
        let home = requests.recv().unwrap();
        assert!(home.starts_with("GET /home HTTP/1.1\r\n"), "{home}");
        assert!(home.contains("Cookie: sid=abc123\r\n"), "{home}");
        assert!(!home.contains("Content-Type"), "{home}");
        assert!(!home.contains("user=kim"), "{home}");
    }

    #[test]
    fn requests_to_one_host_share_a_connection() {
        // One accepted connection answers both requests, chunked then sized.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = tx.send(crate::test_server::read_request(&mut stream));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\none\r\n0\r\n\r\n")
                .unwrap();
            let _ = tx.send(crate::test_server::read_request(&mut stream));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\ntwo").unwrap();
            // A second connection would wait here forever.
            let _ = listener.accept();
        });
        let client = client();
        assert_eq!(client.get(&format!("http://127.0.0.1:{port}/a")).unwrap().body, b"one");
        assert_eq!(client.get(&format!("http://127.0.0.1:{port}/b")).unwrap().body, b"two");
        assert!(rx.recv().unwrap().contains("Connection: keep-alive\r\n"));
        assert!(rx.recv().unwrap().starts_with("GET /b "));
    }

    #[test]
    fn a_connection_the_server_closed_is_replaced() {
        // serve_script answers one request per connection and closes it, so
        // the second request finds its pooled connection dead.
        let (base, _requests) = serve_script(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n1".to_string(),
            "HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n2".to_string(),
        ]);
        let client = client();
        assert_eq!(client.get(&format!("{base}/")).unwrap().body, b"1");
        assert_eq!(client.get(&format!("{base}/")).unwrap().body, b"2");
    }

    #[test]
    fn knows_when_a_response_is_complete() {
        assert_eq!(response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx", "GET"), None);
        assert_eq!(response_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nxy", "GET"), Some(true));
        assert_eq!(
            response_complete(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n", "GET"),
            Some(false)
        );
        assert_eq!(response_complete(b"HTTP/1.1 200 OK\r\n\r\nuntil close", "GET"), None);
        assert_eq!(response_complete(b"HTTP/1.1 304 Not Modified\r\n\r\n", "GET"), Some(true));
        assert!(!chunked_complete(b"3\r\none\r\n"));
        assert!(chunked_complete(b"3\r\none\r\n0\r\nX-Trailer: 1\r\n\r\n"));
    }

    #[test]
    fn a_307_repeats_the_post() {
        let (base, requests) = serve_script(vec![
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: /again\r\nContent-Length: 0\r\n\r\n".to_string(),
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_string(),
        ]);
        client()
            .send(Request::post(&format!("{base}/first"), "text/plain", b"data".to_vec()))
            .expect("post");
        let _ = requests.recv().unwrap();
        let again = requests.recv().unwrap();
        assert!(again.starts_with("POST /again HTTP/1.1\r\n"), "{again}");
        assert!(again.ends_with("\r\n\r\ndata"), "{again}");
    }

    #[test]
    fn the_jar_is_shared_by_clients_that_share_it() {
        let (base, requests) = serve_script(vec![
            "HTTP/1.1 200 OK\r\nSet-Cookie: a=1\r\nContent-Length: 0\r\n\r\n".to_string(),
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_string(),
        ]);
        let jar = Arc::new(Mutex::new(CookieJar::new()));
        Client::new(Arc::clone(&jar)).get(&format!("{base}/")).unwrap();
        Client::new(Arc::clone(&jar)).get(&format!("{base}/")).unwrap();
        let _ = requests.recv().unwrap();
        assert!(requests.recv().unwrap().contains("Cookie: a=1\r\n"));
    }

    #[test]
    fn parses_urls() {
        let url = Url::parse("http://example.com/a/b?c=1").unwrap();
        assert_eq!(url.host, "example.com");
        assert_eq!(url.port, 80);
        assert_eq!(url.path, "/a/b?c=1");
        assert_eq!(url.origin(), "http://example.com");
        assert_eq!(url.to_string_with_path(), "http://example.com/a/b?c=1");

        let url = Url::parse("http://example.com:8080").unwrap();
        assert_eq!(url.port, 8080);
        assert_eq!(url.path, "/");
        assert_eq!(url.origin(), "http://example.com:8080");
    }

    #[test]
    fn parses_https_urls_with_their_own_default_port() {
        let url = Url::parse("https://example.com/a").unwrap();
        assert_eq!(url.port, 443);
        assert_eq!(url.origin(), "https://example.com");
        assert_eq!(url.authority(), "example.com");

        let url = Url::parse("https://example.com:8443/").unwrap();
        assert_eq!(url.origin(), "https://example.com:8443");
        // Port 80 is not the default for https, so it is spelled out.
        assert_eq!(Url::parse("https://example.com:80/").unwrap().authority(), "example.com:80");
    }

    #[test]
    fn refuses_other_schemes_with_a_reason() {
        let err = Url::parse("ftp://example.com/").unwrap_err();
        assert!(matches!(err, FetchError::Unsupported(_)));
        assert!(err.to_string().contains("ftp"), "message should say which: {err}");
    }

    /// Serves one canned response over TLS, with a certificate for
    /// `localhost` that only the returned client configuration trusts.
    fn serve_tls_once(response: String) -> (String, Arc<ClientConfig>) {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("certificate");
        let cert_der = cert.der().clone();
        let key_der = rustls::pki_types::PrivateKeyDer::try_from(signing_key.serialize_der())
            .expect("private key");

        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("server config");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let session = rustls::ServerConnection::new(Arc::new(server)).unwrap();
                let mut tls = StreamOwned::new(session, stream);
                let mut buf = [0u8; 2048];
                if tls.read(&mut buf).is_ok() {
                    let _ = tls.write_all(response.as_bytes());
                    tls.conn.send_close_notify();
                    let _ = tls.flush();
                }
            }
        });

        let mut roots = RootCertStore::empty();
        roots.add(cert_der).expect("trust the test certificate");
        (format!("https://localhost:{port}/"), Arc::new(client_config(roots)))
    }

    #[test]
    fn fetches_over_tls() {
        let (url, tls) = serve_tls_once(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 6\r\n\r\nsecure".to_string(),
        );
        let got = get_with(&url, &tls).expect("fetch over TLS");
        assert_eq!(got.status, 200);
        assert_eq!(got.body, b"secure");
        assert_eq!(got.url, url);
    }

    #[test]
    fn an_untrusted_certificate_is_reported_as_a_handshake_failure() {
        // The default configuration trusts public roots only, and the test
        // server's certificate is self-signed.
        let (url, _) = serve_tls_once("HTTP/1.1 200 OK\r\n\r\n".to_string());
        let err = get(&url).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("TLS handshake"), "message: {message}");
        assert!(message.to_ascii_lowercase().contains("certificate"), "message: {message}");
    }

    #[test]
    fn resolves_relative_locations() {
        let base = Url::parse("http://example.com/a/b").unwrap();
        assert_eq!(base.join("/x").unwrap().path, "/x");
        assert_eq!(base.join("c").unwrap().path, "/a/c");
        assert_eq!(base.join("http://other.test/z").unwrap().host, "other.test");
    }

    #[test]
    fn decodes_content_length_bodies() {
        let body = "<html>hi</html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let url = serve_once(response);
        let got = get(&url).expect("fetch");
        assert_eq!(got.status, 200);
        assert_eq!(got.body, body.as_bytes());
        assert_eq!(got.charset(), None);
        assert!(got.is_html());
    }

    #[test]
    fn decodes_chunked_bodies() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nTransfer-Encoding: chunked\r\n\r\n\
                        5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"
            .to_string();
        let url = serve_once(response);
        let got = get(&url).expect("fetch");
        assert_eq!(got.body, b"hello world");
    }

    #[test]
    fn follows_redirects() {
        let target = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\n\r\nok".to_string(),
        );
        let target_port = Url::parse(&target).unwrap().port;
        let redirect = serve_once(format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target_port}/\r\nContent-Length: 0\r\n\r\n"
        ));
        let got = get(&redirect).expect("fetch");
        assert_eq!(got.body, b"ok");
        assert!(got.url.contains(&target_port.to_string()), "final url: {}", got.url);
    }

    #[test]
    fn reports_status_without_a_body_read() {
        let url = serve_once(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/html\r\nContent-Length: 3\r\n\r\nno!".to_string(),
        );
        let got = get(&url).expect("fetch");
        // A 404 still returns the body; deciding what to do with it is the
        // caller's business, because error pages are frequently useful.
        assert_eq!(got.status, 404);
        assert_eq!(got.reason, "Not Found");
        assert_eq!(got.body, b"no!");
    }

    #[test]
    fn reads_content_type_parameters() {
        let url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=ISO-8859-1\r\nContent-Length: 0\r\n\r\n"
                .to_string(),
        );
        let got = get(&url).expect("fetch");
        assert_eq!(got.charset().as_deref(), Some("iso-8859-1"));
    }

    #[test]
    fn handles_data_urls() {
        let got = get("data:text/html,<h1>Hi</h1>").expect("data url");
        assert_eq!(got.body, b"<h1>Hi</h1>");
        assert_eq!(got.header("content-type"), Some("text/html"));

        let got = get("data:text/html;base64,PGgxPkhpPC9oMT4=").expect("data url");
        assert_eq!(got.body, b"<h1>Hi</h1>");
    }

    #[test]
    fn percent_decodes_data_urls() {
        let got = get("data:text/html,%3Ch1%3EHi%3C%2Fh1%3E").expect("data url");
        assert_eq!(got.body, b"<h1>Hi</h1>");
    }

    #[test]
    fn about_blank_is_an_empty_page() {
        let got = get("about:blank").expect("about url");
        assert_eq!(got.status, 200);
        assert!(got.body.is_empty());
        assert_eq!(got.url, "about:blank");
    }

    #[test]
    fn connection_failure_is_reported_clearly() {
        // Port 1 on loopback is not listening.
        let err = get("http://127.0.0.1:1/").unwrap_err();
        assert!(matches!(err, FetchError::Io(_)));
        assert!(err.to_string().contains("connect"), "message: {err}");
    }
}

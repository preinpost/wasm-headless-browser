//! The CDP engine, backed by a real document and a real JavaScript engine.
//!
//! `engine-fake` answered from a fixed model. This answers from whatever the
//! last navigation actually fetched, runs the page's own script against it, and
//! projects the accessibility tree from the result.
//!
//! What is still missing is **layout**. There is no CSS cascade and no box
//! model, so `getBoundingClientRect` reports zero-sized boxes, hit testing finds
//! nothing and a screenshot would be blank. Methods that depend on those return
//! an error that says so rather than a plausible-looking lie.

use std::sync::{Arc, Mutex, MutexGuard};

use browser_core::{ids, CdpError, Concurrent, Engine, EngineEmit, LiveEmit, TargetInfo};
use serde_json::{json, Value};

use crate::cookies::{self, Cookie, CookieJar};
use crate::secrets::Secrets;
use crate::dom::{Document, NodeData, NodeId};
use crate::fetch::{self, Request};
use crate::bindings::Navigation;
use crate::images::ImageStore;
use crate::screencast::{Format, Screencast};
use crate::js::{JsValue, ScriptHost};
use crate::network::ResourceType;
use crate::parse::parse_html;
use crate::select;

/// How many times a script-initiated navigation is followed before stopping.
/// A page that redirects itself in a loop should not hang the engine.
const MAX_SCRIPT_NAVIGATIONS: u8 = 3;

/// Virtual milliseconds of timers a page gets to settle after loading, and
/// after each `Runtime.evaluate`. Time is virtual, so this costs only the work
/// the timers do, not wall-clock waiting.
const LOAD_BUDGET_MS: f64 = 5_000.0;
const EVALUATE_BUDGET_MS: f64 = 1_000.0;

/// Upper bound on tasks per settling pass, against pages that schedule work
/// forever.
const MAX_TASKS: usize = 10_000;

pub struct HtmlEngine {
    target_id: String,
    /// Shared with the script thread, which is why this is an `Arc<Mutex<_>>`
    /// rather than a plain `Document`: page script mutates the same tree the CDP
    /// surface reads.
    doc: Arc<Mutex<Document>>,
    /// `None` when the script thread could not start. The engine then still
    /// serves navigation and the accessibility tree, and reports why
    /// `Runtime.evaluate` is unavailable instead of failing silently.
    script: Option<ScriptHost>,
    /// Fetches with the jar every target shares.
    client: fetch::Client,
    /// This target's images, attached to every document it shows.
    images: Arc<ImageStore>,
    /// Set between `Page.startScreencast` and `Page.stopScreencast`.
    screencast: Option<Screencast>,
    /// Set by `Runtime.enable`: console messages and exceptions are sent.
    runtime_enabled: bool,
    /// `Runtime.*` events collected from the page and not sent yet.
    page_events: Vec<(String, Value)>,
    /// The last `exceptionId` given.
    exceptions: u64,
    /// Credentials, typed only through placeholders and scrubbed from
    /// everything the engine says. Shared by every target.
    secrets: Arc<Secrets>,
    url: String,
    frame_id: String,
    loader_id: String,
    context_id: u64,
    history: Vec<HistoryEntry>,
    history_index: usize,
    /// Counts documents loaded, so history entries know which share one.
    document_number: u64,
    viewport: (f64, f64),
    last_error: Option<String>,
}

impl HtmlEngine {
    /// An engine with a cookie jar of its own.
    pub fn new(target_id: &str) -> Self {
        Self::with_cookies(target_id, Arc::new(Mutex::new(CookieJar::new())))
    }

    /// An engine sharing `cookies` with others, as tabs in one browser do.
    pub fn with_cookies(target_id: &str, cookies: Arc<Mutex<CookieJar>>) -> Self {
        Self::with_shared(target_id, cookies, Arc::new(Secrets::new()))
    }

    /// An engine sharing a cookie jar and a set of secrets with others.
    pub fn with_shared(target_id: &str, cookies: Arc<Mutex<CookieJar>>, secrets: Arc<Secrets>) -> Self {
        let mut counter = ids::seed();
        let doc = Arc::new(Mutex::new(Document::new()));
        let viewport = (800.0, 600.0);
        let client = fetch::Client::new(cookies);
        let script = ScriptHost::spawn(Arc::clone(&doc), client.clone(), viewport, USER_AGENT).ok();
        trace_resources("script host created");
        let frame_id = ids::token(&mut counter);
        client.log().set_frame(&frame_id);
        let images = Arc::new(ImageStore::new(client.clone()));
        if let Ok(mut d) = doc.lock() {
            d.set_images(Arc::clone(&images));
        }

        HtmlEngine {
            target_id: target_id.to_string(),
            doc,
            script,
            client,
            images,
            screencast: None,
            runtime_enabled: false,
            page_events: Vec::new(),
            exceptions: 0,
            secrets,
            url: "about:blank".to_string(),
            frame_id,
            loader_id: ids::token(&mut counter),
            context_id: 1,
            history: vec![HistoryEntry { url: "about:blank".to_string(), document: 0, state: None }],
            history_index: 0,
            document_number: 0,
            viewport,
            last_error: None,
        }
    }

    /// Every target the factory creates shares one cookie jar.
    pub fn factory() -> browser_core::EngineFactory {
        Self::factory_with_secrets(Arc::new(Secrets::new()))
    }

    /// [`factory`](Self::factory), with credentials every target can type.
    pub fn factory_with_secrets(secrets: Arc<Secrets>) -> browser_core::EngineFactory {
        let cookies = Arc::new(Mutex::new(CookieJar::new()));
        Box::new(move |target_id: &str| {
            Box::new(HtmlEngine::with_shared(target_id, Arc::clone(&cookies), Arc::clone(&secrets))) as Box<dyn Engine>
        })
    }

    fn jar(&self) -> MutexGuard<'_, CookieJar> {
        self.client.cookies().lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Borrows the document. Never held across a call into JavaScript: the
    /// script thread locks the same mutex, and holding it would deadlock.
    fn doc(&self) -> MutexGuard<'_, Document> {
        self.doc.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn origin(&self) -> String {
        match fetch::Url::parse(&self.url) {
            Ok(url) => url.origin(),
            Err(_) => "null".to_string(),
        }
    }

    fn frame_json(&self) -> Value {
        json!({
            "id": self.frame_id,
            "loaderId": self.loader_id,
            "url": self.url,
            "domainAndRegistry": "",
            "mimeType": "text/html",
            "securityOrigin": self.origin(),
            "secureContextType": "Insecure",
            "crossOriginIsolatedContextType": "NotIsolated",
            "gatedAPIFeatures": [],
        })
    }

    fn emit_context(&self, out: &mut EngineEmit) {
        out(
            "Runtime.executionContextCreated",
            json!({
                "context": {
                    "id": self.context_id,
                    "origin": self.origin(),
                    "name": "",
                    "uniqueId": format!("{}.{}", self.frame_id, self.context_id),
                    "auxData": { "isDefault": true, "type": "default", "frameId": self.frame_id },
                }
            }),
        );
    }

    fn emit_lifecycle(&self, out: &mut EngineEmit) {
        // The requests the load made come first, as the document's own
        // request does in Chrome.
        self.emit_network(out);
        let timestamp = now();
        out("Page.frameStartedLoading", json!({ "frameId": self.frame_id }));
        out("Page.frameNavigated", json!({ "frame": self.frame_json(), "type": "Navigation" }));
        out("Runtime.executionContextsCleared", json!({}));
        self.emit_context(out);
        out("Page.domContentEventFired", json!({ "timestamp": timestamp }));
        out("Page.loadEventFired", json!({ "timestamp": timestamp }));
    }

    /// Every image the document shows: `<img>`s and CSS backgrounds.
    fn image_urls(&self) -> Vec<String> {
        let doc = self.doc();
        let mut urls: Vec<String> = doc
            .descendants(doc.document_id())
            .filter(|id| doc.is_tag(*id, "img"))
            .filter_map(|id| doc.image_url(id))
            .collect();
        urls.extend(crate::paint::background_urls(&doc));
        urls.sort();
        urls.dedup();
        urls
    }

    /// `Page.captureScreenshot`: the viewport, or `clip`, painted as PNG.
    fn screenshot(&self, params: &Value) -> Result<Value, CdpError> {
        let format = Format::parse(params.get("format").and_then(Value::as_str), params.get("quality").and_then(Value::as_u64))
            .map_err(|e| CdpError::internal(format!("Page.captureScreenshot: {e}")))?;
        let (w, h) = self.viewport;
        let clip = params.get("clip");
        let number = |name: &str, default: f64| {
            clip.and_then(|c| c.get(name)).and_then(|v| v.as_f64()).unwrap_or(default)
        };
        // Without a clip, the viewport where it is scrolled to; a clip is in
        // page space, as Chrome takes it.
        let (scroll_x, scroll_y) = {
            let doc = self.doc();
            doc.scroll_offset(doc.document_id())
        };
        let region = crate::paint::Region {
            x: number("x", scroll_x),
            y: number("y", scroll_y),
            width: number("width", w),
            height: number("height", h),
            scale: number("scale", 1.0),
        };
        let pixels = region.width * region.scale * region.height * region.scale;
        if !(region.width > 0.0 && region.height > 0.0 && region.scale > 0.0) || pixels > MAX_SCREENSHOT_PIXELS {
            return Err(CdpError::internal(format!(
                "Page.captureScreenshot: a {}x{} region at scale {} is empty or larger than {} pixels",
                region.width, region.height, region.scale, MAX_SCREENSHOT_PIXELS
            )));
        }
        // Images script added since the load, and backgrounds still on their
        // way, are waited for, without the document lock.
        let urls = self.image_urls();
        self.images.wait_all(&urls);
        let secrets = Arc::clone(&self.secrets);
        let focus = self.script.as_ref().and_then(|s| s.state().focused());
        let canvas = crate::paint::paint(&self.doc(), region, focus, &|text| secrets.scrub_str(text));
        use base64::Engine as _;
        Ok(json!({ "data": base64::engine::general_purpose::STANDARD.encode(format.encode(&canvas)) }))
    }

    /// Sends a screencast frame if one is running, the client is ready, and
    /// the page changed. Called at the end of every command and navigation:
    /// nothing changes the page between them.
    fn emit_frame(&mut self, out: &mut EngineEmit) {
        if self.screencast.is_none() {
            return;
        }
        let secrets = Arc::clone(&self.secrets);
        let scrub = |text: &str| secrets.scrub_str(text);
        let (url, viewport) = (self.url.clone(), self.viewport);
        let focus = self.script.as_ref().and_then(|s| s.state().focused());
        let frame = {
            let doc = self.doc.lock().unwrap_or_else(|p| p.into_inner());
            self.screencast.as_mut().and_then(|cast| cast.frame(&doc, &url, viewport, focus, &scrub))
        };
        if let Some(frame) = frame {
            out("Page.screencastFrame", frame);
        }
    }

    /// Takes the console messages and exceptions the page queued, stamped
    /// with when and in which context, if a client asked for them; they are
    /// dropped otherwise, as Chrome does not keep them for a later
    /// `Runtime.enable` either beyond what DevTools shows.
    fn collect_page_events(&mut self) {
        let Some(script) = &self.script else { return };
        let events = script.take_page_events();
        if !self.runtime_enabled {
            return;
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        for (method, mut params) in events {
            params["timestamp"] = json!(timestamp);
            if method == "Runtime.exceptionThrown" {
                self.exceptions += 1;
                params["exceptionDetails"]["exceptionId"] = json!(self.exceptions);
                params["exceptionDetails"]["executionContextId"] = json!(self.context_id);
            } else {
                params["executionContextId"] = json!(self.context_id);
            }
            self.page_events.push((method, params));
        }
        let excess = self.page_events.len().saturating_sub(MAX_PAGE_EVENTS);
        self.page_events.drain(..excess);
    }

    /// Sends what [`collect_page_events`](Self::collect_page_events) has.
    fn emit_page_events(&mut self, out: &mut EngineEmit) {
        self.collect_page_events();
        for (method, params) in std::mem::take(&mut self.page_events) {
            out(&method, params);
        }
    }

    /// Sends the `Network.*` events recorded since the last call. Requests
    /// run on other threads and inside script, where no emitter is at hand,
    /// so they are recorded and sent from here: before the lifecycle events
    /// of a load, and at the end of every command.
    fn emit_network(&self, out: &mut EngineEmit) {
        for (method, params) in self.client.log().take_events() {
            out(&method, params);
        }
    }

    /// Fetches, parses, and runs the page's own script.
    fn load(&mut self, url: &str) {
        self.load_request(Request::get(url));
    }

    /// [`load`](Self::load) for any request, such as a form's `POST`.
    fn load_request(&mut self, request: Request) {
        self.replace_document(request);
        self.run_document_scripts();
        self.follow_script_navigation();
        self.push_history();
        self.sync_script_url();
    }

    /// Follows the URL changes script made without loading anything,
    /// `pushState`, `replaceState` and new fragments, into history. Returns
    /// the URLs moved to.
    fn sync_script_url(&mut self) -> Vec<String> {
        let Some(script) = &self.script else { return Vec::new() };
        let changes = script.state().take_url_changes();
        let mut moved = Vec::new();
        for change in changes {
            let state = Some(change.state).filter(|s| s != "null");
            if change.replace {
                if let Some(entry) = self.history.get_mut(self.history_index) {
                    entry.url = change.url.clone();
                    entry.state = state;
                }
            } else {
                self.history.truncate(self.history_index + 1);
                self.history.push(HistoryEntry { url: change.url.clone(), document: self.document_number, state });
                self.history_index = self.history.len() - 1;
            }
            if change.url != self.url {
                self.url = change.url.clone();
                moved.push(change.url);
            }
        }
        self.publish_history_shape();
        moved
    }

    /// Tells script where history stands, for `history.length`.
    fn publish_history_shape(&self) {
        if let Some(script) = &self.script {
            script.state().set_history_shape(self.history_index, self.history.len());
        }
    }

    /// Fetches `request` and makes the result the document. A fetch failure
    /// becomes an error page, which is what a browser shows and a client can
    /// read.
    fn replace_document(&mut self, request: Request) {
        trace_resources("load start");
        // The document lock borrows `self`, so everything that touches other
        // fields is collected here and applied once the guard is dropped.
        let request = request.of_kind(ResourceType::Document);
        let requested = request.url.clone();
        self.document_number += 1;
        self.client.clear_preloads();
        self.images.clear();
        let fetched = self.client.send(request);
        let mut new_url = requested.clone();
        let new_error;

        {
            let mut doc = self.doc();
            match fetched {
                Ok(response) => {
                    let content_type = response.header("content-type");
                    if response.is_html() {
                        let encoding = crate::encoding::sniff_document(&response.body, content_type);
                        let (text, _, _) = encoding.decode(&response.body);
                        *doc = parse_html(&text);
                        doc.set_encoding(encoding);
                    } else {
                        // Non-HTML is wrapped so the client sees something rather
                        // than an empty document with a misleading title.
                        let text = crate::encoding::decode_body(&response.body, content_type, encoding_rs::UTF_8);
                        let escaped = escape_html(&text);
                        let label = response.header("content-type").unwrap_or("document");
                        *doc = parse_html(&format!(
                            "<!doctype html><title>{}</title><body><pre>{escaped}</pre>",
                            escape_html(label)
                        ));
                    }
                    doc.set_url(&response.url);
                    doc.set_viewport(self.viewport);
                    doc.set_images(Arc::clone(&self.images));
                    new_url = response.url.clone();
                    new_error = None;
                }
                Err(e) => {
                    let message = e.to_string();
                    *doc = parse_html(&error_page(&requested, &message));
                    doc.set_url(&requested);
                    doc.set_viewport(self.viewport);
                    doc.set_images(Arc::clone(&self.images));
                    new_error = Some(message);
                }
            }
        }

        self.url = new_url;
        self.last_error = new_error;

        self.preload_subresources();
        trace_resources("preloads queued");
        self.load_stylesheets();
        trace_resources("stylesheets loaded");

        // What the old document logged goes before its context does.
        self.collect_page_events();
        // A new document gets a new global environment, as in a browser. The
        // document lock is released above, which this needs.
        if let Some(script) = &self.script {
            if let Err(e) = script.reset() {
                self.last_error = Some(format!("could not reset the script context: {}", e.message));
            }
        }
    }

    /// Starts fetching every stylesheet and classic script the parser found,
    /// all at once, as a browser's preload scanner does. They are still used
    /// in the order the page puts them; this only stops each waiting for the
    /// one before it to download. Scripts download while the stylesheets are
    /// parsed and while earlier scripts run.
    fn preload_subresources(&self) {
        let Ok(base) = url::Url::parse(&self.url) else { return };
        let urls: Vec<(String, ResourceType)> = {
            let doc = self.doc();
            doc.descendants(doc.document_id())
                .filter_map(|id| {
                    let (href, kind) = if doc.is_tag(id, "link") && has_token(doc.attr(id, "rel"), "stylesheet") {
                        (doc.attr(id, "href"), ResourceType::Stylesheet)
                    } else if doc.is_tag(id, "link") && has_token(doc.attr(id, "rel"), "modulepreload") {
                        (doc.attr(id, "href"), ResourceType::Script)
                    } else if doc.is_tag(id, "script") && runs_script_type(doc.attr(id, "type"), doc.has_attr(id, "nomodule")) {
                        (doc.attr(id, "src"), ResourceType::Script)
                    } else if doc.is_tag(id, "img") {
                        return doc.image_url(id).map(|url| (url, ResourceType::Image));
                    } else {
                        return None;
                    };
                    Some((base.join(href?).ok()?.into(), kind))
                })
                .collect()
        };
        for (url, kind) in urls {
            // Images go to the image store, which keeps them for layout and
            // painting; the rest wait in the client for whoever asks.
            if kind == ResourceType::Image {
                self.images.request(&url);
            } else {
                self.client.preload(&url, kind);
            }
        }
    }

    /// Fetches the stylesheets the parser found, before any script runs, as a
    /// browser holds scripts back for them. They were
    /// [preloaded](Self::preload_subresources), so this mostly waits. Ones
    /// that fail are left out of the cascade, as a browser leaves them out.
    fn load_stylesheets(&mut self) {
        let links: Vec<(NodeId, String)> = {
            let doc = self.doc();
            doc.descendants(doc.document_id())
                .filter(|id| doc.is_tag(*id, "link") && has_token(doc.attr(*id, "rel"), "stylesheet"))
                .filter_map(|id| doc.attr(id, "href").map(|href| (id, href.to_string())))
                .collect()
        };
        for (id, href) in links {
            let Ok(url) = url::Url::parse(&self.url).and_then(|base| base.join(&href)) else { continue };
            let request = Request::get(url.as_str()).of_kind(ResourceType::Stylesheet);
            let Ok(response) = self.client.send(request) else { continue };
            if (200..300).contains(&response.status) {
                let fallback = self.doc().encoding();
                let text = crate::encoding::decode_body(&response.body, response.header("content-type"), fallback);
                // Its `url()`s are relative to the sheet, not the page.
                let text = crate::css::absolutize_urls(&text, &response.url);
                self.doc().set_stylesheet(id, text);
            }
        }
    }

    /// Runs the page's scripts in the order a browser would, then lets the
    /// page settle.
    ///
    /// The document is parsed completely before any script runs, so a
    /// parser-blocking script sees elements that come after it, and
    /// `document.write` appends rather than inserting. Within that, the order
    /// is the browser's: parser-blocking scripts (inline, and external without
    /// `async` or `defer`) in document order, then `DOMContentLoaded` after
    /// the `defer` scripts, then `async` scripts, then the task queue, then
    /// `load`. A script that throws is reported and does not stop the ones
    /// after it.
    fn run_document_scripts(&mut self) {
        let Some(script) = &self.script else { return };

        struct Found {
            id: NodeId,
            label: String,
            module: bool,
            deferred: bool,
            asynchronous: bool,
        }
        let found: Vec<Found> = {
            let doc = self.doc();
            doc.descendants(doc.document_id())
                .filter(|id| doc.is_tag(*id, "script"))
                .map(|id| {
                    let src = doc.attr(id, "src");
                    let module = doc.attr(id, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("module"));
                    Found {
                        id,
                        label: src.map(str::to_string).unwrap_or_else(|| format!("inline-{id}")),
                        module,
                        // Modules, inline ones too, are deferred unless async;
                        // `defer` means nothing to an inline classic script.
                        deferred: module || (src.is_some() && doc.has_attr(id, "defer")),
                        asynchronous: (module || src.is_some()) && doc.has_attr(id, "async"),
                    }
                })
                .collect()
        };

        let mut errors = Vec::new();
        let mut run = |found: &Found| {
            // The document lock is not held here: the script thread locks it.
            match script.evaluate(&format!("__executeScript({})", found.id)) {
                Ok(JsValue::String(message)) if !message.is_empty() => {
                    errors.push(format!("script error in <script> ({}): {message}", found.label));
                }
                Err(e) => errors.push(format!("script error in <script> ({}): {}", found.label, e.message)),
                _ => {}
            }
        };

        let blocking = |f: &&Found| !f.module && !f.deferred && !f.asynchronous;
        for f in found.iter().filter(blocking) {
            run(f);
        }
        let _ = script.run("__documentParsed()");
        for f in found.iter().filter(|f| f.deferred && !f.asynchronous) {
            run(f);
        }
        let _ = script.run("__fireDOMContentLoaded()");
        for f in found.iter().filter(|f| f.asynchronous) {
            run(f);
        }
        script.run_tasks(LOAD_BUDGET_MS, MAX_TASKS);
        let _ = script.run("__fireLoad()");
        trace_resources("scripts loaded");
        script.run_tasks(LOAD_BUDGET_MS, MAX_TASKS);
        trace_resources("load tasks settled");
        // `<img>`s were fetched by their own tasks above. Backgrounds start
        // now, so they are likely there when a screenshot wants them.
        for url in self.image_urls() {
            self.images.request(&url);
        }
        trace_resources("background images queued");

        if let Some(first) = errors.into_iter().next() {
            self.last_error = Some(first);
        }
    }

    /// If a script assigned `location.href` or submitted a form, follow it.
    /// Bounded, because a page that redirects itself in a loop should not hang
    /// the engine.
    fn follow_script_navigation(&mut self) {
        for _ in 0..MAX_SCRIPT_NAVIGATIONS {
            let Some(requested) = self
                .script
                .as_ref()
                .and_then(|script| script.state().take_requested_navigation())
            else {
                return;
            };
            if !self.script_navigation(requested) {
                return;
            }
        }
    }

    /// Acts on one navigation script asked for. Returns whether anything
    /// changed.
    ///
    /// A `GET` that differs from the current URL only in its fragment stays in
    /// the same document, as `location.hash = ...` does in a browser. A `GET`
    /// to exactly the current URL is ignored, so a page that sets
    /// `location.href` to itself does not loop.
    fn script_navigation(&mut self, requested: Navigation) -> bool {
        let target = url::Url::parse(&self.url)
            .and_then(|base| base.join(&requested.url))
            .map(String::from)
            .unwrap_or(requested.url);
        let is_get = requested.method.eq_ignore_ascii_case("GET") && !requested.reload;
        if is_get && target == self.url {
            return false;
        }
        let without_fragment = |u: &str| u.split('#').next().unwrap_or(u).to_string();
        if is_get && target.contains('#') && without_fragment(&target) == without_fragment(&self.url) {
            self.url = target.clone();
            self.doc().set_url(&target);
            self.history.truncate(self.history_index + 1);
            self.history.push(HistoryEntry { url: target, document: self.document_number, state: None });
            self.history_index = self.history.len() - 1;
            self.publish_history_shape();
            return true;
        }
        let mut request = Request {
            method: requested.method.to_ascii_uppercase(),
            url: target,
            headers: Vec::new(),
            body: requested.body,
            kind: ResourceType::Document,
        };
        if let Some(content_type) = requested.content_type {
            request.headers.push(("Content-Type".into(), content_type));
        }
        self.replace_document(request);
        self.run_document_scripts();
        true
    }

    /// Records the document just loaded as the next entry. Loading the
    /// current URL again replaces its entry, as a reload does, and so does
    /// the first navigation the tab's initial blank page, as in Chrome.
    fn push_history(&mut self) {
        self.history.truncate(self.history_index + 1);
        let entry = HistoryEntry { url: self.url.clone(), document: self.document_number, state: None };
        match self.history.last_mut() {
            Some(last) if last.url == self.url || last.document == 0 => *last = entry,
            _ => {
                self.history.push(entry);
                self.history_index = self.history.len() - 1;
            }
        }
        self.publish_history_shape();
    }

    /// Remote-object id for a raw DOM index, matching `backendDOMNodeId`.
    fn backend_id(dom: NodeId) -> usize {
        dom + 1
    }

    fn dom_from_backend(backend: usize) -> Option<NodeId> {
        backend.checked_sub(1)
    }

    /// The node a DOM method names, by `nodeId` or `backendNodeId` (the same
    /// numbers here) or by a remote `objectId`. Without any, the document.
    fn node_param(&self, params: &Value) -> Result<NodeId, CdpError> {
        let numbered = ["nodeId", "backendNodeId"].iter().find_map(|k| params.get(*k).and_then(|v| v.as_u64()));
        let node = if let Some(n) = numbered {
            Self::dom_from_backend(n as usize)
        } else if let Some(object_id) = params.get("objectId").and_then(|v| v.as_str()) {
            let id = self.cdp(&format!("__cdpNodeId({})", js_string(object_id)))?;
            id.as_i64().filter(|n| *n >= 0).map(|n| n as NodeId)
        } else {
            return Ok(self.doc().document_id());
        };
        match node {
            Some(id) if id < self.doc().len() => Ok(id),
            _ => Err(CdpError { code: -32000, message: "No node with given id found".into() }),
        }
    }

    /// A prelude call answering in CDP JSON. A `{ "error": ... }` answer, a
    /// failure of the plumbing rather than of page script, becomes a protocol
    /// error.
    fn cdp(&self, expression: &str) -> Result<Value, CdpError> {
        let Some(script) = &self.script else {
            return Err(not_yet("Runtime", "the JavaScript engine is not available in this process"));
        };
        let answer = script.cdp_expression(expression);
        cdp_answer(answer)
    }

    /// `Runtime.evaluate`.
    fn evaluate(&self, source: &str, by_value: bool, await_promise: bool) -> Result<Value, CdpError> {
        let Some(script) = &self.script else {
            return Err(not_yet("Runtime.evaluate", "the JavaScript engine is not available in this process"));
        };
        cdp_answer(script.cdp_evaluate(source, by_value, await_promise))
    }

    /// After script ran on a client's behalf: let what it set in motion run
    /// (a handler's timers, a request's completion), then act on a navigation
    /// or history move it asked for, outside the runtime.
    fn after_script(&mut self, out: &mut EngineEmit) {
        if let Some(script) = &self.script {
            script.run_tasks(EVALUATE_BUDGET_MS, MAX_TASKS);
        }
        let moved = self.sync_script_url();
        let mut moved = moved.into_iter();
        let (requested, steps) = match &self.script {
            Some(script) => (script.state().take_requested_navigation(), script.state().take_requested_history()),
            None => (None, 0),
        };
        if let Some(requested) = requested {
            if self.script_navigation(requested) {
                self.follow_script_navigation();
                self.push_history();
                self.emit_lifecycle(out);
            }
        } else if steps != 0 {
            match self.go(steps) {
                Some(Traversal::Loaded) => self.emit_lifecycle(out),
                Some(Traversal::SameDocument(url)) => {
                    // What the traversal set in motion: popstate listeners'
                    // timers and requests, a hashchange.
                    if let Some(script) = &self.script {
                        script.run_tasks(EVALUATE_BUDGET_MS, MAX_TASKS);
                    }
                    self.sync_script_url();
                    out("Page.navigatedWithinDocument", json!({ "frameId": self.frame_id, "url": url, "navigationType": "historyApi" }));
                }
                None => {}
            }
        } else {
            for url in moved.by_ref() {
                out(
                    "Page.navigatedWithinDocument",
                    json!({ "frameId": self.frame_id, "url": url, "navigationType": "historyApi" }),
                );
            }
        }
    }

    /// Moves `steps` entries through history, as `history.go(steps)` does;
    /// a move past either end is ignored, as a browser ignores it.
    /// Between entries of one document, made by `pushState` or a fragment,
    /// nothing loads: the URL and `history.state` change and `popstate`
    /// fires, and `hashchange` if the fragment changed. Between documents,
    /// the entry's URL loads again, there being no page cache, with its
    /// state restored.
    fn go(&mut self, steps: i32) -> Option<Traversal> {
        let target = self.history_index as i64 + steps as i64;
        if target < 0 || target >= self.history.len() as i64 {
            return None;
        }
        let from = self.history[self.history_index].document;
        self.history_index = target as usize;
        let entry = self.history[self.history_index].clone();
        let state = entry.state.clone().unwrap_or_else(|| "null".into());
        if entry.document == from && self.script.is_some() {
            let old_url = std::mem::replace(&mut self.url, entry.url.clone());
            self.doc().set_url(&entry.url);
            self.publish_history_shape();
            if let Some(script) = &self.script {
                let _ = script.run(&format!("__traverseHistory({}, {})", js_string(&state), js_string(&old_url)));
            }
            return Some(Traversal::SameDocument(entry.url));
        }
        let history = self.history.clone();
        let index = self.history_index;
        self.replace_document(Request::get(&entry.url));
        // Before the page's scripts, which may read it.
        if let Some(script) = &self.script {
            let _ = script.run(&format!("__setHistoryState({})", js_string(&state)));
        }
        self.run_document_scripts();
        self.history = history;
        self.history_index = index;
        // The entry belongs to the document just loaded now.
        self.history[index].document = self.document_number;
        self.publish_history_shape();
        Some(Traversal::Loaded)
    }

    /// `Network.setCookie`. Without a `url` or a `domain` there is nothing to
    /// scope the cookie to, and Chrome refuses it too.
    fn set_cookie(&self, params: &Value) -> bool {
        let text = |key: &str| params.get(key).and_then(|v| v.as_str());
        let flag = |key: &str| params.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
        let url = text("url").and_then(|u| fetch::Url::parse(u).ok());
        let Some(cookie) = cookies::from_parts(
            url.as_ref(),
            text("name").unwrap_or(""),
            text("value").unwrap_or(""),
            text("domain"),
            text("path"),
            flag("secure"),
            flag("httpOnly"),
            text("sameSite"),
            params.get("expires").and_then(|v| v.as_f64()).filter(|e| *e > 0.0),
        ) else {
            return false;
        };
        if cookie.name.is_empty() {
            return false;
        }
        self.jar().set(cookie, cookies::now())
    }

    /// `DOM.getBoxModel`'s model: the border box from the layout, and the
    /// content, padding and margin boxes from the computed widths around it.
    fn box_model(&self, node: NodeId) -> Option<Value> {
        let doc = self.doc();
        let rect = crate::layout::document_layout(&doc).client_rect(&doc, node)?;
        let styles = crate::style::document_styles(&doc);
        let style = styles.get(node);
        let px = |name: &str| -> f64 {
            style
                .and_then(|s| crate::style::length_px(s.get(name), s.font_size(), 16.0, rect.width))
                .unwrap_or(0.0)
        };
        let border = |side: &str| -> f64 {
            let none = style.is_none_or(|s| matches!(s.get(&format!("border-{side}-style")), "none" | "hidden"));
            if none { 0.0 } else { px(&format!("border-{side}-width")) }
        };
        let side = |f: &dyn Fn(&str) -> f64| [f("top"), f("right"), f("bottom"), f("left")];
        let [bt, br, bb, bl] = side(&border);
        let [pt, pr, pb, pl] = side(&|s| px(&format!("padding-{s}")));
        let [mt, mr, mb, ml] = side(&|s| px(&format!("margin-{s}")));
        let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);
        let padding = (x + bl, y + bt, w - bl - br, h - bt - bb);
        let content = (padding.0 + pl, padding.1 + pt, padding.2 - pl - pr, padding.3 - pt - pb);
        Some(json!({
            "content": quad(content.0, content.1, content.2.max(0.0), content.3.max(0.0)),
            "padding": quad(padding.0, padding.1, padding.2.max(0.0), padding.3.max(0.0)),
            "border": quad(x, y, w, h),
            "margin": quad(x - ml, y - mt, w + ml + mr, h + mt + mb),
            "width": w.round() as i64,
            "height": h.round() as i64,
        }))
    }

    /// `DOM.describeNode`-shaped output for one node.
    fn describe(&self, node: NodeId) -> Value {
        let doc = self.doc();
        let entry = doc.node(node);
        let (node_type, node_name) = match &entry.data {
            NodeData::Document => (9, "#document".to_string()),
            NodeData::Doctype { name } => (10, name.clone()),
            NodeData::Comment => (8, "#comment".to_string()),
            NodeData::Text => (3, "#text".to_string()),
            NodeData::Fragment => (11, "#document-fragment".to_string()),
            NodeData::Element { .. } => (1, doc.tag(node).unwrap_or("").to_ascii_uppercase()),
        };
        let mut value = json!({
            "nodeId": Self::backend_id(node),
            "backendNodeId": Self::backend_id(node),
            "nodeType": node_type,
            "nodeName": node_name,
            "childNodeCount": doc.children(node).len(),
        });
        if node_type == 1 {
            let attrs: Vec<Value> = doc
                .attrs(node)
                .iter()
                .flat_map(|a| [json!(a.local_name()), json!(a.value)])
                .collect();
            value["attributes"] = json!(attrs);
            value["localName"] = json!(doc.tag(node).unwrap_or(""));
        }
        if node_type == 3 || node_type == 8 {
            value["nodeValue"] = json!(entry.text);
        }
        value
    }
}

/// Reported to pages as the user agent. Kept beside the version string in
/// `browser-core` so the two do not drift.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// The CDP surface, before secrets are scrubbed from what it says. The
/// `Engine` implementation below is the only way out, and it scrubs.
impl HtmlEngine {
    fn info_raw(&self) -> TargetInfo {
        let doc = self.doc();
        let mut info = TargetInfo::new(self.target_id.clone());
        info.title = doc.title().unwrap_or_default();
        info.url = doc.url().to_string();
        if info.url.is_empty() {
            info.url = self.url.clone();
        }
        info
    }

    fn navigate_raw(&mut self, url: &str, out: &mut EngineEmit) -> Result<(), CdpError> {
        self.load(url);
        self.emit_lifecycle(out);
        Ok(())
    }

    fn dispatch_raw(&mut self, method: &str, params: &Value, out: &mut EngineEmit) -> Result<Value, CdpError> {
        match method {
            // ---- Page ----------------------------------------------------
            "Page.enable" | "Page.disable" | "Page.bringToFront" => Ok(json!({})),
            "Page.getFrameTree" => Ok(json!({ "frameTree": { "frame": self.frame_json() } })),

            "Page.navigate" => {
                let url =
                    params.get("url").and_then(|v| v.as_str()).unwrap_or("about:blank").to_string();
                self.load(&url);
                self.emit_lifecycle(out);
                Ok(json!({
                    "frameId": self.frame_id,
                    "loaderId": self.loader_id,
                    "type": "Navigation",
                }))
            }

            "Page.reload" => {
                let url = self.url.clone();
                self.load(&url);
                self.emit_lifecycle(out);
                Ok(json!({}))
            }

            "Page.getNavigationHistory" => {
                let title = self.doc().title().unwrap_or_default();
                let entries: Vec<Value> = self
                    .history
                    .iter()
                    .enumerate()
                    .map(|(i, entry)| {
                        json!({
                            "id": i,
                            "url": entry.url,
                            "userTypedURL": entry.url,
                            "title": if i == self.history_index { title.clone() } else { String::new() },
                            "transitionType": "typed",
                        })
                    })
                    .collect();
                Ok(json!({ "currentIndex": self.history_index, "entries": entries }))
            }

            // The viewport is where the page is scrolled to; the content size
            // is how far the layout reaches.
            "Page.getLayoutMetrics" => {
                let (w, h) = self.viewport;
                let (cw, ch, sx, sy) = {
                    let doc = self.doc();
                    let (cw, ch) = crate::layout::document_layout(&doc).content_size(self.viewport);
                    let (sx, sy) = doc.scroll_offset(doc.document_id());
                    (cw, ch, sx, sy)
                };
                let layout_viewport = json!({ "pageX": sx, "pageY": sy, "clientWidth": w, "clientHeight": h });
                let visual_viewport = json!({
                    "offsetX": 0, "offsetY": 0, "pageX": sx, "pageY": sy,
                    "clientWidth": w, "clientHeight": h, "scale": 1, "zoom": 1,
                });
                let content = json!({ "x": 0, "y": 0, "width": cw.ceil(), "height": ch.ceil() });
                Ok(json!({
                    "layoutViewport": layout_viewport,
                    "visualViewport": visual_viewport,
                    "contentSize": content,
                    "cssLayoutViewport": layout_viewport,
                    "cssVisualViewport": visual_viewport,
                    "cssContentSize": content,
                }))
            }

            "Page.setLifecycleEventsEnabled" | "Page.setBypassCSP" => Ok(json!({})),
            "Page.addScriptToEvaluateOnNewDocument" => Ok(json!({ "identifier": "1" })),
            "Page.createIsolatedWorld" => Ok(json!({ "executionContextId": self.context_id })),

            "Page.captureScreenshot" => self.screenshot(params),
            "Page.startScreencast" => {
                self.screencast = Some(Screencast::start(params).map_err(|e| CdpError::internal(format!("Page.startScreencast: {e}")))?);
                out("Page.screencastVisibilityChanged", json!({ "visible": true }));
                Ok(json!({}))
            }
            "Page.stopScreencast" => {
                self.screencast = None;
                Ok(json!({}))
            }
            "Page.screencastFrameAck" => {
                if let (Some(cast), Some(session)) = (&mut self.screencast, params.get("sessionId").and_then(Value::as_u64)) {
                    cast.ack(session);
                }
                Ok(json!({}))
            }

            // ---- Runtime -------------------------------------------------
            "Runtime.enable" => {
                self.runtime_enabled = true;
                self.emit_context(out);
                Ok(json!({}))
            }
            "Runtime.disable" => {
                self.runtime_enabled = false;
                self.page_events.clear();
                Ok(json!({}))
            }
            "Runtime.discardConsoleEntries" => {
                self.page_events.clear();
                Ok(json!({}))
            }
            "Runtime.runIfWaitingForDebugger" => Ok(json!({})),
            "Runtime.releaseObject" => {
                if let Some(id) = params.get("objectId").and_then(|v| v.as_str()) {
                    self.cdp(&format!("(__cdpRelease({}), '{{}}')", js_string(id)))?;
                }
                Ok(json!({}))
            }
            // Groups are not tracked; a document's handles go with its context.
            "Runtime.releaseObjectGroup" => Ok(json!({})),

            // Real JavaScript, from a real engine. `back` in agent-browser is
            // `history.back()` evaluated this way, which `after_script` acts on.
            "Runtime.evaluate" => {
                let source = params.get("expression").and_then(|v| v.as_str()).unwrap_or("");
                let flag = |k: &str| params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
                let result = self.evaluate(source, flag("returnByValue"), flag("awaitPromise"))?;
                self.after_script(out);
                Ok(result)
            }

            "Runtime.callFunctionOn" => {
                let flag = |k: &str| params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
                let object_id = match params.get("objectId").and_then(|v| v.as_str()) {
                    Some(id) => js_string(id),
                    None => "null".to_string(),
                };
                let declaration = params.get("functionDeclaration").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or_else(|| json!([])).to_string();
                let result = self.cdp(&format!(
                    "__cdpCall({object_id}, {}, {}, {}, {})",
                    js_string(declaration),
                    js_string(&arguments),
                    flag("returnByValue"),
                    flag("awaitPromise"),
                ))?;
                self.after_script(out);
                Ok(result)
            }

            "Runtime.getIsolateId" => Ok(json!({ "id": "m1" })),

            "Runtime.consoleAPICalled" => Ok(json!({})),

            // ---- DOM -----------------------------------------------------
            "DOM.enable" | "DOM.disable" => Ok(json!({})),
            "DOM.getDocument" => {
                let root = self.doc().document_id();
                Ok(json!({ "root": self.describe(root) }))
            }

            "DOM.querySelector" => {
                let doc = self.doc();
                let root = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| doc.document_id());
                let selector =
                    params.get("selector").and_then(|v| v.as_str()).unwrap_or("").to_string();
                match select::query(&doc, root, &selector) {
                    Some(node) => Ok(json!({ "nodeId": Self::backend_id(node) })),
                    None => Ok(json!({ "nodeId": 0 })),
                }
            }

            "DOM.querySelectorAll" => {
                let doc = self.doc();
                let root = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| doc.document_id());
                let selector =
                    params.get("selector").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let ids: Vec<usize> = select::query_all(&doc, root, &selector)
                    .into_iter()
                    .map(Self::backend_id)
                    .collect();
                Ok(json!({ "nodeIds": ids }))
            }

            "DOM.describeNode" => {
                let node = self.node_param(params)?;
                Ok(json!({ "node": self.describe(node) }))
            }

            "DOM.requestNode" => {
                let node = self.node_param(params)?;
                Ok(json!({ "nodeId": Self::backend_id(node) }))
            }

            "DOM.resolveNode" => {
                let node = self.node_param(params)?;
                Ok(json!({ "object": self.cdp(&format!("__cdpNode({node})"))? }))
            }

            // Boxes from the layout. agent-browser clicks a snapshot ref at
            // the centre of its content quad.
            "DOM.getBoxModel" => {
                let node = self.node_param(params)?;
                let model = self.box_model(node).ok_or_else(|| CdpError {
                    code: -32000,
                    message: "Could not compute box model.".into(),
                })?;
                Ok(json!({ "model": model }))
            }
            "DOM.getContentQuads" => {
                let node = self.node_param(params)?;
                let doc = self.doc();
                let quads: Vec<Value> = crate::layout::document_layout(&doc)
                    .client_rect(&doc, node)
                    .filter(|r| r.width > 0.0 && r.height > 0.0)
                    .map(|r| vec![json!(quad(r.x, r.y, r.width, r.height))])
                    .unwrap_or_default();
                Ok(json!({ "quads": quads }))
            }
            // Through the element's own method, so `scroll` events fire.
            "DOM.scrollIntoViewIfNeeded" => {
                let node = self.node_param(params)?;
                let object = self.cdp(&format!("__cdpNode({node})"))?;
                let id = object.get("objectId").and_then(|v| v.as_str()).unwrap_or("");
                self.cdp(&format!(
                    "__cdpCall({}, 'function () {{ this.scrollIntoViewIfNeeded(true); }}', '[]', false, false)",
                    js_string(id)
                ))?;
                self.after_script(out);
                Ok(json!({}))
            }

            "DOM.focus" => {
                let node = self.node_param(params)?;
                let object = self.cdp(&format!("__cdpNode({node})"))?;
                let id = object.get("objectId").and_then(|v| v.as_str()).unwrap_or("");
                self.cdp(&format!("__cdpCall({}, 'function () {{ this.focus(); }}', '[]', false, false)", js_string(id)))?;
                self.after_script(out);
                Ok(json!({}))
            }

            "DOM.getOuterHTML" => {
                let doc = self.doc();
                let node = params
                    .get("nodeId")
                    .and_then(|v| v.as_u64())
                    .and_then(|n| Self::dom_from_backend(n as usize))
                    .unwrap_or_else(|| doc.document_id());
                Ok(json!({ "outerHTML": serialize(&doc, node) }))
            }

            // ---- Accessibility -------------------------------------------
            "Accessibility.enable" => {
                out("Accessibility.loadComplete", json!({}));
                Ok(json!({}))
            }
            "Accessibility.disable" => Ok(json!({})),

            "Accessibility.getFullAXTree" | "Accessibility.getPartialAXTree" => {
                Ok(crate::ax::to_cdp(&crate::ax::build(&self.doc())))
            }

            // ---- Network -------------------------------------------------
            // The cookie jar is real and shared by every target. Requests are
            // reported from `Network.enable` on; see `crate::network`.
            "Network.enable" => {
                let size = |name: &str| params.get(name).and_then(|v| v.as_u64()).map(|n| n as usize);
                self.client.log().enable(size("maxTotalBufferSize"), size("maxResourceBufferSize"));
                Ok(json!({}))
            }
            "Network.disable" => {
                self.client.log().disable();
                Ok(json!({}))
            }
            "Network.getResponseBody" => {
                let id = params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
                let Some(body) = self.client.log().body(id) else {
                    return Err(CdpError::internal("No resource with given identifier found"));
                };
                Ok(match String::from_utf8(body) {
                    Ok(text) => json!({ "body": text, "base64Encoded": false }),
                    // The string scrub cannot see into base64, so the bytes
                    // are scrubbed before encoding.
                    Err(e) => {
                        use base64::Engine as _;
                        let bytes = self.secrets.scrub_bytes(e.as_bytes());
                        json!({ "body": base64::engine::general_purpose::STANDARD.encode(bytes), "base64Encoded": true })
                    }
                })
            }

            // With `urls`, the cookies those URLs would be sent; without, the
            // current page's. That is Chrome's reading of the parameter.
            "Network.getCookies" => {
                let urls: Vec<String> = match params.get("urls").and_then(|v| v.as_array()) {
                    Some(list) => list.iter().filter_map(|u| u.as_str().map(str::to_string)).collect(),
                    None => vec![self.url.clone()],
                };
                let now = cookies::now();
                let jar = self.jar();
                let mut seen: Vec<&Cookie> = Vec::new();
                for url in urls.iter().filter_map(|u| fetch::Url::parse(u).ok()) {
                    for cookie in jar.matching(&url, cookies::Source::Http, now) {
                        if !seen.iter().any(|c| std::ptr::eq(*c, cookie)) {
                            seen.push(cookie);
                        }
                    }
                }
                Ok(json!({ "cookies": seen.into_iter().map(cookie_json).collect::<Vec<_>>() }))
            }
            "Network.getAllCookies" | "Storage.getCookies" => {
                let jar = self.jar();
                let all: Vec<Value> = jar.all(cookies::now()).into_iter().map(cookie_json).collect();
                Ok(json!({ "cookies": all }))
            }
            "Network.setCookie" => Ok(json!({ "success": self.set_cookie(params) })),
            "Network.setCookies" | "Storage.setCookies" => {
                if let Some(list) = params.get("cookies").and_then(|v| v.as_array()) {
                    for cookie in list {
                        self.set_cookie(cookie);
                    }
                }
                Ok(json!({}))
            }
            "Network.deleteCookies" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let url = params.get("url").and_then(|v| v.as_str()).and_then(|u| fetch::Url::parse(u).ok());
                let domain = params
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| url.as_ref().map(|u| u.host.clone()));
                let path = params.get("path").and_then(|v| v.as_str());
                self.jar().delete(name, domain.as_deref(), path);
                Ok(json!({}))
            }
            "Network.clearBrowserCookies" | "Storage.clearCookies" => {
                self.jar().clear();
                Ok(json!({}))
            }

            // ---- Input ---------------------------------------------------
            // Keys and text go to the focused element, which needs no layout.
            // `{{secret:NAME}}` becomes the secret here, and only here, and
            // only on a page from the secret's origin.
            "Input.insertText" => {
                let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let text = self
                    .secrets
                    .resolve(text, &self.url)
                    .map_err(|e| CdpError { code: -32000, message: e.to_string() })?;
                self.cdp(&format!("(__insertText({}), '{{}}')", js_string(&text)))?;
                self.after_script(out);
                Ok(json!({}))
            }
            "Input.dispatchKeyEvent" => {
                self.cdp(&format!("(__dispatchKey({}), '{{}}')", js_string(&params.to_string())))?;
                self.after_script(out);
                Ok(json!({}))
            }

            // The pointer lands on what the layout puts at the point.
            "Input.dispatchMouseEvent" => {
                self.cdp(&format!("(__dispatchMouse({}), '{{}}')", js_string(&params.to_string())))?;
                self.after_script(out);
                Ok(json!({}))
            }
            "Input.dispatchTouchEvent" => Err(not_yet(method, "touch input is not implemented")),

            // ---- Emulation -----------------------------------------------
            "Emulation.setDeviceMetricsOverride" => {
                let w = params.get("width").and_then(|v| v.as_f64()).unwrap_or(800.0);
                let h = params.get("height").and_then(|v| v.as_f64()).unwrap_or(600.0);
                self.viewport = (w, h);
                if let Some(script) = &self.script {
                    script.state().set_viewport(w, h);
                }
                self.doc().set_viewport((w, h));
                Ok(json!({}))
            }
            "Emulation.setUserAgentOverride"
            | "Emulation.setEmulatedMedia"
            | "Emulation.clearDeviceMetricsOverride" => Ok(json!({})),

            "Log.enable" | "Log.clear" | "Performance.enable" => Ok(json!({})),
            "WebMCP.enable" | "WebMCP.disable" => Ok(json!({})),

            _ => Ok(json!({})),
        }
    }
}

/// Everything leaving the engine passes through here, and every secret's
/// value in it becomes its placeholder: results, errors, events, and the
/// target's title and URL.
impl Engine for HtmlEngine {
    fn info(&self) -> TargetInfo {
        let mut info = self.info_raw();
        info.title = self.secrets.scrub_str(&info.title);
        info.url = self.secrets.scrub_str(&info.url);
        info
    }

    fn navigate(&mut self, url: &str, out: &mut EngineEmit) -> Result<(), CdpError> {
        let secrets = Arc::clone(&self.secrets);
        let mut scrubbed = |method: &str, mut params: Value| {
            scrub_outgoing(&secrets, method, &mut params);
            out(method, params)
        };
        let result = self.navigate_raw(url, &mut scrubbed).map_err(|e| scrub_error(&self.secrets, e));
        self.emit_page_events(&mut scrubbed);
        self.emit_frame(&mut scrubbed);
        result
    }

    fn attach(&mut self, out: &mut EngineEmit) {
        let secrets = Arc::clone(&self.secrets);
        let mut scrubbed = |method: &str, mut params: Value| {
            scrub_outgoing(&secrets, method, &mut params);
            out(method, params)
        };
        self.emit_context(&mut scrubbed);
    }

    /// The `Fetch` domain, which answers requests paused in the middle of
    /// a command (intercept.rs). `Fetch.requestPaused` carries a request's
    /// body, a login's password included, so it is scrubbed like every other
    /// event.
    fn concurrent(&mut self, emit: LiveEmit) -> Option<Arc<dyn Concurrent>> {
        let secrets = Arc::clone(&self.secrets);
        let interceptor = Arc::clone(self.client.interceptor());
        interceptor.set_emit(Arc::new(move |method: &str, mut params: Value| {
            scrub_outgoing(&secrets, method, &mut params);
            emit(method, params)
        }));
        Some(interceptor)
    }

    fn close(&mut self) {
        self.client.interceptor().disable();
    }

    fn dispatch(&mut self, method: &str, params: &Value, out: &mut EngineEmit) -> Result<Value, CdpError> {
        let secrets = Arc::clone(&self.secrets);
        let mut scrubbed = |method: &str, mut params: Value| {
            scrub_outgoing(&secrets, method, &mut params);
            out(method, params)
        };
        let result = self.dispatch_raw(method, params, &mut scrubbed);
        self.emit_network(&mut scrubbed);
        self.emit_page_events(&mut scrubbed);
        self.emit_frame(&mut scrubbed);
        trace_resources(method);
        match result {
            Ok(mut value) => {
                scrub_outgoing(&self.secrets, method, &mut value);
                Ok(value)
            }
            Err(e) => Err(scrub_error(&self.secrets, e)),
        }
    }
}

/// Scrubs an outgoing result or event, except a binary payload, which was
/// scrubbed before it was encoded: screenshots and frames are painted from
/// scrubbed text, and a base64 response body is scrubbed as bytes. The
/// string scrub run over base64 could match a short secret by chance, a
/// username like `kim` about once in a 200 KB image, and corrupt it.
fn scrub_outgoing(secrets: &Secrets, method: &str, value: &mut Value) {
    let binary = match method {
        "Page.captureScreenshot" | "Page.screencastFrame" => Some("data"),
        "Network.getResponseBody" if value.get("base64Encoded") == Some(&Value::Bool(true)) => Some("body"),
        _ => None,
    };
    let kept = binary.and_then(|field| value.as_object_mut()?.remove(field).map(|v| (field, v)));
    secrets.scrub(value);
    if let Some((field, payload)) = kept {
        value[field] = payload;
    }
}

fn scrub_error(secrets: &Secrets, error: CdpError) -> CdpError {
    CdpError { code: error.code, message: secrets.scrub_str(&error.message) }
}


/// A cookie in `Network.Cookie` shape.
fn cookie_json(cookie: &Cookie) -> Value {
    let mut value = json!({
        "name": cookie.name,
        "value": cookie.value,
        // Chrome marks a domain cookie with a leading dot and a host-only one
        // without.
        "domain": if cookie.host_only { cookie.domain.clone() } else { format!(".{}", cookie.domain) },
        "path": cookie.path,
        "expires": cookie.expires.unwrap_or(-1.0),
        "size": cookie.name.len() + cookie.value.len(),
        "httpOnly": cookie.http_only,
        "secure": cookie.secure,
        "session": cookie.expires.is_none(),
        "priority": "Medium",
        "sameParty": false,
        "sourceScheme": if cookie.secure { "Secure" } else { "NonSecure" },
    });
    if let Some(same_site) = &cookie.same_site {
        value["sameSite"] = json!(same_site);
    }
    value
}

/// A CDP quad: the four corners, clockwise from the top left.
fn quad(x: f64, y: f64, width: f64, height: f64) -> Vec<f64> {
    vec![x, y, x + width, y, x + width, y + height, x, y + height]
}

/// A JavaScript string literal for `text`. JSON's string syntax is a subset
/// of JavaScript's.
fn js_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// A prelude answer, or the protocol error it carries.
fn cdp_answer(answer: Value) -> Result<Value, CdpError> {
    match answer.get("error").and_then(|e| e.as_str()) {
        Some(message) => Err(CdpError { code: -32000, message: message.to_string() }),
        None => Ok(answer),
    }
}

fn not_yet(method: &str, why: &str) -> CdpError {
    CdpError { code: -32000, message: format!("{method} is not implemented yet: {why}") }
}

/// Linear memory cannot shrink; this is its high-water mark, not live heap.
fn trace_resources(stage: &str) {
    #[cfg(target_arch = "wasm32")]
    if std::env::var_os("WHB_TRACE_RESOURCES").is_some() {
        let mib = std::arch::wasm32::memory_size::<0>() as f64 / 16.0;
        eprintln!("[resources] {stage}: linear memory {mib:.1} MiB");
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = stage;
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// The DOM interface a node presents as. Shared with the JavaScript wrappers'
/// `Symbol.toStringTag`, so `DOM.resolveNode` and `Runtime.evaluate` agree.
pub(crate) fn class_for(doc: &Document, node: NodeId) -> &'static str {
    match &doc.node(node).data {
        NodeData::Document => return "HTMLDocument",
        NodeData::Doctype { .. } => return "DocumentType",
        NodeData::Text => return "Text",
        NodeData::Comment => return "Comment",
        NodeData::Fragment => return "DocumentFragment",
        NodeData::Element { .. } => {}
    }
    if doc.namespace(node) == "http://www.w3.org/2000/svg" {
        return if doc.is_tag(node, "svg") { "SVGSVGElement" } else { "SVGElement" };
    }
    match doc.tag(node) {
        Some("html") => "HTMLHtmlElement",
        Some("body") => "HTMLBodyElement",
        Some("a") => "HTMLAnchorElement",
        Some("button") => "HTMLButtonElement",
        Some("input") => "HTMLInputElement",
        Some("form") => "HTMLFormElement",
        Some("label") => "HTMLLabelElement",
        Some("p") => "HTMLParagraphElement",
        Some("h1") | Some("h2") | Some("h3") | Some("h4") | Some("h5") | Some("h6") => {
            "HTMLHeadingElement"
        }
        Some("img") => "HTMLImageElement",
        Some("select") => "HTMLSelectElement",
        Some("textarea") => "HTMLTextAreaElement",
        Some("script") => "HTMLScriptElement",
        Some("div") => "HTMLDivElement",
        Some("span") => "HTMLSpanElement",
        Some("head") => "HTMLHeadElement",
        Some("title") => "HTMLTitleElement",
        Some("meta") => "HTMLMetaElement",
        Some("link") => "HTMLLinkElement",
        Some("style") => "HTMLStyleElement",
        Some("ul") => "HTMLUListElement",
        Some("ol") => "HTMLOListElement",
        Some("li") => "HTMLLIElement",
        Some("table") => "HTMLTableElement",
        Some("tr") => "HTMLTableRowElement",
        Some("td") | Some("th") => "HTMLTableCellElement",
        Some("thead") | Some("tbody") | Some("tfoot") => "HTMLTableSectionElement",
        Some("option") => "HTMLOptionElement",
        Some("iframe") => "HTMLIFrameElement",
        Some("canvas") => "HTMLCanvasElement",
        Some("template") => "HTMLTemplateElement",
        Some("br") => "HTMLBRElement",
        Some("hr") => "HTMLHRElement",
        Some("pre") => "HTMLPreElement",
        Some("video") => "HTMLVideoElement",
        Some("audio") => "HTMLAudioElement",
        Some("dialog") => "HTMLDialogElement",
        _ => "HTMLElement",
    }
}

/// Minimal HTML serialization, enough for `outerHTML` and for tests.
pub fn serialize(doc: &Document, node: NodeId) -> String {
    let mut out = String::new();
    write_node(doc, node, &mut out);
    out
}

fn write_node(doc: &Document, node: NodeId, out: &mut String) {
    match &doc.node(node).data {
        NodeData::Document | NodeData::Fragment => {
            for child in doc.children(node).to_vec() {
                write_node(doc, child, out);
            }
        }
        NodeData::Doctype { name } => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            out.push('>');
        }
        NodeData::Comment => {
            out.push_str("<!--");
            out.push_str(&doc.node(node).text);
            out.push_str("-->");
        }
        NodeData::Text => out.push_str(&escape_html(&doc.node(node).text)),
        NodeData::Element { .. } => {
            let tag = doc.tag(node).unwrap_or("");
            out.push('<');
            out.push_str(tag);
            for attr in doc.attrs(node) {
                out.push(' ');
                out.push_str(attr.local_name());
                out.push_str("=\"");
                out.push_str(&escape_html(&attr.value));
                out.push('"');
            }
            out.push('>');
            let void = matches!(
                tag,
                "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link"
                    | "meta" | "param" | "source" | "track" | "wbr"
            );
            if void {
                return;
            }
            for child in doc.children(node).to_vec() {
                write_node(doc, child, out);
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
    }
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// One entry of a tab's session history.
#[derive(Debug, Clone)]
struct HistoryEntry {
    url: String,
    /// The document it belongs to, by `HtmlEngine::document_number`: entries
    /// `pushState` or a fragment made share their document's.
    document: u64,
    /// `history.state` as JSON, when it is not null.
    state: Option<String>,
}

/// How [`HtmlEngine::go`] got there.
enum Traversal {
    /// A new document was loaded.
    Loaded,
    /// Within the document, to this URL.
    SameDocument(String),
}

/// Console messages and exceptions held for a client, at most; the oldest
/// go first.
const MAX_PAGE_EVENTS: usize = 1000;

/// The largest screenshot painted: 64 megapixels, a full-page shot of a long
/// page at twice the scale. Beyond that the request is refused, not cropped.
const MAX_SCREENSHOT_PIXELS: f64 = 64.0 * 1024.0 * 1024.0;

/// Whether a space-separated attribute such as `rel` holds `token`.
fn has_token(value: Option<&str>, token: &str) -> bool {
    value.is_some_and(|v| v.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case(token)))
}

/// Whether a `<script>` is one the engine runs: a classic script not marked
/// `nomodule`, or a module. The prelude's `scriptKind` decides for real;
/// this only has to agree well enough not to preload what will not run.
fn runs_script_type(kind: Option<&str>, nomodule: bool) -> bool {
    let Some(kind) = kind.map(|k| k.trim().to_ascii_lowercase()) else { return !nomodule };
    if kind == "module" {
        return true;
    }
    let classic = kind.is_empty() || kind.contains("javascript") || kind.contains("ecmascript") || kind.contains("jscript");
    classic && !nomodule
}

/// The page shown when a navigation fails. A browser shows something; so does
/// this, because a client reading an empty document learns nothing.
fn error_page(url: &str, message: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title></head>\
         <body><h1>{title}</h1><p><code>{url}</code></p><pre>{message}</pre></body></html>",
        title = escape_html("Navigation failed"),
        url = escape_html(url),
        message = escape_html(message)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_core::Engine;

    fn engine_with(html: &str) -> HtmlEngine {
        let mut engine = HtmlEngine::new("target");
        {
            let mut doc = engine.doc();
            *doc = parse_html(html);
            doc.set_url("http://example.test/page");
        }
        engine.url = "http://example.test/page".to_string();
        engine
    }

    fn evaluate(engine: &mut HtmlEngine, expression: &str) -> Value {
        engine.evaluate(expression, false, false).expect(expression)
    }

    #[test]
    fn evaluates_real_page_javascript() {
        let mut engine = engine_with("<title>Real</title><body><h1>Heading</h1></body>");
        assert_eq!(evaluate(&mut engine, "1 + 1")["result"]["value"], 2.0);
        assert_eq!(evaluate(&mut engine, "location.href")["result"]["value"], "http://example.test/page");
        assert_eq!(evaluate(&mut engine, "document.title")["result"]["value"], "Real");
        assert_eq!(evaluate(&mut engine, "document.querySelector('h1').textContent")["result"]["value"], "Heading");
        assert_eq!(evaluate(&mut engine, "window.innerWidth")["result"]["value"], 800.0);
    }

    fn dispatch(engine: &mut HtmlEngine, method: &str, params: Value) -> Value {
        let mut sink = |_: &str, _: Value| {};
        engine.dispatch(method, &params, &mut sink).expect(method)
    }

    fn cookie_names(value: &Value) -> Vec<String> {
        let mut names: Vec<String> = value["cookies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn server_cookies_come_back_on_the_next_navigation_and_through_cdp() {
        let (base, requests) = crate::test_server::serve_script(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
             Set-Cookie: sid=s3cret; Path=/; HttpOnly\r\nSet-Cookie: theme=dark; Path=/\r\n\
             Content-Length: 21\r\n\r\n<title>one</title>one"
                .to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 21\r\n\r\n<title>two</title>two"
                .to_string(),
        ]);
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/one"));
        engine.load(&format!("{base}/two"));
        let _ = requests.recv().unwrap();
        let second = requests.recv().unwrap();
        assert!(second.contains("Cookie: sid=s3cret; theme=dark\r\n"), "{second}");

        let cookies = dispatch(&mut engine, "Network.getCookies", json!({}));
        assert_eq!(cookie_names(&cookies), ["sid", "theme"]);
        let sid = &cookies["cookies"].as_array().unwrap().iter().find(|c| c["name"] == "sid").unwrap();
        assert_eq!(sid["httpOnly"], true);
        assert_eq!(sid["session"], true);
        assert_eq!(sid["domain"], "127.0.0.1");

        // Page script sees only what is not HttpOnly.
        assert_eq!(evaluate(&mut engine, "document.cookie")["result"]["value"], "theme=dark");
    }

    #[test]
    fn the_parser_found_scripts_and_stylesheets_download_together() {
        let base = crate::test_server::serve_together(
            vec![
                ("/", "text/html", "<link rel=stylesheet href=s.css><script src=a.js></script><script src=b.js></script>".into()),
                ("/s.css", "text/css", "p { color: red }".into()),
                ("/a.js", "text/javascript", "document.title = 'a';".into()),
                ("/b.js", "text/javascript", "document.title += 'b';".into()),
            ],
            vec!["/s.css", "/a.js", "/b.js"],
        );
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        assert_eq!(engine.last_error, None);
        assert_eq!(evaluate(&mut engine, "document.title")["result"]["value"], "ab");
        assert_eq!(evaluate(&mut engine, "document.styleSheets.length")["result"]["value"], 1);
    }

    #[test]
    fn what_script_requests_downloads_together_and_runs_in_order() {
        let base = crate::test_server::serve_together(
            vec![
                (
                    "/",
                    "text/html",
                    "<script>
                       window.log = [];
                       for (const name of ['c.js', 'd.js']) {
                         const s = document.createElement('script');
                         s.src = name;
                         document.head.append(s);
                       }
                       const l = document.createElement('link');
                       l.rel = 'stylesheet';
                       l.href = 't.css';
                       l.onload = () => log.push('css');
                       document.head.append(l);
                       fetch('e.json').then((r) => r.json()).then((j) => log.push(j.name));
                       const x = new XMLHttpRequest();
                       x.open('GET', 'f.txt');
                       x.onload = () => log.push(x.responseText);
                       x.send();
                     </script>"
                        .into(),
                ),
                ("/c.js", "text/javascript", "log.push('c');".into()),
                ("/d.js", "text/javascript", "log.push('d');".into()),
                ("/t.css", "text/css", "p { color: red }".into()),
                ("/e.json", "application/json", r#"{"name":"e"}"#.into()),
                ("/f.txt", "text/plain", "f".into()),
            ],
            vec!["/c.js", "/d.js", "/t.css", "/e.json", "/f.txt"],
        );
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        assert_eq!(engine.last_error, None);
        // Each completes in the task it was queued in, so the order is the
        // order they were asked for, whatever order they arrived in.
        assert_eq!(evaluate(&mut engine, "log.join()")["result"]["value"], "c,d,css,e,f");
    }

    #[test]
    fn an_aborted_fetch_is_dropped() {
        let base = crate::test_server::serve_together(
            vec![
                (
                    "/",
                    "text/html",
                    "<script>
                       window.log = [];
                       const c = new AbortController();
                       fetch('slow.txt', { signal: c.signal }).then(() => log.push('done'), (e) => log.push(e.name));
                       c.abort();
                     </script>"
                        .into(),
                ),
                ("/slow.txt", "text/plain", "x".into()),
            ],
            vec!["/slow.txt"],
        );
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        assert_eq!(evaluate(&mut engine, "log.join()")["result"]["value"], "AbortError");
    }

    #[test]
    fn script_cookies_land_in_the_shared_jar() {
        let mut engine = engine_with("<body>x</body>");
        evaluate(&mut engine, "document.cookie = 'lang=ko; path=/'");
        let all = dispatch(&mut engine, "Network.getAllCookies", json!({}));
        assert_eq!(cookie_names(&all), ["lang"]);
        assert_eq!(all["cookies"][0]["domain"], "example.test");
    }

    #[test]
    fn cdp_can_set_delete_and_clear_cookies() {
        let mut engine = engine_with("<body>x</body>");
        let ok = dispatch(
            &mut engine,
            "Network.setCookie",
            json!({ "name": "a", "value": "1", "url": "http://example.test/" }),
        );
        assert_eq!(ok["success"], true);
        dispatch(
            &mut engine,
            "Network.setCookies",
            json!({ "cookies": [{ "name": "b", "value": "2", "domain": "example.test", "path": "/" }] }),
        );
        assert_eq!(evaluate(&mut engine, "document.cookie")["result"]["value"], "a=1; b=2");

        dispatch(&mut engine, "Network.deleteCookies", json!({ "name": "a", "url": "http://example.test/" }));
        assert_eq!(cookie_names(&dispatch(&mut engine, "Network.getAllCookies", json!({}))), ["b"]);

        dispatch(&mut engine, "Network.clearBrowserCookies", json!({}));
        assert!(cookie_names(&dispatch(&mut engine, "Network.getAllCookies", json!({}))).is_empty());

        // Nothing to scope it to.
        let refused = dispatch(&mut engine, "Network.setCookie", json!({ "name": "c", "value": "3" }));
        assert_eq!(refused["success"], false);
    }

    #[test]
    fn targets_from_one_factory_share_cookies() {
        let factory = HtmlEngine::factory();
        let mut first = factory("one");
        let mut second = factory("two");
        let mut sink = |_: &str, _: Value| {};
        first
            .dispatch(
                "Network.setCookie",
                &json!({ "name": "shared", "value": "1", "url": "http://example.test/" }),
                &mut sink,
            )
            .unwrap();
        let all = second.dispatch("Network.getAllCookies", &json!({}), &mut sink).unwrap();
        assert_eq!(cookie_names(&all), ["shared"]);
    }

    #[test]
    fn history_back_and_forward_from_script_move_through_history() {
        let mut engine = HtmlEngine::new("t");
        engine.load("data:text/html,<title>first</title>");
        engine.load("data:text/html,<title>second</title>");
        dispatch(&mut engine, "Runtime.evaluate", json!({ "expression": "history.back()" }));
        assert_eq!(engine.info().title, "first");
        dispatch(&mut engine, "Runtime.evaluate", json!({ "expression": "history.forward()" }));
        assert_eq!(engine.info().title, "second");
        // Past the end is ignored.
        dispatch(&mut engine, "Runtime.evaluate", json!({ "expression": "history.forward()" }));
        assert_eq!(engine.info().title, "second");
        // The tab's initial blank page was replaced by the first load.
        assert_eq!(engine.history.len(), 2, "{:?}", engine.history);
    }

    #[test]
    fn each_document_gets_a_fresh_global_environment() {
        let page = "data:text/html,<div id=\"o\"></div><script>const once = 1; \
                    document.getElementById('o').textContent = 'ran'; leaked = 'yes';</script>";
        let mut engine = HtmlEngine::new("t");
        engine.load(page);
        engine.load("data:text/html,<title>other</title>");
        // The previous page's globals are gone.
        assert_eq!(evaluate(&mut engine, "typeof leaked")["result"]["value"], "undefined");
        // And the same page loads again without redeclaring its `const`.
        engine.load(page);
        assert_eq!(engine.last_error, None);
        let doc = engine.doc();
        let o = doc.find_id(doc.document_id(), "o").unwrap();
        assert_eq!(doc.text_content(o), "ran");
    }

    #[test]
    fn whole_numbers_go_out_without_a_fraction() {
        let mut engine = engine_with("<body>x</body>");
        assert_eq!(evaluate(&mut engine, "6 * 7")["result"]["value"].to_string(), "42");
        assert_eq!(evaluate(&mut engine, "1.5")["result"]["value"].to_string(), "1.5");
    }

    #[test]
    fn exceptions_come_back_as_exception_details() {
        // A page script that throws is normal. Reporting it as a protocol error
        // would make a client abort instead of carrying on.
        let mut engine = engine_with("<body>x</body>");
        let result = evaluate(&mut engine, "throw new Error('page bug')");
        assert_eq!(result["result"]["subtype"], "error");
        let text = result["exceptionDetails"]["text"].as_str().unwrap_or("");
        assert!(text.contains("page bug"), "message was: {text}");
    }

    #[test]
    fn a_script_navigating_updates_the_url() {
        let mut engine = engine_with("<body>x</body>");
        engine.load("data:text/html,<body><script>location.href='#other'</script></body>");
        // A fragment change is a same-document navigation, so the load happens
        // once and does not loop.
        assert!(engine.url.contains("#other") || engine.url.starts_with("data:"), "{}", engine.url);
    }

    #[test]
    fn inline_scripts_run_against_the_document() {
        let mut engine = HtmlEngine::new("t");
        engine.load(
            "data:text/html,<title>T</title><body><div id=\"out\"></div>\
             <script>document.getElementById('out').textContent = 'from script';</script></body>",
        );
        let doc = engine.doc();
        let out = doc.find_id(doc.document_id(), "out").expect("the div exists");
        assert_eq!(doc.text_content(out), "from script");
    }

    #[test]
    fn a_script_that_throws_does_not_stop_the_next_one() {
        let mut engine = HtmlEngine::new("t");
        engine.load(
            "data:text/html,<body><div id=\"out\"></div>\
             <script>throw new Error('first');</script>\
             <script>document.getElementById('out').textContent = 'second ran';</script></body>",
        );
        let doc = engine.doc();
        let out = doc.find_id(doc.document_id(), "out").expect("the div exists");
        assert_eq!(doc.text_content(out), "second ran");
        assert!(engine.last_error.as_deref().unwrap_or("").contains("first"));
    }

    #[test]
    fn external_scripts_are_fetched_and_run_in_order() {
        let script = "document.getElementById('out').textContent += 'external;';";
        let (base, _requests) = crate::test_server::serve_script(vec![
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                PAGE.len(),
                PAGE
            ),
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\n\r\n{script}", script.len()),
        ]);
        const PAGE: &str = "<body><div id=\"out\"></div><script src=\"/app.js\" defer></script>\
                            <script>document.getElementById('out').textContent += 'inline;';</script></body>";
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        let doc = engine.doc();
        let out = doc.find_id(doc.document_id(), "out").expect("the div exists");
        // `defer` runs after the parser-blocking inline script.
        assert_eq!(doc.text_content(out), "inline;external;");
    }

    #[test]
    fn a_script_that_cannot_be_fetched_is_reported() {
        let mut engine = HtmlEngine::new("t");
        engine.load("data:text/html,<body><script src=\"http://127.0.0.1:1/app.js\"></script></body>");
        assert!(engine.last_error.as_deref().unwrap_or("").contains("could not load"), "{:?}", engine.last_error);
    }

    #[test]
    fn the_accessibility_tree_reflects_script_mutations() {
        let mut engine = HtmlEngine::new("t");
        engine.load(
            "data:text/html,<title>T</title><body><div id=\"root\"></div>\
             <script>\
               const h = document.createElement('h2');\
               h.textContent = 'Added by script';\
               document.getElementById('root').appendChild(h);\
             </script></body>",
        );
        let mut events = Vec::new();
        let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
        let value =
            engine.dispatch("Accessibility.getFullAXTree", &Value::Null, &mut sink).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert!(
            nodes
                .iter()
                .any(|n| n["role"]["value"] == "heading" && n["name"]["value"] == "Added by script"),
            "script-created heading missing from the tree"
        );
    }

    #[test]
    fn unknown_expressions_are_evaluated_not_guessed() {
        // The old rule set returned undefined for anything unrecognised. Now
        // JavaScript answers, so an arbitrary expression has a real value.
        let mut engine = engine_with("<body><p id='p'>hello</p></body>");
        assert_eq!(
            evaluate(&mut engine, "document.querySelector('#p').textContent.length")["result"]["value"],
            5.0
        );
        assert_eq!(evaluate(&mut engine, "['a','b'].join('-')")["result"]["value"], "a-b");
        assert_eq!(
            evaluate(&mut engine, "JSON.stringify({n: [1,2]})")["result"]["value"],
            r#"{"n":[1,2]}"#
        );
    }

    #[test]
    fn an_evaluated_node_comes_back_as_a_handle_the_client_can_call() {
        // agent-browser resolves a CSS selector this way, then focuses and
        // reads through callFunctionOn.
        let mut engine = engine_with("<body><p id='p' class='a b'>hello there</p></body>");
        let object = evaluate(&mut engine, "document.querySelector('#p')");
        assert_eq!(object["result"]["subtype"], "node");
        assert_eq!(object["result"]["className"], "HTMLParagraphElement");
        assert_eq!(object["result"]["description"], "p#p.a.b");
        let id = object["result"]["objectId"].as_str().expect("a handle").to_string();

        let text = dispatch(
            &mut engine,
            "Runtime.callFunctionOn",
            json!({ "objectId": id, "functionDeclaration": "function(suffix) { return this.textContent + suffix; }",
                    "arguments": [{ "value": "!" }], "returnByValue": true }),
        );
        assert_eq!(text["result"]["value"], "hello there!");

        // The same node through DOM.resolveNode is the same handle.
        let backend = { let doc = engine.doc(); doc.find_id(doc.document_id(), "p").unwrap() + 1 };
        let resolved = dispatch(&mut engine, "DOM.resolveNode", json!({ "backendNodeId": backend }));
        assert_eq!(resolved["object"]["objectId"], json!(id));
        let described = dispatch(&mut engine, "DOM.describeNode", json!({ "objectId": id }));
        assert_eq!(described["node"]["backendNodeId"], json!(backend));
    }

    #[test]
    fn values_come_back_by_value_and_promises_are_awaited() {
        let engine = engine_with("<body>x</body>");
        let by_value = engine.evaluate("({ a: [1, 'two'] })", true, false).unwrap();
        assert_eq!(by_value["result"]["value"], json!({ "a": [1, "two"] }));
        let awaited =
            engine.evaluate("new Promise((r) => setTimeout(() => r(7), 2000))", true, true).unwrap();
        assert_eq!(awaited["result"]["value"], json!(7));
        let rejected = engine.evaluate("Promise.reject(new Error('no'))", true, true).unwrap();
        assert!(rejected["exceptionDetails"]["text"].as_str().unwrap().contains("no"), "{rejected}");
        let never = engine.evaluate("new Promise(() => {})", true, true);
        assert!(never.unwrap_err().message.contains("did not settle"));
    }

    #[test]
    fn fill_types_into_a_react_style_controlled_input() {
        // React tracks a controlled input's value through a property it
        // defines on the element, and fires onChange only when the value the
        // element reports differs from the one it tracked. This mimics that
        // tracker, then fills the field the way agent-browser does.
        let mut engine = engine_with(
            "<body><form><input id='user' name='user'></form>\
             <script></script></body>",
        );
        evaluate(
            &mut engine,
            "const input = document.getElementById('user');\
             const native = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');\
             let tracked = '';\
             Object.defineProperty(input, 'value', { configurable: true,\
               get() { return native.get.call(this); },\
               set(v) { tracked = '' + v; native.set.call(this, v); } });\
             globalThis.changes = [];\
             input.addEventListener('input', () => { if (input.value !== tracked) { tracked = input.value; changes.push(tracked); } });",
        );
        let object = evaluate(&mut engine, "document.getElementById('user')");
        let id = object["result"]["objectId"].as_str().unwrap().to_string();
        dispatch(&mut engine, "Runtime.callFunctionOn", json!({ "objectId": id, "functionDeclaration": "function() { this.focus(); }" }));
        dispatch(&mut engine, "Input.insertText", json!({ "text": "kim" }));
        dispatch(&mut engine, "Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": "Backspace", "code": "Backspace", "windowsVirtualKeyCode": 8 }));
        dispatch(&mut engine, "Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": "a", "code": "KeyA", "text": "a" }));
        assert_eq!(evaluate(&mut engine, "changes.join(',')")["result"]["value"], "kim,ki,kia");
        assert_eq!(evaluate(&mut engine, "document.getElementById('user').value")["result"]["value"], "kia");
    }

    #[test]
    fn enter_in_a_field_submits_its_form() {
        let (base, requests) = crate::test_server::serve_script(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 74\r\n\r\n\
             <form action='/login' method='post'><input name='user'><button>go</button></form>"
                .to_string(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 22\r\n\r\n<title>welcome</title>"
                .to_string(),
        ]);
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        evaluate(&mut engine, "document.querySelector('input').focus()");
        dispatch(&mut engine, "Input.insertText", json!({ "text": "kim" }));
        dispatch(&mut engine, "Input.dispatchKeyEvent", json!({ "type": "keyDown", "key": "Enter", "code": "Enter", "text": "\r", "windowsVirtualKeyCode": 13 }));
        assert_eq!(engine.info().title, "welcome");
        let _ = requests.recv().unwrap();
        let login = requests.recv().unwrap();
        assert!(login.starts_with("POST /login HTTP/1.1\r\n"), "{login}");
        assert!(login.ends_with("\r\n\r\nuser=kim"), "{login}");
    }

    #[test]
    fn the_mouse_clicks_what_is_at_the_point() {
        let mut engine = engine_with(
            "<style>body { margin: 0 } button { display: block; width: 100px; height: 40px }\
             #cover { position: fixed; top: 100px; left: 0; width: 200px; height: 50px; z-index: 5 }</style>\
             <body><button id='a'>A</button><button id='b'>B</button><div id='cover'></div>\
             <button id='c' style='margin-top: 30px'>C</button></body>",
        );
        evaluate(
            &mut engine,
            "globalThis.log = []; for (const id of ['a', 'b', 'c', 'cover']) \
               document.getElementById(id).addEventListener('click', () => log.push(id));\
             document.getElementById('b').addEventListener('mousedown', () => log.push('down'));",
        );
        let click = |engine: &mut HtmlEngine, x: f64, y: f64| {
            for kind in ["mousePressed", "mouseReleased"] {
                dispatch(engine, "Input.dispatchMouseEvent", json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 }));
            }
        };
        click(&mut engine, 50.0, 60.0);
        assert_eq!(evaluate(&mut engine, "document.activeElement.id")["result"]["value"], "b");
        // The fixed cover sits over C, and takes its click; pressing
        // something that cannot take focus takes it away.
        click(&mut engine, 50.0, 120.0);
        assert_eq!(evaluate(&mut engine, "log.join(',')")["result"]["value"], "down,b,cover");
        assert_eq!(evaluate(&mut engine, "document.activeElement.tagName")["result"]["value"], "BODY");
        // What the client measures is what it hits.
        let rect = evaluate(&mut engine, "JSON.stringify(document.getElementById('a').getBoundingClientRect())");
        assert_eq!(rect["result"]["value"], r#"{"x":0,"y":0,"width":100,"height":40,"top":0,"right":100,"bottom":40,"left":0}"#);
        assert_eq!(evaluate(&mut engine, "document.elementFromPoint(50, 20).id")["result"]["value"], "a");
    }

    fn engine_with_secrets(html: &str) -> HtmlEngine {
        let mut secrets = Secrets::new();
        secrets.add("pw@http://example.test", "hunter2-s3cret".into()).unwrap();
        secrets.add("user@http://example.test", "kim.minsu".into()).unwrap();
        let mut engine =
            HtmlEngine::with_shared("target", Arc::new(Mutex::new(CookieJar::new())), Arc::new(secrets));
        {
            let mut doc = engine.doc();
            *doc = parse_html(html);
            doc.set_url("http://example.test/page");
        }
        engine.url = "http://example.test/page".to_string();
        engine
    }

    #[test]
    fn secrets_are_typed_by_placeholder_and_never_come_back() {
        let mut engine = engine_with_secrets(
            "<body><input id='u' aria-label='User'><input id='p' type='password' aria-label='Password'></body>",
        );
        for (field, placeholder) in [("u", "{{secret:user}}"), ("p", "{{secret:pw}}")] {
            evaluate(&mut engine, &format!("document.getElementById('{field}').focus()"));
            dispatch(&mut engine, "Input.insertText", json!({ "text": placeholder }));
        }
        // The page has the real values: it can log in.
        let check = evaluate(&mut engine, "document.getElementById('p').value === 'hunter2-s3cret' && document.getElementById('u').value.length");
        assert_eq!(check["result"]["value"], json!(9));
        // A client reading them gets placeholders, in results, snapshots and
        // errors alike. Clients only ever come in through `dispatch`.
        let read = dispatch(
            &mut engine,
            "Runtime.evaluate",
            json!({ "expression": "document.getElementById('p').value + ' / ' + document.getElementById('u').value" }),
        );
        assert_eq!(read["result"]["value"], "{{secret:pw}} / {{secret:user}}");
        let tree = dispatch(&mut engine, "Accessibility.getFullAXTree", json!({})).to_string();
        assert!(tree.contains("{{secret:user}}") && !tree.contains("kim.minsu"), "{tree}");
        let thrown = dispatch(
            &mut engine,
            "Runtime.evaluate",
            json!({ "expression": "throw new Error('pw is ' + document.getElementById('p').value)" }),
        );
        assert!(!thrown.to_string().contains("hunter2"), "{thrown}");
    }

    /// Every event a command or navigation sends, through the scrubbing
    /// exit a client sees.
    fn events_of(engine: &mut HtmlEngine, action: impl FnOnce(&mut HtmlEngine, &mut EngineEmit)) -> Vec<(String, Value)> {
        let mut events = Vec::new();
        let mut sink = |method: &str, params: Value| events.push((method.to_string(), params));
        action(engine, &mut sink);
        events
    }

    fn network_only(events: &[(String, Value)]) -> Vec<(&str, &Value)> {
        events.iter().filter(|(m, _)| m.starts_with("Network.")).map(|(m, p)| (m.as_str(), p)).collect()
    }

    #[test]
    fn network_events_report_a_login_post_with_its_secret_scrubbed() {
        let base = crate::test_server::serve_together(
            vec![
                (
                    "/",
                    "text/html",
                    "<input id=p><script src=app.js></script>".into(),
                ),
                (
                    "/app.js",
                    "text/javascript",
                    "window.login = () => fetch('/api/login', { method: 'POST', headers: { 'Content-Type': 'application/json' },
                       body: JSON.stringify({ password: document.getElementById('p').value }) });"
                        .into(),
                ),
                ("/api/login", "application/json", r#"{"ok":true}"#.into()),
            ],
            vec![],
        );
        let mut secrets = Secrets::new();
        secrets.add(&format!("pw@{base}"), r#"hunt"er2"#.into()).unwrap();
        let mut engine = HtmlEngine::with_shared("t", Arc::new(Mutex::new(CookieJar::new())), Arc::new(secrets));

        // Nothing is recorded before Network.enable.
        engine.load(&format!("{base}/"));
        assert!(network_only(&events_of(&mut engine, |e, out| { e.dispatch("Page.enable", &json!({}), out).unwrap(); })).is_empty());

        dispatch(&mut engine, "Network.enable", json!({}));
        let events = events_of(&mut engine, |e, out| e.navigate(&format!("{base}/"), out).unwrap());
        let network = network_only(&events);
        let summary: Vec<String> =
            network.iter().map(|(m, p)| format!("{m} {}", p["type"].as_str().unwrap_or(""))).collect();
        assert_eq!(
            summary,
            [
                "Network.requestWillBeSent Document",
                "Network.responseReceived Document",
                "Network.dataReceived ",
                "Network.loadingFinished ",
                "Network.requestWillBeSent Script",
                "Network.responseReceived Script",
                "Network.dataReceived ",
                "Network.loadingFinished ",
            ]
        );
        // The load's requests come before the page says it navigated.
        let navigated = events.iter().position(|(m, _)| m == "Page.frameNavigated").unwrap();
        let last_network = events.iter().rposition(|(m, _)| m.starts_with("Network.")).unwrap();
        assert!(last_network < navigated);
        let document = network[0].1;
        assert_eq!(document["requestId"], document["loaderId"]);
        assert_eq!(document["frameId"], json!(engine.frame_id));
        assert_eq!(network[1].1["response"]["mimeType"], "text/html");

        evaluate(&mut engine, "document.getElementById('p').focus()");
        dispatch(&mut engine, "Input.insertText", json!({ "text": "{{secret:pw}}" }));
        let events = events_of(&mut engine, |e, out| {
            e.dispatch("Runtime.evaluate", &json!({ "expression": "login()", "awaitPromise": true }), out).unwrap();
        });
        let network = network_only(&events);
        let (_, post) = network.iter().find(|(m, _)| *m == "Network.requestWillBeSent").unwrap();
        assert_eq!(post["type"], "Fetch");
        assert_eq!(post["request"]["method"], "POST");
        // `Headers` lowercases names, as the Fetch standard says.
        assert_eq!(post["request"]["headers"]["content-type"], "application/json");
        assert_eq!(post["request"]["headers"]["Accept"], "*/*");
        // The value went out escaped in JSON; the client sees the placeholder.
        assert_eq!(post["request"]["postData"], r#"{"password":"{{secret:pw}}"}"#);
        assert!(!events.iter().any(|(_, p)| p.to_string().contains("er2")));

        let id = post["requestId"].clone();
        let body = dispatch(&mut engine, "Network.getResponseBody", json!({ "requestId": id }));
        assert_eq!(body, json!({ "body": r#"{"ok":true}"#, "base64Encoded": false }));
        let mut sink = |_: &str, _: Value| {};
        let missing = engine.dispatch("Network.getResponseBody", &json!({ "requestId": "nope" }), &mut sink);
        assert!(missing.is_err());
    }

    /// Attaches a client that answers every `Fetch.requestPaused` from its
    /// own thread, as agent-browser's daemon does: the image is failed, the
    /// data mocked, anything else continued. Returns what it was shown.
    fn route_like_agent_browser(engine: &mut HtmlEngine) -> Arc<Mutex<Vec<Value>>> {
        use base64::Engine as _;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let door: Arc<Mutex<Option<Arc<dyn Concurrent>>>> = Arc::new(Mutex::new(None));
        let (log, answering) = (Arc::clone(&seen), Arc::clone(&door));
        let handler = engine
            .concurrent(Arc::new(move |method: &str, params: Value| {
                assert_eq!(method, "Fetch.requestPaused");
                log.lock().unwrap().push(params.clone());
                let door = Arc::clone(&answering);
                std::thread::spawn(move || {
                    let url = params["request"]["url"].as_str().unwrap_or("");
                    let id = params["requestId"].clone();
                    let (method, reply) = if url.ends_with("/pic.png") {
                        ("Fetch.failRequest", json!({ "requestId": id, "errorReason": "Failed" }))
                    } else if url.ends_with("/data.json") {
                        let body = base64::engine::general_purpose::STANDARD.encode(br#"{"mock":true}"#);
                        ("Fetch.fulfillRequest", json!({ "requestId": id, "responseCode": 200, "responseHeaders": [], "body": body }))
                    } else {
                        ("Fetch.continueRequest", json!({ "requestId": id }))
                    };
                    let handler = door.lock().unwrap().clone().unwrap();
                    handler.handle(method, &reply).unwrap().unwrap();
                });
                true
            }))
            .expect("the html engine intercepts");
        *door.lock().unwrap() = Some(Arc::clone(&handler));
        let patterns = json!({ "patterns": [{ "urlPattern": "*/pic.png" }, { "urlPattern": "*/api/*" }] });
        handler.handle("Fetch.enable", &patterns).unwrap().unwrap();
        seen
    }

    #[test]
    fn routed_requests_are_failed_mocked_or_continued_while_the_page_loads() {
        let base = crate::test_server::serve_together(
            vec![
                (
                    "/",
                    "text/html",
                    "<input id=p>
                     <img src=pic.png onload=\"log.push('img loaded')\" onerror=\"log.push('img error')\">
                     <script>
                       window.log = [];
                       fetch('api/data.json').then(r => r.text().then(t => log.push(r.status + ' ' + r.headers.get('content-type') + ' ' + t)));
                       window.login = () => fetch('api/login', { method: 'POST', body: 'pw=' + document.getElementById('p').value }).then(r => r.text());
                     </script>"
                        .into(),
                ),
                ("/pic.png", "image/png", b"not reached".to_vec()),
                ("/api/data.json", "application/json", br#"{"real":true}"#.to_vec()),
                ("/api/login", "text/plain", b"welcome".to_vec()),
            ],
            vec![],
        );
        let mut secrets = Secrets::new();
        secrets.add(&format!("pw@{base}"), "hunter2".into()).unwrap();
        let mut engine = HtmlEngine::with_shared("t", Arc::new(Mutex::new(CookieJar::new())), Arc::new(secrets));
        let seen = route_like_agent_browser(&mut engine);
        dispatch(&mut engine, "Network.enable", json!({}));

        // The load pauses on its own thread's requests, and goes on when
        // the client answers from another.
        let events = events_of(&mut engine, |e, out| e.navigate(&format!("{base}/"), out).unwrap());
        let log = evaluate(&mut engine, "log.join(' | ')")["result"]["value"].clone();
        assert_eq!(log, r#"200 null {"mock":true} | img error"#);

        let paused = seen.lock().unwrap().clone();
        let kinds: Vec<String> =
            paused.iter().map(|p| format!("{} {}", p["resourceType"].as_str().unwrap(), p["request"]["url"])).collect();
        assert_eq!(kinds.len(), 2, "{kinds:?}");
        assert!(kinds.contains(&format!("Image \"{base}/pic.png\"")), "{kinds:?}");
        // A fetch() is XHR here, as Chrome reports it to the Fetch domain.
        assert!(kinds.contains(&format!("XHR \"{base}/api/data.json\"")), "{kinds:?}");
        assert!(paused.iter().all(|p| p["frameId"] == json!(engine.frame_id)));

        let network = network_only(&events);
        let image = paused.iter().find(|p| p["resourceType"] == "Image").unwrap();
        let failed = network.iter().find(|(m, p)| *m == "Network.loadingFailed" && p["requestId"] == image["networkId"]);
        assert_eq!(failed.unwrap().1["errorText"], "net::ERR_FAILED");

        // A POST shows its body to the client, the password as a placeholder.
        evaluate(&mut engine, "document.getElementById('p').focus()");
        dispatch(&mut engine, "Input.insertText", json!({ "text": "{{secret:pw}}" }));
        let answer = dispatch(&mut engine, "Runtime.evaluate", json!({ "expression": "login()", "awaitPromise": true }));
        assert_eq!(answer["result"]["value"], "welcome");
        let paused = seen.lock().unwrap().clone();
        let login = paused.iter().find(|p| p["request"]["method"] == "POST").expect("the login was paused");
        assert_eq!(login["request"]["postData"], "pw={{secret:pw}}");
        assert!(!login.to_string().contains("hunter2"));

        // Disabled, nothing pauses.
        engine.client.interceptor().handle("Fetch.disable", &json!({})).unwrap().unwrap();
        let before = seen.lock().unwrap().len();
        engine.load(&format!("{base}/"));
        assert_eq!(evaluate(&mut engine, "log.join(' | ')")["result"]["value"], r#"200 application/json {"real":true} | img error"#);
        assert_eq!(seen.lock().unwrap().len(), before);
    }

    #[test]
    fn network_events_follow_redirects_and_report_failures() {
        let (base, _requests) = crate::test_server::serve_script(vec![
            "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 4\r\nConnection: close\r\n\r\nhere".into(),
        ]);
        let mut engine = HtmlEngine::new("t");
        dispatch(&mut engine, "Network.enable", json!({}));
        let events = events_of(&mut engine, |e, out| e.navigate(&format!("{base}/start"), out).unwrap());
        let network = network_only(&events);
        assert_eq!(network[0].0, "Network.requestWillBeSent");
        assert_eq!(network[0].1["request"]["url"], format!("{base}/start"));
        assert_eq!(network[1].0, "Network.requestWillBeSent");
        assert_eq!(network[1].1["request"]["url"], format!("{base}/final"));
        assert_eq!(network[1].1["redirectResponse"]["status"], 302);
        assert_eq!(network[1].1["requestId"], network[0].1["requestId"]);
        assert_eq!(network[2].0, "Network.responseReceived");
        assert_eq!(network[2].1["response"]["url"], format!("{base}/final"));

        // Port 1 refuses connections.
        let events = events_of(&mut engine, |e, out| {
            e.dispatch("Runtime.evaluate", &json!({ "expression": "fetch('http://127.0.0.1:1/').catch(() => 0)", "awaitPromise": true }), out)
                .unwrap();
        });
        let network = network_only(&events);
        assert_eq!(network.last().unwrap().0, "Network.loadingFailed");
        assert_eq!(network.last().unwrap().1["type"], "Fetch");
    }

    #[test]
    fn a_secret_is_not_typed_into_another_origin() {
        let mut engine = engine_with_secrets("<body><input id='p'></body>");
        engine.url = "http://evil.test/".to_string();
        evaluate(&mut engine, "document.getElementById('p').focus()");
        let mut sink = |_: &str, _: Value| {};
        let error = engine.dispatch("Input.insertText", &json!({ "text": "{{secret:pw}}" }), &mut sink).unwrap_err();
        assert!(error.message.contains("is for http://example.test"), "{}", error.message);
        assert!(!error.message.contains("hunter2"));
        assert_eq!(evaluate(&mut engine, "document.getElementById('p').value")["result"]["value"], "");
    }

    #[test]
    fn a_typed_password_stays_out_of_the_tree_and_node_descriptions() {
        // Page script reads it, as it must to log in, and so can a client
        // calling into the page, as in Chrome. The accessibility tree and
        // DOM.describeNode, which clients log and snapshot, never carry it.
        let mut engine = engine_with("<body><input type='password' id='pw' aria-label='Password'></body>");
        evaluate(&mut engine, "document.getElementById('pw').focus()");
        dispatch(&mut engine, "Input.insertText", json!({ "text": "hunter2" }));
        let tree = dispatch(&mut engine, "Accessibility.getFullAXTree", json!({}));
        assert!(!tree.to_string().contains("hunter2"), "{tree}");
        let backend = { let doc = engine.doc(); doc.find_id(doc.document_id(), "pw").unwrap() + 1 };
        let described = dispatch(&mut engine, "DOM.describeNode", json!({ "backendNodeId": backend }));
        assert!(!described.to_string().contains("hunter2"), "{described}");
    }

    #[test]
    fn dom_node_ids_round_trip() {
        let engine = engine_with("<body><p id='p'>x</p></body>");
        let p = { let doc = engine.doc(); doc.find_id(doc.document_id(), "p") }.unwrap();
        assert_eq!(HtmlEngine::dom_from_backend(HtmlEngine::backend_id(p)), Some(p));
    }

    #[test]
    fn describe_reports_node_types_and_attributes() {
        let engine = engine_with("<body><a href='/x' id='l'>t</a></body>");
        let root = engine.doc().document_id();
        let document = engine.describe(root);
        assert_eq!(document["nodeType"], 9);
        assert_eq!(document["nodeName"], "#document");

        let a = { let doc = engine.doc(); doc.find_id(doc.document_id(), "l") }.unwrap();
        let described = engine.describe(a);
        assert_eq!(described["nodeType"], 1);
        assert_eq!(described["nodeName"], "A");
        assert!(described["attributes"].as_array().unwrap().contains(&json!("href")));
    }

    #[test]
    fn serialize_round_trips_a_small_fragment() {
        let engine = engine_with("<body><div id=\"a\"><p>hi</p></div></body>");
        let div = { let doc = engine.doc(); doc.find_id(doc.document_id(), "a") }.unwrap();
        assert_eq!(serialize(&engine.doc(), div), "<div id=\"a\"><p>hi</p></div>");
    }

    #[test]
    fn serialize_does_not_invent_closing_tags_for_void_elements() {
        let engine = engine_with("<body><img src=\"x.png\" alt=\"a\"><br></body>");
        let img = { let doc = engine.doc(); doc.find_tag(doc.document_id(), "img") }.unwrap();
        assert_eq!(serialize(&engine.doc(), img), "<img src=\"x.png\" alt=\"a\">");
    }

    fn screenshot_png(engine: &mut HtmlEngine, params: Value) -> Vec<u8> {
        use base64::Engine as _;
        let shot = dispatch(engine, "Page.captureScreenshot", params);
        base64::engine::general_purpose::STANDARD.decode(shot["data"].as_str().unwrap()).unwrap()
    }

    fn png_size(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        (u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap()))
    }

    #[test]
    fn screenshots_are_the_viewport_or_the_clip_as_png() {
        let mut engine = engine_with("<body style='margin:0'><div style='height:1500px;width:1000px'>tall</div></body>");
        assert_eq!(png_size(&screenshot_png(&mut engine, json!({}))), (800, 600));
        let clip = json!({ "clip": { "x": 0, "y": 100, "width": 200, "height": 50, "scale": 2 }, "captureBeyondViewport": true });
        assert_eq!(png_size(&screenshot_png(&mut engine, clip)), (400, 100));

        let metrics = dispatch(&mut engine, "Page.getLayoutMetrics", json!({}));
        assert_eq!(metrics["cssContentSize"]["height"], 1500.0);
        assert_eq!(metrics["cssContentSize"]["width"], 1000.0);
        assert_eq!(metrics["cssLayoutViewport"]["clientWidth"], 800.0);

        let mut sink = |_: &str, _: Value| {};
        let webp = engine.dispatch("Page.captureScreenshot", &json!({ "format": "webp" }), &mut sink).unwrap_err();
        assert!(webp.message.contains("only png and jpeg"), "{}", webp.message);
        let jpeg = screenshot_png(&mut engine, json!({ "format": "jpeg", "quality": 50 }));
        assert_eq!(&jpeg[..3], b"\xff\xd8\xff");
        assert_eq!(crate::images::dimensions(&jpeg), Some((800, 600)));
        let huge = json!({ "clip": { "x": 0, "y": 0, "width": 100000, "height": 100000, "scale": 1 } });
        assert!(engine.dispatch("Page.captureScreenshot", &huge, &mut sink).is_err());
    }

    #[test]
    fn a_screenshot_shows_secrets_as_placeholders_and_passwords_as_bullets() {
        let page = "<body><p>Welcome, kim.minsu</p><input id=p type=password></body>";
        let shot = |html: &str, password: &str| {
            let mut engine = engine_with_secrets(html);
            evaluate(&mut engine, "document.getElementById('p').focus()");
            dispatch(&mut engine, "Input.insertText", json!({ "text": password }));
            screenshot_png(&mut engine, json!({}))
        };
        // The secret's value and a same-length other one paint alike: the
        // image carries only its length.
        // Compared as booleans: a failing assert_eq would print the PNGs.
        assert!(shot(page, "{{secret:pw}}") == shot(page, "x".repeat("hunter2-s3cret".len()).as_str()));
        // The username is painted as its placeholder, as a snapshot shows it.
        assert!(shot(page, "") == shot("<body><p>Welcome, {{secret:user}}</p><input id=p type=password></body>", ""));
        assert!(shot(page, "") != shot("<body><p>Welcome, someone.else</p><input id=p type=password></body>", ""));
    }

    #[test]
    fn images_load_like_a_browsers_and_show_in_screenshots() {
        let mut png = Vec::new();
        let red = image::RgbaImage::from_pixel(30, 10, image::Rgba([255, 0, 0, 255]));
        red.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let base = crate::test_server::serve_together(
            vec![
                (
                    "/",
                    "text/html",
                    "<body style='margin:0'><img id=i src=red.png style='vertical-align:top'>\
                     <script>window.log = []; const i = document.getElementById('i');\
                     i.onload = () => log.push('img ' + i.naturalWidth);\
                     addEventListener('load', () => log.push('window ' + i.complete));\
                     const late = new Image(); late.onerror = () => log.push('missing'); late.src = 'nope.png';</script></body>"
                        .into(),
                ),
                ("/red.png", "image/png", png),
            ],
            vec![],
        );
        let mut engine = HtmlEngine::new("t");
        dispatch(&mut engine, "Network.enable", json!({}));
        let events = events_of(&mut engine, |e, out| e.navigate(&format!("{base}/"), out).unwrap());
        // The two images' events come in whatever order they arrive, as in
        // Chrome; the window's load comes after both.
        assert_eq!(
            evaluate(&mut engine, "log.slice(0, 2).sort().join() + ' / ' + log[2]")["result"]["value"],
            "img 30,missing / window true"
        );
        assert!(network_only(&events).iter().any(|(m, p)| *m == "Network.requestWillBeSent" && p["type"] == "Image"));
        assert_eq!(engine.last_error, None);
        let image = evaluate(&mut engine, "document.getElementById('i')")["result"]["objectId"].clone();
        let quads = dispatch(&mut engine, "DOM.getContentQuads", json!({ "objectId": image }));
        assert_eq!(quads["quads"][0], json!([0.0, 0.0, 30.0, 0.0, 30.0, 10.0, 0.0, 10.0]));
        let shot = screenshot_png(&mut engine, json!({ "clip": { "x": 0, "y": 0, "width": 40, "height": 10, "scale": 1 } }));
        let decoded = crate::images::decode(&shot).unwrap();
        assert_eq!(decoded.pixel(15, 5), [255, 0, 0, 255]);
        assert_eq!(decoded.pixel(35, 5), [255, 255, 255, 255]);
    }

    #[test]
    fn script_created_svg_logo_keeps_its_viewbox_and_paints_its_computed_fill() {
        let html = "<style>body { margin:0 } :root { --color-logo: var(--core-green); --core-green: #03c75a } \
             #logo { width:24px; height:24px } #logo > svg { fill:var(--color-logo) }</style>\
             <body><div id=logo></div><script>\
             const icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');\
             icon.setAttribute('viewBox', '0 0 24 24'); icon.setAttribute('fill', 'none');\
             const path = document.createElementNS(icon.namespaceURI, 'path');\
             path.setAttribute('d', 'M0 0h24v24H0z'); icon.appendChild(path);\
             document.getElementById('logo').appendChild(icon);</script></body>";
        let mut engine = HtmlEngine::new("t");
        use base64::Engine as _;
        engine.load(&format!("data:text/html;base64,{}", base64::engine::general_purpose::STANDARD.encode(html)));
        assert_eq!(evaluate(&mut engine, "document.querySelector('svg').getAttributeNames().join()")["result"]["value"], "viewBox,fill");
        assert_eq!(evaluate(&mut engine, "document.querySelector('svg').getAttribute('viewBox')")["result"]["value"], "0 0 24 24");
        assert_eq!(evaluate(&mut engine, "document.querySelector('svg').hasAttribute('viewbox')")["result"]["value"], false);
        assert_eq!(evaluate(&mut engine, "getComputedStyle(document.querySelector('svg')).fill")["result"]["value"], "#03c75a");
        let rect = evaluate(&mut engine, "(() => { const r = document.querySelector('svg').getBoundingClientRect(); return JSON.stringify([r.width, r.height]); })()");
        assert_eq!(rect["result"]["value"], "[24,24]");
        let shot = screenshot_png(&mut engine, json!({ "clip": { "x": 0, "y": 0, "width": 30, "height": 30, "scale": 1 } }));
        let decoded = crate::images::decode(&shot).unwrap();
        assert_eq!(decoded.pixel(12, 12), [3, 199, 90, 255]);
        assert_eq!(decoded.pixel(27, 12), [255, 255, 255, 255]);
    }

    #[test]
    fn generated_styles_are_queryable_without_leaking_render_nodes_into_the_dom() {
        let mut engine = engine_with(
            "<style>#label::before{content:'prefix ';color:red} #label:after{content:attr(data-suffix)}</style>\
             <body><button id=label data-suffix='!'><span style='position:absolute;clip:rect(0,0,0,0)'>Accessible label</span></button></body>",
        );
        let answer = evaluate(&mut engine, "(() => { const b=document.getElementById('label'); \
            b.getBoundingClientRect(); return JSON.stringify([getComputedStyle(b,'::before').content, \
            getComputedStyle(b,':after').content,b.childNodes.length,b.textContent,document.querySelectorAll('whb-pseudo,whb-text').length]); })()");
        assert_eq!(answer["result"]["value"], r#"["'prefix '","attr(data-suffix)",1,"Accessible label",0]"#);
        let ax = dispatch(&mut engine,"Accessibility.getFullAXTree",json!({}));
        assert!(ax["nodes"].as_array().unwrap().iter().any(|n| n["role"]["value"] == "button" && n["name"]["value"] == "Accessible label"));
    }

    #[test]
    fn svg_attribute_apis_are_case_sensitive_but_html_attributes_are_not() {
        let mut engine = engine_with("<div id=h></div><svg id=s viewBox='0 0 10 10'></svg>");
        let answer = evaluate(&mut engine, "(() => {\
            const h = document.getElementById('h'), s = document.getElementById('s');\
            h.setAttribute('DATA-X', 'html'); s.setAttribute('viewBox', '0 0 24 24');\
            const attr = s.attributes.getNamedItem('viewBox');\
            const values = [h.getAttribute('data-x'), h.hasAttribute('DATA-X'),\
              s.getAttribute('viewBox'), s.hasAttribute('viewbox'), attr.name, attr.value,\
              s.getAttributeNode('viewBox').value, s.outerHTML.includes('viewBox=')];\
            s.removeAttribute('viewbox'); values.push(s.hasAttribute('viewBox'));\
            s.removeAttribute('viewBox'); values.push(s.hasAttribute('viewBox'));\
            h.removeAttribute('DATA-X'); values.push(h.hasAttribute('data-x'));\
            return JSON.stringify(values); })()");
        assert_eq!(answer["result"]["value"], r#"["html",true,"0 0 24 24",false,"viewBox","0 0 24 24","0 0 24 24",true,true,false,false]"#);
    }

    #[test]
    fn a_screencast_sends_a_frame_when_the_page_changes_and_the_last_was_acknowledged() {
        let mut engine = engine_with("<body><p id=t>one</p></body>");
        let frames = |engine: &mut HtmlEngine, method: &str, params: Value| -> Vec<Value> {
            events_of(engine, |e, out| {
                e.dispatch(method, &params, out).unwrap();
            })
            .into_iter()
            .filter(|(m, _)| m == "Page.screencastFrame")
            .map(|(_, p)| p)
            .collect()
        };
        let first = frames(&mut engine, "Page.startScreencast", json!({ "format": "jpeg", "quality": 80, "maxWidth": 400, "maxHeight": 400 }));
        assert_eq!(first.len(), 1);
        assert!(first[0]["data"].as_str().unwrap().starts_with("/9j/"), "a JPEG");
        assert_eq!(first[0]["metadata"]["deviceWidth"], 800.0);
        use base64::Engine as _;
        let jpeg = base64::engine::general_purpose::STANDARD.decode(first[0]["data"].as_str().unwrap()).unwrap();
        assert_eq!(crate::images::dimensions(&jpeg), Some((400, 300)), "scaled to fit, not stretched");

        let change = json!({ "expression": "document.getElementById('t').textContent = 'two'" });
        assert!(frames(&mut engine, "Runtime.evaluate", change.clone()).is_empty(), "the last frame is not acknowledged yet");
        let session = first[0]["sessionId"].clone();
        assert_eq!(frames(&mut engine, "Page.screencastFrameAck", json!({ "sessionId": session })).len(), 1, "the change, once acknowledged");
        let second_session = 2;
        assert!(frames(&mut engine, "Page.screencastFrameAck", json!({ "sessionId": second_session })).is_empty(), "nothing changed");
        assert!(frames(&mut engine, "Runtime.evaluate", json!({ "expression": "1 + 1" })).is_empty());
        // Focus moves the caret, without changing the document.
        evaluate(&mut engine, "document.body.insertAdjacentHTML('beforeend', '<input id=f>')");
        let session = frames(&mut engine, "Page.screencastFrameAck", json!({ "sessionId": 2 }))[0]["sessionId"].clone();
        dispatch(&mut engine, "Page.screencastFrameAck", json!({ "sessionId": session }));
        assert_eq!(frames(&mut engine, "Runtime.evaluate", json!({ "expression": "document.getElementById('f').focus()" })).len(), 1);
        dispatch(&mut engine, "Page.stopScreencast", json!({}));
        assert!(frames(&mut engine, "Runtime.evaluate", change).is_empty());
    }

    #[test]
    fn a_short_secret_does_not_corrupt_a_screenshot() {
        let page = "<body><h1>Hello</h1></body>";
        use base64::Engine as _;
        let mut plain = engine_with(page);
        let data = dispatch(&mut plain, "Page.captureScreenshot", json!({}))["data"].as_str().unwrap().to_string();
        // A "secret" that occurs in the image's base64 by construction.
        let mut secrets = Secrets::new();
        secrets.add("s@http://example.test", data[data.len() / 2..data.len() / 2 + 4].to_string()).unwrap();
        let mut engine = HtmlEngine::with_shared("t", Arc::new(Mutex::new(CookieJar::new())), Arc::new(secrets));
        {
            let mut doc = engine.doc();
            *doc = parse_html(page);
            doc.set_url("http://example.test/page");
        }
        let shot = dispatch(&mut engine, "Page.captureScreenshot", json!({}))["data"].as_str().unwrap().to_string();
        assert!(shot == data, "the payload went out untouched");
        assert!(base64::engine::general_purpose::STANDARD.decode(shot).is_ok());
    }

    #[test]
    fn module_scripts_import_resolve_and_run_in_order() {
        let base = crate::test_server::serve_together(
            vec![
                (
                    "/",
                    "text/html",
                    "<script type=importmap>{\"imports\": {\"lib\": \"/lib/index.js\"}}</script>\
                     <script>window.log = []; log.push('classic');\
                       import('./dyn.js').then((m) => log.push('classic import ' + m.value));\
                       document.addEventListener('DOMContentLoaded', () => log.push('dcl'));</script>\
                     <script type=module>import { name } from './a.js'; import lib from 'lib';\
                       log.push('inline ' + name + ' ' + lib + ' ' + (import.meta.url === location.href));</script>\
                     <script type=module src=main.js></script>\
                     <script nomodule>log.push('nomodule ran')</script>\
                     <script defer src=deferred.js></script>"
                        .into(),
                ),
                ("/a.js", "text/javascript", "export const name = 'a'; log.push('a evaluated');".into()),
                ("/lib/index.js", "text/javascript", "import { helper } from './helper.js'; export default helper();".into()),
                ("/lib/helper.js", "text/javascript", "export const helper = () => 'lib';".into()),
                (
                    "/main.js",
                    "text/javascript",
                    "import { name } from './a.js'; const later = await Promise.resolve('tla');\
                     log.push('main ' + name + ' ' + later + ' ' + import.meta.url.split('/').pop());\
                     const dynamic = await import('./dyn.js'); log.push('main import ' + dynamic.value);"
                        .into(),
                ),
                ("/dyn.js", "text/javascript", "export const value = 42;".into()),
                ("/deferred.js", "text/javascript", "log.push('deferred');".into()),
            ],
            vec![],
        );
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/"));
        assert_eq!(engine.last_error, None);
        let log = evaluate(&mut engine, "JSON.stringify(log)")["result"]["value"].as_str().unwrap().to_string();
        let log: Vec<String> = serde_json::from_str(&log).unwrap();
        let at = |entry: &str| log.iter().position(|l| l == entry).unwrap_or_else(|| panic!("{entry} missing from {log:?}"));
        assert_eq!(log[0], "classic");
        assert_eq!(log.iter().filter(|l| *l == "a evaluated").count(), 1, "one module instance: {log:?}");
        assert!(at("a evaluated") < at("inline a lib true"));
        assert!(at("inline a lib true") < at("main a tla main.js"), "{log:?}");
        assert!(at("main a tla main.js") < at("main import 42"));
        assert!(at("deferred") < at("dcl"), "{log:?}");
        at("classic import 42");
        assert!(!log.iter().any(|l| l == "nomodule ran"), "{log:?}");
    }

    #[test]
    fn a_bare_specifier_without_an_import_map_fails_like_chrome() {
        let mut engine = engine_with("<body></body>");
        engine.load(
            "data:text/html,<script>window.log = []</script>\
             <script type=module onerror=\"log.push('error event')\">import x from 'vue'</script>",
        );
        let error = engine.last_error.clone().unwrap_or_default();
        assert!(error.contains("Failed to resolve module specifier \"vue\""), "{error}");
        assert_eq!(evaluate(&mut engine, "log.join()")["result"]["value"], "error event");
    }

    #[test]
    fn pages_scroll_as_agent_browser_drives_them() {
        let mut engine = engine_with(
            "<body style='margin:0'>\
             <div id=list style='height:100px;overflow:auto'><div style='height:400px'></div></div>\
             <div style='height:1500px'></div>\
             <button id=far style='display:block;height:40px;width:100px;background:#00f'>far</button>\
             <div style='height:500px'></div></body>",
        );
        evaluate(&mut engine, "window.log = []; addEventListener('scroll', () => log.push('scroll ' + scrollY))");

        // `agent-browser scroll down 300`, through Runtime.evaluate, which
        // runs the tasks it queued, as the event's is.
        dispatch(&mut engine, "Runtime.evaluate", json!({ "expression": "window.scrollBy(0, 300)" }));
        assert_eq!(evaluate(&mut engine, "scrollY")["result"]["value"], 300);
        assert_eq!(evaluate(&mut engine, "log.join()")["result"]["value"], "scroll 300", "after its task");

        // A click on an element out of view: scrollIntoView, then the
        // centre of the box, and what is there.
        let found = evaluate(
            &mut engine,
            "(() => { const el = document.getElementById('far');\
               el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });\
               const r = el.getBoundingClientRect();\
               return [Math.round(r.top), document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2) === el].join() })()",
        );
        assert_eq!(found["result"]["value"], "280,true", "centred in the 600px viewport");
        let far = evaluate(&mut engine, "document.getElementById('far')")["result"]["objectId"].clone();
        let quads = dispatch(&mut engine, "DOM.getContentQuads", json!({ "objectId": far }));
        assert_eq!(quads["quads"][0][1], 280.0);
        let metrics = dispatch(&mut engine, "Page.getLayoutMetrics", json!({}));
        assert_eq!(metrics["cssLayoutViewport"]["pageY"], 1320.0);

        // The screenshot shows the viewport where it is scrolled to.
        let png = screenshot_png(&mut engine, json!({}));
        // Sample its face, away from the now-centred button label.
        assert_eq!(crate::images::decode(&png).unwrap().pixel(5, 285), [0, 0, 255, 255]);

        // The wheel over the inner list scrolls the list, not the page.
        evaluate(&mut engine, "scrollTo(0, 0)");
        dispatch(&mut engine, "Input.dispatchMouseEvent", json!({ "type": "mouseWheel", "x": 50, "y": 50, "deltaX": 0, "deltaY": 120 }));
        assert_eq!(evaluate(&mut engine, "[document.getElementById('list').scrollTop, scrollY].join()")["result"]["value"], "120,0");
    }

    fn runtime_events(events: &[(String, Value)]) -> Vec<&Value> {
        events
            .iter()
            .filter(|(m, _)| m == "Runtime.consoleAPICalled" || m == "Runtime.exceptionThrown")
            .map(|(_, p)| p)
            .collect()
    }

    #[test]
    fn console_calls_and_uncaught_exceptions_reach_the_client_as_chrome_sends_them() {
        let mut engine = engine_with_secrets("<body></body>");
        let quiet = events_of(&mut engine, |e, out| {
            e.dispatch("Runtime.evaluate", &json!({ "expression": "console.log('before enable')" }), out).unwrap();
        });
        assert!(runtime_events(&quiet).is_empty(), "nothing before Runtime.enable");

        dispatch(&mut engine, "Runtime.enable", json!({}));
        let page = "console.log('hello', 42, { a: 1, b: 'x' }, [1, 2]);\
                    console.warn('careful'); console.error(new Error('boom'));\
                    console.count(); console.count(); console.assert(1 === 2, 'math');\
                    console.log('pw', 'hunter2-s3cret');\
                    setTimeout(() => { throw new TypeError('late') }, 0);\
                    Promise.reject(new Error('rejected'));\
                    addEventListener('unhandledrejection', (e) => { if (e.reason === 'quiet') e.preventDefault() });\
                    Promise.reject('quiet');\
                    Promise.reject(new Error('handled later')).catch(() => {});";
        let events = events_of(&mut engine, |e, out| {
            e.dispatch("Runtime.evaluate", &json!({ "expression": page }), out).unwrap();
        });
        let events = runtime_events(&events);
        let consoles: Vec<&&Value> = events.iter().filter(|p| p.get("type").is_some()).collect();
        let types: Vec<&str> = consoles.iter().map(|p| p["type"].as_str().unwrap()).collect();
        assert_eq!(types, ["log", "warning", "error", "count", "count", "assert", "log"]);
        let hello = &consoles[0]["args"];
        assert_eq!(hello[0], json!({ "type": "string", "value": "hello" }));
        assert_eq!(hello[1]["value"], 42);
        assert_eq!(hello[2]["preview"]["properties"], json!([{ "name": "a", "type": "number", "value": "1" }, { "name": "b", "type": "string", "value": "x" }]));
        assert_eq!(hello[3]["subtype"], "array");
        assert_eq!(hello[3]["description"], "Array(2)");
        assert_eq!(consoles[0]["executionContextId"], 1);
        assert!(consoles[0]["timestamp"].as_f64().unwrap() > 1.7e12, "epoch milliseconds");
        assert_eq!(consoles[2]["args"][0]["subtype"], "error");
        assert_eq!(consoles[4]["args"][0]["value"], "default: 2");
        assert_eq!(consoles[5]["args"][0]["value"], "Assertion failed: math");
        // Console arguments go out through the same scrub as everything else.
        assert_eq!(consoles[6]["args"][1]["value"], "{{secret:pw}}");

        let exceptions: Vec<&Value> = events.iter().filter_map(|p| p.get("exceptionDetails")).collect();
        let texts: Vec<(String, String)> = exceptions
            .iter()
            .map(|d| (d["text"].as_str().unwrap().to_string(), d["exception"]["description"].as_str().unwrap().lines().next().unwrap().to_string()))
            .collect();
        assert!(texts.contains(&("Uncaught".into(), "TypeError: late".into())), "{texts:?}");
        assert!(texts.contains(&("Uncaught (in promise)".into(), "Error: rejected".into())), "{texts:?}");
        assert_eq!(texts.len(), 2, "the prevented and the handled ones are not reported: {texts:?}");
        assert_ne!(exceptions[0]["exceptionId"], exceptions[1]["exceptionId"]);
    }

    #[test]
    fn a_failing_script_is_reported_with_where_it_threw() {
        let mut engine = HtmlEngine::new("t");
        dispatch(&mut engine, "Runtime.enable", json!({}));
        let events = events_of(&mut engine, |e, out| {
            e.navigate("data:text/html,<script>\nwindow.onerror = () => { window.seen = true };\nnope();\n</script>", out).unwrap();
        });
        let details = runtime_events(&events).into_iter().find_map(|p| p.get("exceptionDetails")).expect("reported");
        assert_eq!(details["text"], "Uncaught");
        assert_eq!(details["lineNumber"], 2, "{details}");
        assert!(details["exception"]["description"].as_str().unwrap().starts_with("ReferenceError"), "{details}");
        assert_eq!(evaluate(&mut engine, "window.seen")["result"]["value"], true, "and fires error at the window");
    }

    #[test]
    fn elements_are_named_on_the_window_by_id_and_name() {
        let mut engine = engine_with(
            "<body><div id=menu>m</div><form name=login></form><img name=logo><input name=user id=field>\
             <div id=alert>shadowed</div><div id=later></div></body>",
        );
        let check = |engine: &mut HtmlEngine, expression: &str| evaluate(engine, expression)["result"]["value"].clone();
        assert_eq!(check(&mut engine, "menu.textContent + ' ' + (window.menu === document.getElementById('menu'))"), "m true");
        assert_eq!(check(&mut engine, "[login.tagName, logo.tagName, field.tagName, typeof user].join()"), "FORM,IMG,INPUT,undefined");
        assert_eq!(check(&mut engine, "typeof alert"), "function", "real globals come first");
        assert_eq!(check(&mut engine, "'menu' in window && !('nothing' in window) && typeof nothing === 'undefined'"), true);
        assert_eq!(check(&mut engine, "window instanceof Window"), true);
        // A name follows the document.
        assert_eq!(check(&mut engine, "document.getElementById('later').id = 'renamed'; typeof later + ' ' + renamed.tagName"), "undefined DIV");
        // Script's own globals win, and assigning one does not touch the element.
        assert_eq!(check(&mut engine, "menu = 5; menu + ' ' + document.getElementById('menu').textContent"), "5 m");
    }

    #[test]
    fn pages_in_euc_kr_are_decoded_and_their_scripts_and_sheets_too() {
        let euc = |text: &str| encoding_rs::EUC_KR.encode(text).0.into_owned();
        let base = crate::test_server::serve_together(
            vec![
                ("/header", "text/html; charset=euc-kr", euc("<title>한글 제목</title><script src=s.js></script><link rel=stylesheet href=c.css><p id=p>본문</p>")),
                ("/meta", "text/html", euc("<meta charset=\"euc-kr\"><title>메타 선언</title>")),
                ("/none", "text/html", euc("<title>선언 없음</title>")),
                ("/s.js", "text/javascript", euc("window.fromScript = '스크립트';")),
                ("/c.css", "text/css", euc("p::after { content: '시트' } p { font-family: '맑은 고딕' }")),
            ],
            vec![],
        );
        let mut engine = HtmlEngine::new("t");
        for (path, title) in [("/meta", "메타 선언"), ("/none", "선언 없음"), ("/header", "한글 제목")] {
            engine.load(&format!("{base}{path}"));
            assert_eq!(engine.last_error, None);
            assert_eq!(engine.info().title, title, "{path}");
            assert_eq!(evaluate(&mut engine, "document.characterSet")["result"]["value"], "EUC-KR", "{path}");
        }
        assert_eq!(evaluate(&mut engine, "fromScript + ' ' + document.getElementById('p').textContent")["result"]["value"], "스크립트 본문");
        assert_eq!(evaluate(&mut engine, "getComputedStyle(document.getElementById('p')).fontFamily")["result"]["value"], "'맑은 고딕'");
        engine.load("data:text/html,<title>x</title>");
        assert_eq!(evaluate(&mut engine, "document.characterSet")["result"]["value"], "UTF-8");
    }

    #[test]
    fn text_decoders_know_every_encoding_and_svg_strings_are_animated() {
        let mut engine = engine_with("<body><svg><a id=s class='c1'></a></svg></body>");
        let value = |engine: &mut HtmlEngine, expression: &str| evaluate(engine, expression)["result"]["value"].clone();
        assert_eq!(value(&mut engine, "new TextDecoder('utf-16le').decode(new Uint8Array([0xff, 0xfe, 0x68, 0, 0x69, 0]))"), "hi");
        assert_eq!(value(&mut engine, "new TextDecoder('ks_c_5601-1987').encoding"), "euc-kr");
        assert_eq!(value(&mut engine, "new TextDecoder('euc-kr').decode(new Uint8Array([0xc7, 0xd1, 0xb1, 0xdb]))"), "한글");
        assert_eq!(value(&mut engine, "new TextDecoder('utf8').decode(new Uint8Array([0xed, 0x95, 0x9c]))"), "한");
        assert_eq!(
            value(&mut engine, "try { new TextDecoder('utf-16le', { fatal: true }).decode(new Uint8Array([0x00, 0xd8])) } catch (e) { e.name }"),
            "TypeError"
        );
        assert_eq!(value(&mut engine, "try { new TextDecoder('nope') } catch (e) { e.name }"), "RangeError");
        assert_eq!(
            value(&mut engine, "const c = document.getElementById('s').className; [c instanceof SVGAnimatedString, c.baseVal, c.animVal].join()"),
            "true,c1,c1"
        );
    }

    #[test]
    fn going_back_through_push_state_entries_pops_state_without_loading() {
        let (base, requests) = crate::test_server::serve_script(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 16\r\n\r\n<title>a</title>".into(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 16\r\n\r\n<title>b</title>".into(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 16\r\n\r\n<title>a</title>".into(),
        ]);
        let mut engine = HtmlEngine::new("t");
        engine.load(&format!("{base}/a"));
        let run = |engine: &mut HtmlEngine, expression: &str| -> Vec<(String, Value)> {
            events_of(engine, |e, out| {
                e.dispatch("Runtime.evaluate", &json!({ "expression": expression }), out).unwrap();
            })
        };
        let value = |engine: &mut HtmlEngine, expression: &str| evaluate(engine, expression)["result"]["value"].clone();
        run(
            &mut engine,
            "window.log = []; addEventListener('popstate', (e) => log.push('pop ' + JSON.stringify(e.state)));\
             addEventListener('hashchange', () => log.push('hash ' + location.hash));\
             history.pushState({ step: 1 }, '', '/a/one'); history.pushState({ step: 2 }, '', '/a/two');\
             location.hash = 'x';",
        );
        assert_eq!(value(&mut engine, "history.length"), 4);
        let urls = |engine: &HtmlEngine| engine.history.iter().map(|e| e.url.rsplit('/').next().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(urls(&engine), ["a", "one", "two", "two#x"]);

        // Back from the fragment, then back once more: no load, the state comes back.
        let events = run(&mut engine, "history.back()");
        assert!(events.iter().any(|(m, p)| m == "Page.navigatedWithinDocument" && p["url"].as_str().unwrap().ends_with("/a/two")));
        assert!(!events.iter().any(|(m, _)| m == "Page.frameNavigated"), "nothing loaded");
        run(&mut engine, "history.back()");
        assert_eq!(value(&mut engine, "location.pathname + ' ' + history.state.step"), "/a/one 1");
        // Setting the hash fired its own hashchange; going back from it
        // pops, and the fragment is gone again.
        assert_eq!(value(&mut engine, "log.join(' | ')"), "hash #x | pop {\"step\":2} | hash  | pop {\"step\":1}");
        assert_eq!(value(&mut engine, "history.length"), 4, "going back keeps the entries");
        run(&mut engine, "history.forward()");
        assert_eq!(value(&mut engine, "history.state.step"), 2);

        // Another document, and back: that one loads again, with its state.
        run(&mut engine, &format!("location.href = '{base}/b'"));
        assert_eq!(engine.info().title, "b");
        run(&mut engine, "history.back()");
        assert_eq!(engine.info().title, "a");
        assert_eq!(value(&mut engine, "location.pathname + ' ' + history.state.step"), "/a/two 2");
        let _ = (requests.recv(), requests.recv(), requests.recv());
    }

    #[test]
    fn methods_that_would_lie_return_an_error_instead() {
        let mut engine = engine_with("<body>x</body>");
        let mut events = Vec::new();
        for method in ["Input.dispatchTouchEvent"] {
            let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
            let error = engine.dispatch(method, &Value::Null, &mut sink).expect_err(method);
            assert!(
                error.message.contains("not implemented yet"),
                "{method} should say why: {}",
                error.message
            );
        }
    }

    #[test]
    fn the_engine_is_send() {
        // `browser-core` hands engines to connection threads.
        fn assert_send<T: Send>() {}
        assert_send::<HtmlEngine>();
    }

    #[test]
    fn navigate_on_a_data_url_parses_and_reports() {
        let mut engine = HtmlEngine::new("t");
        let mut events = Vec::new();
        let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
        engine
            .navigate("data:text/html,<title>Inline</title><body><h1>Hi</h1></body>", &mut sink)
            .expect("navigate");

        assert_eq!(engine.info().title, "Inline");
        let names: Vec<&str> = events.iter().map(|(m, _)| m.as_str()).collect();
        assert!(names.contains(&"Page.loadEventFired"), "{names:?}");
        assert!(names.contains(&"Runtime.executionContextCreated"), "{names:?}");
    }

    #[test]
    fn a_failed_navigation_becomes_an_error_page_not_a_failure() {
        let mut engine = HtmlEngine::new("t");
        // Nothing listens on port 1. The engine reports that as page content
        // the client can read rather than as a protocol failure.
        engine.load("http://127.0.0.1:1/");
        assert!(engine.last_error.as_deref().unwrap_or("").contains("connect"));
        assert_eq!(engine.info().title, "Navigation failed");
        let doc = engine.doc();
        assert!(doc.text_content(doc.document_id()).contains("connect"));
    }

    #[test]
    fn full_ax_tree_comes_from_the_loaded_document() {
        let mut engine =
            engine_with("<title>Login</title><body><h1>Login</h1><a href='/'>go</a></body>");
        let mut events = Vec::new();
        let mut sink = |m: &str, p: Value| events.push((m.to_string(), p));
        let value =
            engine.dispatch("Accessibility.getFullAXTree", &Value::Null, &mut sink).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert!(nodes
            .iter()
            .any(|n| n["role"]["value"] == "heading" && n["name"]["value"] == "Login"));
        assert!(nodes.iter().any(|n| n["role"]["value"] == "link" && n["name"]["value"] == "go"));
    }

    /// Opens a real page and prints what its scripts said. Not a check, a
    /// tool for finding what a site needs next:
    ///
    ///   WHB_PROBE_URL=https://example.com cargo test -p engine probe_a_url -- --ignored --nocapture
    #[test]
    #[ignore]
    fn probe_a_url() {
        let Ok(url) = std::env::var("WHB_PROBE_URL") else { return };
        let mut engine = HtmlEngine::new("probe");
        let started = std::time::Instant::now();
        engine.load(&url);
        eprintln!("loaded {} in {:?}", engine.url, started.elapsed());
        eprintln!("last error: {:?}", engine.last_error);
        if let Some(script) = &engine.script {
            for line in script.console_lines() {
                eprintln!("console: {}", line.chars().take(600).collect::<String>());
            }
        }
        let doc = engine.doc();
        let text = doc.text_content(doc.document_id());
        eprintln!("text: {}", text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(800).collect::<String>());
    }
}

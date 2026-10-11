//! Installs the DOM primitives the JavaScript prelude builds on.
//!
//! Rust exposes flat functions keyed by numeric node id; the prelude turns them
//! into objects with getters, methods and identity. The split is deliberate.
//! Object shape and identity are cheap in JavaScript, and the engine's arena is
//! cheap to index from Rust, so each side does what it is good at.
//!
//! No primitive returns a JS object. They return scalars or flat vectors and
//! the prelude assembles them, which keeps every closure a plain `move`
//! closure.
//!
//! Everything shared with a callback is `Arc<Mutex<_>>`: the JS runtime lives
//! on its own thread (see `crate::js::ScriptHost`) and the closures outlive this
//! struct.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rquickjs::context::EvalOptions;
use rquickjs::{CatchResultExt, Ctx, Function, Object, Value};

use crate::cookies::{self, Source};
use crate::dom::{Document, NodeData, NodeId};
use crate::fetch;
use crate::js::JsError;
use crate::network::ResourceType;
use crate::parse::parse_fragment;
use crate::select;

/// The prelude that builds `document`, `window` and friends on top of the
/// primitives. A real `.js` file so it reads as JavaScript.
///
/// The files in `prelude/` share one function scope: they are concatenated in
/// name order inside it, so each file stays small and the page's global
/// object sees only what the last one exports.
const PRELUDE: &str = concat!(
    "(function () {\n",
    include_str!("prelude/00-core.js"),
    include_str!("prelude/10-events.js"),
    include_str!("prelude/20-node.js"),
    include_str!("prelude/30-element.js"),
    include_str!("prelude/40-forms.js"),
    include_str!("prelude/45-submit.js"),
    include_str!("prelude/46-elements.js"),
    include_str!("prelude/50-document.js"),
    include_str!("prelude/60-data.js"),
    include_str!("prelude/65-fetch.js"),
    include_str!("prelude/70-window.js"),
    include_str!("prelude/80-global.js"),
    include_str!("prelude/90-cdp.js"),
    "\n})();\n"
);

/// A navigation script asked for: `location.href = ...` is a `GET`, a form
/// submission may be a `POST` with a body.
#[derive(Debug, Clone, PartialEq)]
pub struct Navigation {
    pub url: String,
    pub method: String,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    /// `location.reload()`: load even though the URL is the current one.
    pub reload: bool,
}

impl Navigation {
    pub fn get(url: &str) -> Self {
        Navigation {
            url: url.to_string(),
            method: "GET".into(),
            body: Vec::new(),
            content_type: None,
            reload: false,
        }
    }
}

/// State the primitives share with each other and with the engine.
pub struct DomState {
    pub doc: Arc<Mutex<Document>>,
    /// The engine's network client. Its jar is what `document.cookie` reads,
    /// and `XMLHttpRequest`, `fetch` and `<script src>` go through it, so
    /// every request carries the page's cookies.
    client: fetch::Client,
    ready_state: Arc<Mutex<String>>,
    focus: Arc<Mutex<Option<NodeId>>>,
    /// Set when script asks to navigate, so the engine can act on it after the
    /// evaluation finishes rather than re-entering the runtime.
    requested_navigation: Arc<Mutex<Option<Navigation>>>,
    /// Net steps through history that script asked for (`history.back()` is
    /// -1), acted on by the engine the same way as a navigation.
    requested_history: Arc<Mutex<i32>>,
    /// Set when script changed the URL without loading anything
    /// (`pushState`, a fragment change). `true` replaces the current history
    /// entry rather than adding one.
    url_change: Arc<Mutex<Option<bool>>>,
    viewport: Arc<Mutex<(f64, f64)>>,
    user_agent: Arc<Mutex<String>>,
    /// `localStorage` and `sessionStorage`, keyed by kind and origin. In
    /// memory only, like the cookie jar, and for the same reason: login tokens
    /// end up here.
    storage: Arc<Mutex<HashMap<String, Vec<(String, String)>>>>,
    /// Requests script started and has not collected yet, by handle. See
    /// `httpStart`.
    requests: Arc<Mutex<HashMap<u32, (fetch::Pending, String)>>>,
    /// The page's import maps, merged, for resolving module specifiers.
    import_map: Mutex<crate::modules::ImportMap>,
}

impl DomState {
    pub fn new(
        doc: Arc<Mutex<Document>>,
        client: fetch::Client,
        viewport: (f64, f64),
        user_agent: &str,
    ) -> Self {
        DomState {
            doc,
            client,
            ready_state: Arc::new(Mutex::new("loading".to_string())),
            focus: Arc::new(Mutex::new(None)),
            requested_navigation: Arc::new(Mutex::new(None)),
            requested_history: Arc::new(Mutex::new(0)),
            url_change: Arc::new(Mutex::new(None)),
            viewport: Arc::new(Mutex::new(viewport)),
            user_agent: Arc::new(Mutex::new(user_agent.to_string())),
            storage: Arc::new(Mutex::new(HashMap::new())),
            requests: Arc::new(Mutex::new(HashMap::new())),
            import_map: Mutex::new(Default::default()),
        }
    }

    pub fn set_viewport(&self, width: f64, height: f64) {
        if let Ok(mut viewport) = self.viewport.lock() {
            *viewport = (width, height);
        }
    }

    pub fn take_requested_navigation(&self) -> Option<Navigation> {
        self.requested_navigation.lock().ok().and_then(|mut slot| slot.take())
    }

    /// Forgets what belonged to the previous document: focus, ready state,
    /// and any navigation it asked for but did not get.
    pub fn reset_page(&self) {
        if let Ok(mut focus) = self.focus.lock() {
            *focus = None;
        }
        if let Ok(mut ready) = self.ready_state.lock() {
            *ready = "loading".to_string();
        }
        let _ = self.take_requested_navigation();
        let _ = self.take_requested_history();
        let _ = self.take_url_change();
        if let Ok(mut map) = self.import_map.lock() {
            *map = Default::default();
        }
    }

    pub fn client(&self) -> &fetch::Client {
        &self.client
    }

    pub fn document_url(&self) -> String {
        with_doc(&self.doc, |d| d.url().to_string()).unwrap_or_default()
    }

    pub fn import_map(&self) -> crate::modules::ImportMap {
        self.import_map.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// Adds a `<script type=importmap>`'s JSON to the page's map.
    pub fn add_import_map(&self, json: &str) -> Result<(), String> {
        let base = self.document_url();
        self.import_map.lock().map_err(|_| "the import map is unavailable".to_string())?.merge(json, &base)
    }

    /// Whether script changed the URL in place since the last call, and if so
    /// whether it replaced the history entry.
    pub fn take_url_change(&self) -> Option<bool> {
        self.url_change.lock().ok().and_then(|mut slot| slot.take())
    }

    pub fn take_requested_history(&self) -> i32 {
        self.requested_history.lock().map(|mut steps| std::mem::take(&mut *steps)).unwrap_or(0)
    }

    pub fn ready_state(&self) -> String {
        self.ready_state.lock().map(|s| s.clone()).unwrap_or_else(|_| "loading".into())
    }

    /// The node script last focused, if it is still in the document.
    pub fn focused(&self) -> Option<NodeId> {
        self.focus.lock().ok().and_then(|f| *f)
    }
}

fn map_error(err: rquickjs::Error) -> JsError {
    JsError { message: err.to_string() }
}

/// Registers one primitive at the call site. Inlined rather than wrapped in a
/// generic helper, because the helper left the closure lifetime ambiguous.
macro_rules! install_fn {
    ($ctx:expr, $api:expr, $name:expr, $f:expr) => {{
        let function = Function::new($ctx.clone(), $f).map_err(map_error)?;
        $api.set($name, function).map_err(map_error)?;
    }};
}

/// Locks the shared document, or `None` if the mutex is poisoned.
fn with_doc<R>(doc: &Arc<Mutex<Document>>, f: impl FnOnce(&Document) -> R) -> Option<R> {
    doc.lock().ok().map(|d| f(&d))
}

fn with_doc_mut<R>(doc: &Arc<Mutex<Document>>, f: impl FnOnce(&mut Document) -> R) -> Option<R> {
    doc.lock().ok().map(|mut d| f(&mut d))
}

/// Installs `globalThis.__dom`, then evaluates the prelude.
pub fn install(ctx: &Ctx<'_>, state: &Arc<DomState>) -> Result<(), JsError> {
    let api = Object::new(ctx.clone()).map_err(map_error)?;
    let doc = Arc::clone(&state.doc);

    put_reads(ctx, &api, &doc)?;
    put_selectors(ctx, &api, state)?;
    put_mutations(ctx, &api, &doc)?;
    put_geometry(ctx, &api, state)?;
    put_navigation(ctx, &api, state)?;
    put_platform(ctx, &api, state)?;

    ctx.globals().set("__dom", api).map_err(map_error)?;
    // Caught, so a prelude bug reports its own message rather than QuickJS's
    // generic "Exception generated by QuickJS".
    ctx.eval::<Value, _>(PRELUDE)
        .catch(ctx)
        .map_err(|caught| JsError { message: format!("the DOM prelude failed: {caught}") })?;
    Ok(())
}

/// Reads. Every node-returning primitive returns `Option`, which JavaScript
/// sees as `undefined` for "none". Node 0 is the document, so 0 cannot mean
/// "none".
///
/// Written out rather than generated by a macro: the closures must own their
/// `Arc`, and a macro cannot hand its own binding to the caller's expressions
/// because macro hygiene keeps the two apart. Each block clones the `Arc` it
/// needs and the closure owns it, which is what makes the closure `'static`.
fn put_reads<'js>(
    ctx: &Ctx<'js>,
    api: &Object<'js>,
    doc: &Arc<Mutex<Document>>,
) -> Result<(), JsError> {
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "documentId", move || {
            with_doc(&doc, |d| d.document_id()).unwrap_or(0)
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "url", move || {
            with_doc(&doc, |d| d.url().to_string()).unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "title", move || {
            with_doc(&doc, |d| d.title()).flatten().unwrap_or_default()
        });
    }
    for (name, tag) in [("documentElement", "html"), ("body", "body"), ("head", "head")] {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, name, move || {
            with_doc(&doc, |d| d.find_tag(d.document_id(), tag)).flatten()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "nodeType", move |id: usize| {
            with_doc(&doc, |d| node_type_of(d, id)).unwrap_or(0)
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "nodeName", move |id: usize| {
            with_doc(&doc, |d| match &d.node(id).data {
                NodeData::Element { .. } => {
                    let tag = d.tag(id).unwrap_or("");
                    // HTML elements report upper case; SVG keeps its case.
                    if d.namespace(id) == "http://www.w3.org/1999/xhtml" {
                        tag.to_ascii_uppercase()
                    } else {
                        tag.to_string()
                    }
                }
                NodeData::Text => "#text".into(),
                NodeData::Comment => "#comment".into(),
                NodeData::Fragment => "#document-fragment".into(),
                NodeData::Doctype { name } => name.split(' ').next().unwrap_or("html").to_string(),
                NodeData::Document => "#document".into(),
            })
            .unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "tagName", move |id: usize| {
            with_doc(&doc, |d| d.tag(id).unwrap_or("").to_string()).unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "namespace", move |id: usize| {
            with_doc(&doc, |d| d.namespace(id).to_string()).unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "parent", move |id: usize| {
            with_doc(&doc, |d| d.parent(id)).flatten()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "children", move |id: usize| {
            with_doc(&doc, |d| d.children(id).to_vec()).unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "sibling", move |id: usize, direction: i32| {
            with_doc(&doc, |d| {
                let parent = d.parent(id)?;
                let siblings = d.children(parent);
                let position = siblings.iter().position(|s| *s == id)?;
                if direction < 0 {
                    position.checked_sub(1).and_then(|i| siblings.get(i)).copied()
                } else {
                    siblings.get(position + 1).copied()
                }
            })
            .flatten()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "textContent", move |id: usize| {
            with_doc(&doc, |d| d.text_content(id)).unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "data", move |id: usize| {
            with_doc(&doc, |d| d.node(id).text.clone()).unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "attr", move |id: usize, name: String| {
            with_doc(&doc, |d| d.attr(id, &name.to_ascii_lowercase()).map(str::to_string)).flatten()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "hasAttr", move |id: usize, name: String| {
            with_doc(&doc, |d| d.has_attr(id, &name.to_ascii_lowercase())).unwrap_or(false)
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "attributeNames", move |id: usize| {
            with_doc(&doc, |d| {
                d.attrs(id).iter().map(|a| a.local_name().to_string()).collect::<Vec<_>>()
            })
            .unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "attributeList", move |id: usize| {
            with_doc(&doc, |d| {
                d.attrs(id)
                    .iter()
                    .flat_map(|a| [a.local_name().to_string(), a.value.clone()])
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        });
    }
    install_fn!(ctx, api, "validSelector", |selector: String| select::parse_list(&selector).is_some());
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "byTag", move |root: usize, tag: String| {
            let tag = tag.to_ascii_lowercase();
            with_doc(&doc, |d| {
                d.descendants(root)
                    .skip(1)
                    .filter(|id| d.is_element(*id) && (tag == "*" || d.is_tag(*id, &tag)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "getElementById", move |value: String| {
            with_doc(&doc, |d| d.find_id(d.document_id(), &value)).flatten()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "contains", move |ancestor: usize, id: usize| {
            with_doc(&doc, |d| d.is_inclusive_ancestor(ancestor, id)).unwrap_or(false)
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "isConnected", move |id: usize| {
            with_doc(&doc, |d| d.is_inclusive_ancestor(d.document_id(), id)).unwrap_or(false)
        });
    }
    // Form control state. The page's own script reads a password field's
    // value, as it must to submit a login form; what keeps it out of the
    // accessibility tree and CDP descriptions is `ax.rs`, not this.
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "controlValue", move |id: usize| {
            with_doc(&doc, |d| d.control(id).value.clone()).flatten()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "controlChecked", move |id: usize| {
            with_doc(&doc, |d| d.control(id).checked).flatten()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "controlSelected", move |id: usize| {
            with_doc(&doc, |d| d.control(id).selected).flatten()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "innerHTML", move |id: usize| {
            with_doc(&doc, |d| {
                d.children(id)
                    .iter()
                    .map(|child| crate::browser::serialize(d, *child))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "outerHTML", move |id: usize| {
            with_doc(&doc, |d| crate::browser::serialize(d, id)).unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "interfaceName", move |id: usize| {
            with_doc(&doc, |d| crate::browser::class_for(d, id).to_string()).unwrap_or_default()
        });
    }
    Ok(())
}

/// Selector matching, which needs the focused element for `:focus`. An
/// invalid selector matches nothing here; the prelude checks `validSelector`
/// first and throws the SyntaxError a browser throws.
fn put_selectors<'js>(ctx: &Ctx<'js>, api: &Object<'js>, state: &Arc<DomState>) -> Result<(), JsError> {
    {
        let (doc, focus) = (Arc::clone(&state.doc), Arc::clone(&state.focus));
        install_fn!(ctx, api, "matches", move |id: usize, selector: String| {
            let focused = focus.lock().ok().and_then(|f| *f);
            with_doc(&doc, |d| select::matches_with(d, id, &selector, focused)).flatten().unwrap_or(false)
        });
    }
    {
        let (doc, focus) = (Arc::clone(&state.doc), Arc::clone(&state.focus));
        install_fn!(ctx, api, "queryAll", move |root: usize, selector: String| {
            let focused = focus.lock().ok().and_then(|f| *f);
            with_doc(&doc, |d| select::query_all_with(d, root, &selector, focused)).flatten().unwrap_or_default()
        });
    }
    {
        let (doc, focus) = (Arc::clone(&state.doc), Arc::clone(&state.focus));
        install_fn!(ctx, api, "query", move |root: usize, selector: String| {
            let focused = focus.lock().ok().and_then(|f| *f);
            with_doc(&doc, |d| select::query_all_with(d, root, &selector, focused))
                .flatten()
                .and_then(|all| all.into_iter().next())
        });
    }
    Ok(())
}

/// Mutating primitives.
fn put_mutations<'js>(
    ctx: &Ctx<'js>,
    api: &Object<'js>,
    doc: &Arc<Mutex<Document>>,
) -> Result<(), JsError> {
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "setAttr", move |id: usize, name: String, value: String| {
            let _ = with_doc_mut(&doc, |d| d.set_attr(id, &name.to_ascii_lowercase(), &value));
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "removeAttr", move |id: usize, name: String| {
            let _ = with_doc_mut(&doc, |d| d.remove_attr(id, &name.to_ascii_lowercase()));
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "setTextContent", move |id: usize, text: String| {
            let _ = with_doc_mut(&doc, |d| d.set_text_content(id, &text));
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "setTitle", move |value: String| {
            let _ = with_doc_mut(&doc, |d| d.set_title(&value));
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "createElement", move |tag: String| {
            with_doc_mut(&doc, |d| d.create_element(&tag.to_ascii_lowercase()))
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "createElementNS", move |namespace: String, tag: String| {
            with_doc_mut(&doc, |d| {
                if namespace == "http://www.w3.org/1999/xhtml" {
                    d.create_element(&tag.to_ascii_lowercase())
                } else {
                    d.create_element_ns(&namespace, &tag)
                }
            })
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "createTextNode", move |text: String| {
            with_doc_mut(&doc, |d| d.create_text(text))
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "createComment", move |text: String| {
            with_doc_mut(&doc, |d| d.create_comment(text))
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "createFragment", move || with_doc_mut(&doc, |d| d.create_fragment()));
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "cloneNode", move |id: usize, deep: bool| {
            with_doc_mut(&doc, |d| d.clone_node(id, deep))
        });
    }
    {
        // Returns false when the insertion would put a node inside itself,
        // which the prelude turns into a HierarchyRequestError. A fragment
        // contributes its children, not itself.
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "insertChild", move |parent: usize, child: usize, reference: Option<usize>| {
            with_doc_mut(&doc, |d| {
                if d.is_inclusive_ancestor(child, parent) {
                    return false;
                }
                if matches!(d.node(child).data, NodeData::Fragment) {
                    for grandchild in d.children(child).to_vec() {
                        d.insert_child(parent, grandchild, reference);
                    }
                } else {
                    d.insert_child(parent, child, reference);
                }
                true
            })
            .unwrap_or(false)
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "setControlValue", move |id: usize, value: Option<String>| {
            let _ = with_doc_mut(&doc, |d| d.control_mut(id).value = value);
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "setControlChecked", move |id: usize, checked: Option<bool>| {
            let _ = with_doc_mut(&doc, |d| d.control_mut(id).checked = checked);
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "setControlSelected", move |id: usize, selected: Option<bool>| {
            let _ = with_doc_mut(&doc, |d| d.control_mut(id).selected = selected);
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "removeNode", move |id: usize| {
            let _ = with_doc_mut(&doc, |d| d.detach(id));
        });
    }
    {
        // Parses `html` as `innerHTML` of `context` would, into a new
        // fragment in the live document.
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "parseFragment", move |context: usize, html: String| {
            with_doc_mut(&doc, |d| {
                let tag = d.tag(context).unwrap_or("body").to_string();
                let (scratch, container) = parse_fragment(&html, &tag);
                let fragment = d.create_fragment();
                for child in scratch.children(container).to_vec() {
                    let imported = d.import(&scratch, child);
                    d.append_child(fragment, imported);
                }
                fragment
            })
        });
    }
    {
        let doc = Arc::clone(doc);
        install_fn!(ctx, api, "write", move |text: String| {
            let _ = with_doc_mut(&doc, |d| d.document_write(&text));
        });
    }

    Ok(())
}

fn put_geometry<'js>(ctx: &Ctx<'js>, api: &Object<'js>, state: &Arc<DomState>) -> Result<(), JsError> {
    // Boxes and hit testing share one layout (layout.rs), so the box an
    // element reports is where a click on it lands. An element without a
    // box (not rendered, or not in the document) reports zeroes.
    {
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "rect", move |id: usize| {
            with_doc(&doc, |d| {
                crate::layout::document_layout(d)
                    .client_rect(d, id)
                    .map(|r| vec![r.x, r.y, r.width, r.height])
                    .unwrap_or_else(|| vec![0.0; 4])
            })
            .unwrap_or_else(|| vec![0.0; 4])
        });
    }
    {
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "hitTest", move |x: f64, y: f64| {
            with_doc(&doc, |d| {
                let styles = crate::style::document_styles(d);
                crate::layout::document_layout(d).hits(d, &styles, x, y)
            })
            .unwrap_or_default()
        });
    }

    // Scrolling (scroll.rs). The document node's id stands for the viewport.
    {
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "scrollOffset", move |id: usize| {
            with_doc(&doc, |d| d.scroll_offset(id)).map(|(x, y)| vec![x, y]).unwrap_or_else(|| vec![0.0, 0.0])
        });
    }
    {
        // Returns whether it moved.
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "scrollTo", move |id: usize, x: f64, y: f64| {
            with_doc_mut(&doc, |d| crate::scroll::scroll_to(d, id, (x, y))).unwrap_or(false)
        });
    }
    {
        // [scrollWidth, scrollHeight, clientWidth, clientHeight].
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "scrollSizes", move |id: usize| {
            with_doc(&doc, |d| {
                let layout = crate::layout::document_layout(d);
                let styles = crate::style::document_styles(d);
                crate::scroll::sizes(d, &layout, &styles, id)
                    .map(|s| vec![s.scroll_width, s.scroll_height, s.client_width, s.client_height])
            })
            .flatten()
            .unwrap_or_else(|| vec![0.0; 4])
        });
    }
    {
        // Returns the containers that moved.
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "scrollIntoView", move |id: usize, block: String, inline: String| {
            use crate::scroll::Align;
            let (block, inline) = (Align::parse(&block, Align::Start), Align::parse(&inline, Align::Nearest));
            with_doc_mut(&doc, |d| crate::scroll::into_view(d, id, block, inline)).unwrap_or_default()
        });
    }
    {
        // The wheel at a viewport point; returns what moved.
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "wheel", move |x: f64, y: f64, dx: f64, dy: f64| {
            with_doc_mut(&doc, |d| crate::scroll::wheel(d, x, y, dx, dy)).flatten()
        });
    }

    {
        // The cascade's answer for one property, or its initial value.
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "computedValue", move |id: usize, name: String| {
            with_doc(&doc, |d| {
                crate::style::document_styles(d)
                    .get(id)
                    .map(|s| s.get(&name).to_string())
                    .unwrap_or_else(|| crate::style::initial_value(&name).to_string())
            })
            .unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "computedNames", move |id: usize| {
            with_doc(&doc, |d| {
                let styles = crate::style::document_styles(d);
                let mut names: Vec<String> =
                    styles.get(id).map(|s| s.properties().map(|(n, _)| n.to_string()).collect()).unwrap_or_default();
                names.sort();
                names
            })
            .unwrap_or_default()
        });
    }
    {
        // `url` is where the sheet came from; its `url()`s are relative to it.
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "setStylesheet", move |id: usize, text: String, url: String| {
            let text = crate::css::absolutize_urls(&text, &url);
            let _ = with_doc_mut(&doc, |d| d.set_stylesheet(id, text));
        });
    }
    {
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "setCssom", move |id: usize, text: String| {
            let _ = with_doc_mut(&doc, |d| d.set_cssom(id, text));
        });
    }
    {
        let viewport = Arc::clone(&state.viewport);
        install_fn!(ctx, api, "viewportWidth", move || {
            viewport.lock().map(|v| v.0).unwrap_or(800.0)
        });
    }
    {
        let viewport = Arc::clone(&state.viewport);
        install_fn!(ctx, api, "viewportHeight", move || {
            viewport.lock().map(|v| v.1).unwrap_or(600.0)
        });
    }
    {
        let focus = Arc::clone(&state.focus);
        install_fn!(ctx, api, "setFocus", move |id: Option<usize>| {
            if let Ok(mut slot) = focus.lock() {
                *slot = id;
            }
        });
    }
    {
        let focus = Arc::clone(&state.focus);
        install_fn!(ctx, api, "activeElement", move || focus.lock().ok().and_then(|f| *f));
    }
    Ok(())
}

fn put_navigation<'js>(ctx: &Ctx<'js>, api: &Object<'js>, state: &Arc<DomState>) -> Result<(), JsError> {
    {
        let navigation = Arc::clone(&state.requested_navigation);
        install_fn!(ctx, api, "navigate", move |url: String| {
            if let Ok(mut slot) = navigation.lock() {
                *slot = Some(Navigation::get(&url));
            }
        });
    }
    {
        let navigation = Arc::clone(&state.requested_navigation);
        install_fn!(ctx, api, "submit", move |method: String, url: String, body: String, content_type: Option<String>| {
            if let Ok(mut slot) = navigation.lock() {
                *slot = Some(Navigation { url, method, body: body.into_bytes(), content_type, reload: false });
            }
        });
    }
    {
        let navigation = Arc::clone(&state.requested_navigation);
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "reload", move || {
            let url = with_doc(&doc, |d| d.url().to_string()).unwrap_or_default();
            if let Ok(mut slot) = navigation.lock() {
                *slot = Some(Navigation { reload: true, ..Navigation::get(&url) });
            }
        });
    }
    {
        // A same-document URL change. Repeated changes before the engine
        // looks keep "replace" only if every one of them replaced.
        let doc = Arc::clone(&state.doc);
        let change = Arc::clone(&state.url_change);
        install_fn!(ctx, api, "setUrl", move |url: String, replace: bool| {
            let _ = with_doc_mut(&doc, |d| d.set_url(&url));
            if let Ok(mut slot) = change.lock() {
                *slot = Some(slot.unwrap_or(true) && replace);
            }
        });
    }
    {
        let ready = Arc::clone(&state.ready_state);
        install_fn!(ctx, api, "markReady", move |value: String| {
            if let Ok(mut slot) = ready.lock() {
                *slot = value;
            }
        });
    }
    {
        let agent = Arc::clone(&state.user_agent);
        install_fn!(ctx, api, "userAgent", move || {
            agent.lock().map(|u| u.clone()).unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(&state.doc);
        let jar = Arc::clone(state.client.cookies());
        install_fn!(ctx, api, "cookie", move || {
            let Some(Ok(url)) = with_doc(&doc, |d| fetch::Url::parse(d.url())) else {
                return String::new();
            };
            jar.lock().map(|j| j.script_string(&url, cookies::now())).unwrap_or_default()
        });
    }
    {
        let doc = Arc::clone(&state.doc);
        let jar = Arc::clone(state.client.cookies());
        install_fn!(ctx, api, "setCookie", move |value: String| {
            let Some(Ok(url)) = with_doc(&doc, |d| fetch::Url::parse(d.url())) else { return };
            if let Ok(mut j) = jar.lock() {
                j.store(&url, &value, Source::Script, cookies::now());
            }
        });
    }
    {
        let steps = Arc::clone(&state.requested_history);
        install_fn!(ctx, api, "historyGo", move |delta: i32| {
            if let Ok(mut s) = steps.lock() {
                *s += delta;
            }
        });
    }
    Ok(())
}

/// A request from script: `url` against the document's, and headers from
/// the flat `[name, value, name, value]` list the prelude passes.
fn http_request(
    doc: &Arc<Mutex<Document>>,
    method: String,
    url: String,
    headers: Vec<String>,
    body: String,
    kind: String,
) -> fetch::Request {
    let base = with_doc(doc, |d| d.url().to_string()).unwrap_or_default();
    let url = resolve_url(&base, &url).unwrap_or(url);
    let mut headers: Vec<(String, String)> = headers
        .chunks(2)
        .filter_map(|pair| match pair {
            [name, value] => Some((name.clone(), value.clone())),
            _ => None,
        })
        .collect();
    let kind = ResourceType::parse(&kind);
    // What `fetch` and XHR send when the page sets none, rather than the
    // client's default, which is a document's.
    if matches!(kind, ResourceType::Fetch | ResourceType::Xhr) && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept")) {
        headers.push(("Accept".into(), "*/*".into()));
    }
    fetch::Request { method, url, headers, body: body.into_bytes(), kind }
}

/// A response as the prelude reads it: `[status, statusText, finalUrl,
/// rawHeaders, body]`. A status of 0 means the request failed, and
/// statusText says why.
fn http_answer(result: Result<fetch::Response, fetch::FetchError>, url: String) -> Vec<String> {
    match result {
        Ok(response) => {
            let raw_headers = response.headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect::<String>();
            vec![
                response.status.to_string(),
                response.reason,
                response.url,
                raw_headers,
                String::from_utf8_lossy(&response.body).into_owned(),
            ]
        }
        Err(e) => vec!["0".into(), e.to_string(), url, String::new(), String::new()],
    }
}

/// The rest of the web platform that needs the host: running script text,
/// the network, URLs, randomness, base64 and storage.
fn put_platform<'js>(ctx: &Ctx<'js>, api: &Object<'js>, state: &Arc<DomState>) -> Result<(), JsError> {
    // Classic script text, evaluated as a global script. Not `eval` from
    // JavaScript: indirect eval would scope top-level `let` and `const` to the
    // one script, where a browser shares them between every script of the
    // page. Returns nothing, or `[message, thrown]`: the error as the engine
    // reports it, and the value thrown, for the console and
    // `Runtime.exceptionThrown`.
    install_fn!(ctx, api, "runScript", move |ctx: Ctx<'js>, source: String, name: String| -> Option<Vec<Value<'js>>> {
        let mut options = EvalOptions::default();
        options.strict = false;
        options.filename = Some(name);
        match ctx.eval_with_options::<Value, _>(source, options).catch(&ctx) {
            Ok(_) => None,
            Err(caught) => {
                let thrown = match &caught {
                    rquickjs::CaughtError::Exception(e) => Some(e.as_value().clone()),
                    rquickjs::CaughtError::Value(v) => Some(v.clone()),
                    rquickjs::CaughtError::Error(_) => None,
                };
                let message = rquickjs::String::from_str(ctx.clone(), &describe_caught(&ctx, caught)).ok()?.into_value();
                let thrown = thrown.unwrap_or_else(|| message.clone());
                Some(vec![message, thrown])
            }
        }
    });

    {
        // One request through the engine's client, synchronously: a
        // synchronous XHR, and parser scripts, which the engine has usually
        // preloaded. Returns what `http_answer` makes.
        let doc = Arc::clone(&state.doc);
        let client = state.client.clone();
        install_fn!(ctx, api, "http", move |method: String, url: String, headers: Vec<String>, body: String, kind: String| {
            let request = http_request(&doc, method, url, headers, body, kind);
            let url = request.url.clone();
            http_answer(client.send(request), url)
        });
    }
    {
        // The same request, started now on its own thread, for `fetch`, an
        // asynchronous XHR, or a script or stylesheet that script inserted.
        // The prelude collects it with `httpFinish` from the task that
        // delivers it, so the page sees events in the same order as before
        // and the download overlaps whatever runs in between. Returns a
        // handle.
        let doc = Arc::clone(&state.doc);
        let client = state.client.clone();
        let requests = Arc::clone(&state.requests);
        let next = Arc::new(Mutex::new(0u32));
        install_fn!(ctx, api, "httpStart", move |method: String, url: String, headers: Vec<String>, body: String, kind: String| {
            let request = http_request(&doc, method, url, headers, body, kind);
            let url = request.url.clone();
            let pending = client.start(request);
            let mut next = next.lock().unwrap_or_else(|p| p.into_inner());
            *next += 1;
            if let Ok(mut requests) = requests.lock() {
                requests.insert(*next, (pending, url));
            }
            *next
        });
    }
    {
        let requests = Arc::clone(&state.requests);
        install_fn!(ctx, api, "httpFinish", move |handle: u32| {
            let started = requests.lock().ok().and_then(|mut r| r.remove(&handle));
            match started {
                Some((pending, url)) => http_answer(pending.wait(), url),
                None => http_answer(Err(fetch::FetchError::Io("no such request".into())), String::new()),
            }
        });
    }
    {
        // An aborted request: its answer is not wanted. The thread finishes
        // on its own.
        let requests = Arc::clone(&state.requests);
        install_fn!(ctx, api, "httpDrop", move |handle: u32| {
            if let Ok(mut requests) = requests.lock() {
                requests.remove(&handle);
            }
        });
    }

    install_fn!(ctx, api, "resolveUrl", |base: String, relative: String| resolve_url(&base, &relative));

    // A module script. Inline, its source is declared under `name`, unique
    // per script, with `url`, the document's, as `import.meta.url`; external,
    // `url` is loaded through the page's loader (modules.rs), with whatever
    // it imports. Either way the result is the promise of its evaluation,
    // which top-level `await` can keep pending. Syntax and loading errors
    // are thrown.
    install_fn!(
        ctx,
        api,
        "runModule",
        |ctx: Ctx<'js>, source: Option<String>, name: String, url: String| -> rquickjs::Result<rquickjs::Promise<'js>> {
            match source {
                Some(source) => {
                    let (_, promise) = crate::modules::declare(&ctx, &name, &url, source)?.eval()?;
                    Ok(promise)
                }
                None => rquickjs::Module::import(&ctx, url),
            }
        }
    );
    {
        // A `<script type=importmap>`'s JSON. Returns the error, or nothing.
        let state = Arc::clone(state);
        install_fn!(ctx, api, "addImportMap", move |json: String| state.add_import_map(&json).err());
    }

    // Images, through the target's store. The store is taken out of the
    // document before waiting: the lock must not be held while a download
    // finishes.
    let images = {
        let doc = Arc::clone(&state.doc);
        move || with_doc(&doc, |d| d.images().cloned()).flatten()
    };
    {
        let doc = Arc::clone(&state.doc);
        install_fn!(ctx, api, "imageUrl", move |id: usize| with_doc(&doc, |d| d.image_url(id)).flatten());
    }
    {
        let images = images.clone();
        install_fn!(ctx, api, "imageRequest", move |url: String| {
            if let Some(store) = images() {
                store.request(&url);
            }
        });
    }
    {
        // [0] on its way, [1] failed, [2, width, height] arrived.
        let images = images.clone();
        install_fn!(ctx, api, "imageState", move |url: String| -> Vec<u32> {
            match images().and_then(|store| store.state(&url)) {
                None => vec![0],
                Some(None) => vec![1],
                Some(Some((w, h))) => vec![2, w, h],
            }
        });
    }
    {
        // [width, height], or nothing when it failed, after waiting for it.
        install_fn!(ctx, api, "imageWait", move |url: String| -> Vec<u32> {
            images().and_then(|store| store.wait(&url)).map(|(w, h)| vec![w, h]).unwrap_or_default()
        });
    }

    // [href, protocol, username, password, host, hostname, port, pathname,
    //  search, hash, origin], or nothing when the input is not a URL.
    install_fn!(ctx, api, "urlParts", |input: String, base: Option<String>| {
        let parsed = match base {
            Some(base) => url::Url::parse(&base).and_then(|b| b.join(&input)),
            None => url::Url::parse(&input),
        };
        let url = parsed.ok()?;
        let host = url.host_str().unwrap_or("").to_string();
        let port = url.port().map(|p| p.to_string()).unwrap_or_default();
        Some(vec![
            url.as_str().to_string(),
            format!("{}:", url.scheme()),
            url.username().to_string(),
            url.password().unwrap_or("").to_string(),
            if port.is_empty() { host.clone() } else { format!("{host}:{port}") },
            host,
            port,
            url.path().to_string(),
            url.query().map(|q| format!("?{q}")).filter(|q| q != "?").unwrap_or_default(),
            url.fragment().map(|f| format!("#{f}")).filter(|f| f != "#").unwrap_or_default(),
            url.origin().ascii_serialization(),
        ])
    });

    install_fn!(ctx, api, "randomBytes", |length: usize| {
        let mut bytes = vec![0u8; length.min(65_536)];
        let _ = rustls::crypto::ring::default_provider().secure_random.fill(&mut bytes);
        bytes
    });

    // `btoa` and `atob` work on "binary strings", one byte per character.
    install_fn!(ctx, api, "btoa", |input: String| {
        use base64::Engine as _;
        let bytes: Option<Vec<u8>> = input.chars().map(|c| u8::try_from(c as u32).ok()).collect();
        bytes.map(|b| base64::engine::general_purpose::STANDARD.encode(b))
    });
    install_fn!(ctx, api, "atob", |input: String| {
        use base64::Engine as _;
        let cleaned: String = input.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let cleaned = cleaned.trim_end_matches('=');
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(cleaned)
            .ok()
            .map(|bytes| bytes.into_iter().map(char::from).collect::<String>())
    });

    {
        let doc = Arc::clone(&state.doc);
        let storage = Arc::clone(&state.storage);
        install_fn!(ctx, api, "storage", move |kind: String, op: String, key: Option<String>, value: Option<String>| {
            let origin = with_doc(&doc, |d| document_origin(d.url())).unwrap_or_default();
            let mut all = storage.lock().unwrap_or_else(|p| p.into_inner());
            let area = all.entry(format!("{kind} {origin}")).or_default();
            let key = key.unwrap_or_default();
            match op.as_str() {
                "get" => area.iter().find(|(k, _)| *k == key).map(|(_, v)| vec![v.clone()]),
                "set" => {
                    let value = value.unwrap_or_default();
                    match area.iter_mut().find(|(k, _)| *k == key) {
                        Some(entry) => entry.1 = value,
                        None => area.push((key, value)),
                    }
                    None
                }
                "remove" => {
                    area.retain(|(k, _)| *k != key);
                    None
                }
                "clear" => {
                    area.clear();
                    None
                }
                "keys" => Some(area.iter().map(|(k, _)| k.clone()).collect()),
                _ => None,
            }
        });
    }
    Ok(())
}

/// A thrown value as text. An `Error` brings its message and stack; anything
/// else thrown (`throw {code: 1}` is common in bundles) is shown as JSON,
/// because rquickjs would only print its address.
fn describe_caught<'js>(ctx: &Ctx<'js>, caught: rquickjs::CaughtError<'js>) -> String {
    match caught {
        rquickjs::CaughtError::Value(value) => {
            let field = |key: &str| -> Option<String> {
                value.as_object()?.get::<_, Option<rquickjs::Coerced<String>>>(key).ok().flatten().map(|c| c.0)
            };
            let class = value
                .as_object()
                .and_then(|o| o.get::<_, Option<Object>>("constructor").ok().flatten())
                .and_then(|c| c.get::<_, Option<String>>("name").ok().flatten());
            let json = ctx.json_stringify(value.clone()).ok().flatten().and_then(|s| s.to_string().ok());
            let mut text = format!(
                "uncaught non-Error {}: {}",
                class.unwrap_or_else(|| value.type_name().to_string()),
                json.unwrap_or_default()
            );
            for key in ["name", "message", "stack"] {
                if let Some(v) = field(key) {
                    text.push_str(&format!("\n  {key}: {v}"));
                }
            }
            text
        }
        other => other.to_string(),
    }
}

/// `relative` against `base`, as a browser resolves `href` and `src`.
fn resolve_url(base: &str, relative: &str) -> Option<String> {
    match url::Url::parse(base) {
        Ok(base) => base.join(relative).ok().map(String::from),
        Err(_) => url::Url::parse(relative).ok().map(String::from),
    }
}

fn document_origin(url: &str) -> String {
    url::Url::parse(url).map(|u| u.origin().ascii_serialization()).unwrap_or_else(|_| "null".into())
}

fn node_type_of(doc: &Document, id: NodeId) -> u32 {
    match &doc.node(id).data {
        NodeData::Element { .. } => 1,
        NodeData::Text => 3,
        NodeData::Comment => 8,
        NodeData::Document => 9,
        NodeData::Doctype { .. } => 10,
        NodeData::Fragment => 11,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js::{JsRuntime, JsValue};

    fn host(html: &str) -> (JsRuntime, Arc<DomState>) {
        let doc = Arc::new(Mutex::new(crate::parse::parse_html(html)));
        if let Ok(mut d) = doc.lock() {
            d.set_url("http://example.test/page");
        }
        let client = fetch::Client::new(Arc::new(Mutex::new(crate::cookies::CookieJar::new())));
        let state = Arc::new(DomState::new(doc, client, (1024.0, 768.0), "test-agent"));
        let js = JsRuntime::new().expect("runtime");
        js.with(|ctx| install(&ctx, &state)).expect("install bindings");
        (js, state)
    }

    fn text(js: &JsRuntime, expression: &str) -> String {
        match js.evaluate(&format!("String({expression})")).expect(expression) {
            JsValue::String(s) => s,
            other => other.type_name().to_string(),
        }
    }

    #[test]
    fn document_identity_is_stable() {
        // Clients compare elements with `===`, so the wrapper cache has to hold.
        let (js, _) = host("<body><p id='p'>x</p></body>");
        assert_eq!(text(&js, "document.body === document.body"), "true");
        assert_eq!(
            text(&js, "document.getElementById('p') === document.querySelector('#p')"),
            "true"
        );
    }

    #[test]
    fn reads_the_document_through_javascript() {
        let (js, _) = host(
            "<html><head><title>Hi</title></head><body><h1>Heading</h1><a href='/x'>go</a></body></html>",
        );
        assert_eq!(text(&js, "document.title"), "Hi");
        assert_eq!(text(&js, "document.querySelector('h1').textContent"), "Heading");
        assert_eq!(text(&js, "document.querySelector('a').getAttribute('href')"), "/x");
        assert_eq!(text(&js, "document.querySelector('a').tagName"), "A");
        assert_eq!(text(&js, "document.querySelector('a').nodeName"), "A");
        assert_eq!(text(&js, "document.querySelector('a').nodeType"), "1");
        assert_eq!(text(&js, "document.querySelectorAll('a').length"), "1");
        assert_eq!(text(&js, "document.querySelector('h1').parentElement.tagName"), "BODY");
        assert_eq!(text(&js, "document.getElementsByTagName('a').length"), "1");
        assert_eq!(text(&js, "document.querySelector('a').matches('a[href]')"), "true");
        assert_eq!(text(&js, "document.querySelector('h1').closest('body').tagName"), "BODY");
        assert_eq!(text(&js, "document.body.contains(document.querySelector('h1'))"), "true");
        assert_eq!(text(&js, "document.URL"), "http://example.test/page");
        // The engine drives the lifecycle; without it the page is still loading.
        assert_eq!(text(&js, "document.readyState"), "loading");
        js.evaluate("__documentParsed(); __fireDOMContentLoaded(); __fireLoad()").unwrap();
        assert_eq!(text(&js, "document.readyState"), "complete");
        assert_eq!(text(&js, "window.innerWidth"), "1024");
        assert_eq!(text(&js, "navigator.userAgent"), "test-agent");
    }

    #[test]
    fn missing_lookups_return_null_not_an_error() {
        let (js, _) = host("<body><p>x</p></body>");
        assert_eq!(text(&js, "document.querySelector('.absent')"), "null");
        assert_eq!(text(&js, "document.getElementById('nope')"), "null");
        assert_eq!(text(&js, "document.querySelector('p').closest('.absent')"), "null");
        assert_eq!(text(&js, "document.querySelector('p').parentNode.parentNode.tagName"), "HTML");
        // Node 0 is the document, not "none": the chain ends at the document,
        // and only above it at null.
        assert_eq!(text(&js, "document.querySelector('p').parentNode.parentNode.parentNode === document"), "true");
        assert_eq!(text(&js, "String(document.parentNode)"), "null");
    }

    #[test]
    fn geometry_comes_from_the_layout() {
        let (js, _) = host("<body><p id='p'>x</p></body>");
        // 800 wide, less the body's 8px margins.
        assert_eq!(text(&js, "document.body.getBoundingClientRect().width"), "784");
        assert_eq!(text(&js, "document.getElementById('p').getBoundingClientRect().height > 0"), "true");
        assert_eq!(text(&js, "document.getElementById('p').offsetParent === document.body"), "true");
        assert_eq!(
            text(&js, "Object.keys(document.body.getBoundingClientRect().toJSON()).join()"),
            "x,y,width,height,top,right,bottom,left"
        );
    }

    #[test]
    fn computed_style_comes_from_the_cascade() {
        let (js, _) = host(
            "<head><style>.card { display: flex; color: rgb(1, 2, 3) } .card > .off { display: none }</style></head>\
             <body><a href='/'>link</a><div hidden id='h'>x</div>\
             <p id='c' style='display:flex;cursor:crosshair'>y</p>\
             <div class='card' id='card'><span id='in'>t</span><b class='off' id='off'>o</b></div></body>",
        );
        let style = |id: &str, prop: &str| text(&js, &format!("getComputedStyle(document.getElementById('{id}')).{prop}"));
        assert_eq!(text(&js, "getComputedStyle(document.querySelector('a')).cursor"), "pointer");
        assert_eq!(style("h", "display"), "none");
        assert_eq!(style("c", "display"), "flex");
        assert_eq!(style("c", "cursor"), "crosshair");
        assert_eq!(style("card", "display"), "flex");
        assert_eq!(style("in", "color"), "rgb(1, 2, 3)", "inherited");
        assert_eq!(style("off", "display"), "none");
        assert_eq!(text(&js, "getComputedStyle(document.body).borderWidth"), "0px");
        assert_eq!(text(&js, "getComputedStyle(document.body).getPropertyValue('margin-top')"), "8px");
    }

    #[test]
    fn script_added_rules_join_the_cascade_and_hide_from_the_tree() {
        let (js, state) = host("<body><button id='b'>shown</button><button id='g' class='gone'>gone</button></body>");
        js.evaluate(
            "const style = document.createElement('style');\
             document.head.appendChild(style);\
             style.sheet.insertRule('.gone { display: none }', 0);",
        )
        .unwrap();
        assert_eq!(text(&js, "getComputedStyle(document.getElementById('g')).display"), "none");
        let tree = crate::ax::to_cdp(&crate::ax::build(&state.doc.lock().unwrap())).to_string();
        assert!(tree.contains("shown") && !tree.contains("gone"), "{tree}");
    }

    #[test]
    fn the_clients_click_scan_expression_runs_without_throwing() {
        // This is the expression agent-browser sends to find clickable
        // candidates, and it is the reason a JavaScript engine is required at
        // all. Running it to completion is the milestone.
        let (js, _) = host(
            "<body><div onclick='x()' id='card'><span>Card</span></div><a href='/x'>link</a></body>",
        );
        let source = r#"
            (function() {
                var results = [];
                if (!document.body) return results;
                var interactiveRoles = { 'button':1, 'link':1, 'textbox':1, 'checkbox':1, 'radio':1 };
                var interactiveTags = { 'a':1, 'button':1, 'input':1, 'select':1, 'textarea':1 };
                var allElements = document.body.querySelectorAll('*');
                for (var i = 0; i < allElements.length; i++) {
                    var el = allElements[i];
                    if (el.closest && el.closest('[hidden], [aria-hidden="true"]')) continue;
                    var tagName = el.tagName.toLowerCase();
                    if (interactiveTags[tagName]) continue;
                    var role = el.getAttribute('role');
                    if (role && interactiveRoles[role.toLowerCase()]) continue;
                    var computedStyle = getComputedStyle(el);
                    var hasCursorPointer = computedStyle.cursor === 'pointer';
                    var hasOnClick = el.hasAttribute('onclick') || el.onclick !== null;
                    var tabIndex = el.getAttribute('tabindex');
                    var hasTabIndex = tabIndex !== null && tabIndex !== '-1';
                    var ce = el.getAttribute('contenteditable');
                    var isEditable = ce === '' || ce === 'true';
                    if (!hasCursorPointer && !hasOnClick && !hasTabIndex && !isEditable) continue;
                    var text = (el.textContent || '').trim().slice(0, 100);
                    var rect = el.getBoundingClientRect();
                    if (rect.width === 0 || rect.height === 0) continue;
                    results.push({ text: text, tagName: tagName, hasOnClick: hasOnClick });
                }
                return results;
            })()
        "#;
        assert_eq!(
            js.evaluate(source).expect("the scan expression must not throw").type_name(),
            "object"
        );
    }

    #[test]
    fn the_clients_click_point_expression_runs_without_throwing() {
        let (js, _) = host("<body><a href='/x'>link</a></body>");
        let source = r#"
            (() => {
                const el = document.querySelector("a");
                if (!el) return null;
                const inView = (r) => r.width > 0 && r.height > 0 &&
                    r.bottom > 0 && r.right > 0 &&
                    r.top < (window.innerHeight || document.documentElement.clientHeight) &&
                    r.left < (window.innerWidth || document.documentElement.clientWidth);
                let rect = el.getBoundingClientRect();
                if (!inView(rect)) {
                    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
                    rect = el.getBoundingClientRect();
                }
                const x = rect.x + rect.width / 2;
                const y = rect.y + rect.height / 2;
                const blockerAt = (doc, el, x, y) => {
                    let hit = doc.elementFromPoint ? doc.elementFromPoint(x, y) : null;
                    if (!hit || hit === el) return null;
                    let n = hit;
                    while (n) { if (n === el) return null; n = n.parentNode || n.host || null; }
                    return hit.tagName.toLowerCase();
                };
                return { x: x, y: y, blocker: blockerAt(document, el, x, y) };
            })()
        "#;
        assert_eq!(
            js.evaluate(source).expect("the click point expression must not throw").type_name(),
            "object"
        );
    }

    #[test]
    fn page_script_can_mutate_the_document_and_the_engine_sees_it() {
        let (js, state) = host("<body><div id='root'></div></body>");
        js.evaluate(
            "const root = document.getElementById('root');\
             const heading = document.createElement('h2');\
             heading.setAttribute('id', 'added');\
             heading.appendChild(document.createTextNode('injected'));\
             root.appendChild(heading);",
        )
        .expect("mutation");

        // The mutation landed in the engine's arena, not in a copy.
        let doc = state.doc.lock().unwrap();
        let added = doc.find_id(doc.document_id(), "added").expect("the new element exists");
        assert!(doc.is_tag(added, "h2"));
        assert_eq!(doc.text_content(added), "injected");
    }

    #[test]
    fn set_attribute_is_visible_from_both_sides() {
        let (js, state) = host("<body><p id='p'>x</p></body>");
        js.evaluate("document.getElementById('p').setAttribute('data-state', 'ready')").unwrap();
        let doc = state.doc.lock().unwrap();
        let p = doc.find_id(doc.document_id(), "p").unwrap();
        assert_eq!(doc.attr(p, "data-state"), Some("ready"));
    }

    #[test]
    fn assigning_text_content_replaces_the_children() {
        let (js, state) = host("<body><div id='d'><b>old</b> text</div></body>");
        js.evaluate("document.getElementById('d').textContent = 'new'").unwrap();
        {
            let doc = state.doc.lock().unwrap();
            let d = doc.find_id(doc.document_id(), "d").unwrap();
            assert_eq!(doc.text_content(d), "new");
            assert_eq!(doc.children(d).len(), 1);
        }
        js.evaluate("document.getElementById('d').innerText = null").unwrap();
        assert_eq!(text(&js, "document.getElementById('d').childNodes.length"), "0");
    }

    #[test]
    fn reflected_properties_write_their_attribute() {
        let (js, state) = host("<body><p id='p'>x</p></body>");
        js.evaluate(
            "const p = document.getElementById('p');\
             p.className = 'note';\
             p.title = 'tip';\
             p.hidden = true;",
        )
        .unwrap();
        let doc = state.doc.lock().unwrap();
        let p = doc.find_id(doc.document_id(), "p").unwrap();
        assert_eq!(doc.attr(p, "class"), Some("note"));
        assert_eq!(doc.attr(p, "title"), Some("tip"));
        assert!(doc.has_attr(p, "hidden"));
    }

    #[test]
    fn nodes_name_their_interface() {
        let (js, _) = host("<body><p id='p'>x</p></body>");
        assert_eq!(
            text(&js, "Object.prototype.toString.call(document.getElementById('p'))"),
            "[object HTMLParagraphElement]"
        );
        assert_eq!(text(&js, "Object.prototype.toString.call(document)"), "[object HTMLDocument]");
        assert_eq!(
            js.evaluate("document.getElementById('p').firstChild").unwrap(),
            JsValue::Object { class: "Text".into(), is_function: false }
        );
    }

    #[test]
    fn assigning_the_title_changes_the_document() {
        let (js, state) = host("<html><head><title>Old</title></head><body>x</body></html>");
        js.evaluate("document.title = 'New'").unwrap();
        assert_eq!(state.doc.lock().unwrap().title().as_deref(), Some("New"));
        js.evaluate("document.title = 'Newest'").unwrap();
        assert_eq!(state.doc.lock().unwrap().title().as_deref(), Some("Newest"));
    }

    #[test]
    fn selectors_behave_like_a_browser() {
        let (js, _) = host("<body><ul><li id='a'>1</li><li id='b'>2</li></ul><input id='i'></body>");
        assert_eq!(text(&js, "document.querySelector('li:last-child').id"), "b");
        assert_eq!(text(&js, "document.querySelectorAll('ul > li:nth-child(odd)').length"), "1");
        assert_eq!(
            text(&js, "(() => { try { document.querySelector('li:'); return 'no'; } catch (e) { return e.name; } })()"),
            "SyntaxError"
        );
        js.evaluate("document.getElementById('i').focus()").unwrap();
        assert_eq!(text(&js, "document.querySelector(':focus').id"), "i");
        assert_eq!(text(&js, "document.body.matches(':focus-within')"), "true");
    }

    #[test]
    fn nodes_are_instances_of_their_interfaces() {
        let (js, _) = host("<body><input id='i'><p>x</p></body>");
        for (expression, expected) in [
            ("document.getElementById('i') instanceof HTMLInputElement", "true"),
            ("document.getElementById('i') instanceof HTMLElement", "true"),
            ("document.body instanceof Node", "true"),
            ("document instanceof Document", "true"),
            ("document.querySelector('p').firstChild instanceof Text", "true"),
            ("window instanceof EventTarget", "true"),
            ("Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value') !== undefined", "true"),
        ] {
            assert_eq!(text(&js, expression), expected, "{expression}");
        }
    }

    #[test]
    fn events_capture_then_bubble_and_can_be_cancelled() {
        let (js, _) = host("<body><div id='outer'><button id='b'>go</button></div></body>");
        js.evaluate(
            "globalThis.log = [];\
             const outer = document.getElementById('outer');\
             outer.addEventListener('click', () => log.push('capture'), true);\
             outer.addEventListener('click', (e) => { log.push('bubble'); e.preventDefault(); });\
             document.getElementById('b').onclick = () => log.push('target');\
             globalThis.result = document.getElementById('b').dispatchEvent(\
               new MouseEvent('click', { bubbles: true, cancelable: true }));",
        )
        .unwrap();
        assert_eq!(text(&js, "log.join(',')"), "capture,target,bubble");
        assert_eq!(text(&js, "result"), "false");
    }

    #[test]
    fn clicking_a_submit_button_submits_the_form_with_its_values() {
        let (js, state) = host(
            "<body><form action='/login' method='post'>\
               <input name='user'><input type='password' name='pass'>\
               <input type='checkbox' name='remember' value='yes'>\
               <button id='go'>Sign in</button></form></body>",
        );
        js.evaluate(
            "document.querySelector('[name=user]').value = 'kim';\
             document.querySelector('[name=pass]').value = 'p&w d';\
             document.querySelector('[name=remember]').click();\
             document.getElementById('go').click();",
        )
        .unwrap();
        let navigation = state.take_requested_navigation().expect("a submission");
        assert_eq!(navigation.method, "POST");
        assert_eq!(navigation.url, "http://example.test/login");
        assert_eq!(navigation.content_type.as_deref(), Some("application/x-www-form-urlencoded"));
        assert_eq!(String::from_utf8(navigation.body).unwrap(), "user=kim&pass=p%26w+d&remember=yes");
    }

    #[test]
    fn a_cancelled_submit_does_not_navigate() {
        let (js, state) = host("<body><form><input name='q' value='x'><button>go</button></form></body>");
        js.evaluate(
            "document.querySelector('form').addEventListener('submit', (e) => e.preventDefault());\
             document.querySelector('button').click();",
        )
        .unwrap();
        assert_eq!(state.take_requested_navigation(), None);
    }

    #[test]
    fn fetch_and_xhr_go_through_the_engine_with_cookies() {
        let (base, requests) = crate::test_server::serve_script(vec![
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nSet-Cookie: sid=1\r\nContent-Length: 11\r\n\r\n{\"ok\":true}".to_string(),
            "HTTP/1.1 201 Created\r\nContent-Length: 4\r\n\r\ndone".to_string(),
        ]);
        let (js, state) = host("<body>x</body>");
        state.doc.lock().unwrap().set_url(&format!("{base}/app"));
        js.evaluate(
            "fetch('/api/me').then((r) => r.json()).then((j) => { globalThis.fetched = j.ok; });\
             const xhr = new XMLHttpRequest();\
             xhr.open('POST', '/api/save');\
             xhr.setRequestHeader('Content-Type', 'application/json');\
             xhr.onload = () => { globalThis.saved = xhr.status + ' ' + xhr.responseText; };\
             globalThis.later = () => xhr.send('{\"a\":1}');",
        )
        .unwrap();
        js.run_tasks(1_000.0, 100);
        assert_eq!(text(&js, "fetched"), "true");
        js.evaluate("later()").unwrap();
        js.run_tasks(1_000.0, 100);
        assert_eq!(text(&js, "saved"), "201 done");
        let _ = requests.recv().unwrap();
        let save = requests.recv().unwrap();
        assert!(save.starts_with("POST /api/save HTTP/1.1\r\n"), "{save}");
        assert!(save.contains("Cookie: sid=1\r\n"), "{save}");
        assert!(save.ends_with("{\"a\":1}"), "{save}");
    }

    #[test]
    fn a_script_inserted_by_script_runs_and_innerhtml_scripts_do_not() {
        let (base, _requests) = crate::test_server::serve_script(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 27\r\n\r\nglobalThis.chunk = 'loaded';".to_string(),
        ]);
        let (js, state) = host("<body><div id='d'></div></body>");
        state.doc.lock().unwrap().set_url(&format!("{base}/"));
        js.evaluate(
            "const inline = document.createElement('script');\
             inline.textContent = 'globalThis.inlineRan = true';\
             document.body.appendChild(inline);\
             document.getElementById('d').innerHTML = '<script>globalThis.fromHtml = true<\\/script>';\
             const external = document.createElement('script');\
             external.src = '/chunk.js';\
             external.onload = () => { globalThis.onloadRan = true; };\
             document.head.appendChild(external);",
        )
        .unwrap();
        assert_eq!(text(&js, "inlineRan"), "true");
        assert_eq!(text(&js, "typeof fromHtml"), "undefined");
        js.run_tasks(1_000.0, 100);
        assert_eq!(text(&js, "chunk"), "loaded");
        assert_eq!(text(&js, "onloadRan"), "true");
    }

    #[test]
    fn push_state_changes_the_url_in_place() {
        let (js, state) = host("<body>x</body>");
        js.evaluate("history.pushState({ step: 2 }, '', '/next?x=1')").unwrap();
        assert_eq!(text(&js, "location.pathname + location.search"), "/next?x=1");
        assert_eq!(text(&js, "history.state.step"), "2");
        assert_eq!(state.take_url_change(), Some(false));
        assert_eq!(state.take_requested_navigation(), None);
    }

    #[test]
    fn storage_is_per_origin_and_reachable_as_properties() {
        let (js, _) = host("<body>x</body>");
        js.evaluate("localStorage.setItem('token', 'abc'); localStorage.other = 'y';").unwrap();
        assert_eq!(text(&js, "localStorage.getItem('token') + localStorage.other + localStorage.length"), "abcy2");
        assert_eq!(text(&js, "sessionStorage.getItem('token')"), "null");
    }

    #[test]
    fn mutation_observers_hear_about_script_changes() {
        let (js, _) = host("<body><ul id='l'></ul></body>");
        js.evaluate(
            "globalThis.seen = [];\
             new MutationObserver((records) => records.forEach((r) => seen.push(r.type + ':' + r.addedNodes.length)))\
               .observe(document.getElementById('l'), { childList: true, attributes: true });\
             document.getElementById('l').append(document.createElement('li'));\
             document.getElementById('l').setAttribute('data-x', '1');",
        )
        .unwrap();
        assert_eq!(text(&js, "seen.join(',')"), "childList:1,attributes:0");
    }

    #[test]
    fn urls_and_encoding_work_like_a_browser() {
        let (js, _) = host("<body>x</body>");
        for (expression, expected) in [
            ("new URL('../a?b=1#c', 'https://x.test/p/q').href", "https://x.test/a?b=1#c"),
            ("new URLSearchParams({ q: 'a b&c' }).toString()", "q=a+b%26c"),
            ("new TextDecoder().decode(new TextEncoder().encode('한글'))", "한글"),
            ("btoa('hi') + atob('aGk=')", "aGk=hi"),
            ("crypto.randomUUID().length", "36"),
            ("matchMedia('(min-width: 800px)').matches", "true"),
        ] {
            assert_eq!(text(&js, expression), expected, "{expression}");
        }
    }

    #[test]
    fn timers_are_queued_and_drained() {
        let (js, _) = host("<body>x</body>");
        js.evaluate("setTimeout(() => { globalThis.ran = true; }, 10)").unwrap();
        assert_eq!(text(&js, "globalThis.ran"), "undefined");
        js.run_tasks(1_000.0, 100);
        assert_eq!(text(&js, "globalThis.ran"), "true");
    }

    #[test]
    fn console_is_reachable_from_page_script() {
        let (js, _) = host("<body>x</body>");
        js.evaluate("console.log('from the page', 1)").unwrap();
        assert_eq!(js.console_lines(), vec!["from the page 1"]);
    }

    #[test]
    fn location_changes_are_recorded_for_the_engine_to_act_on() {
        let (js, state) = host("<body>x</body>");
        js.evaluate("location.href = '/elsewhere'").unwrap();
        assert_eq!(
            state.take_requested_navigation().map(|n| n.url).as_deref(),
            Some("http://example.test/elsewhere")
        );
        assert_eq!(state.take_requested_navigation(), None);
    }

    #[test]
    fn the_page_reads_its_own_password_field_but_the_tree_does_not() {
        // A login page's script has to read the password to submit it, as in
        // any browser. What keeps it from clients is the accessibility tree.
        let (js, state) = host("<body><input type='password' id='pw' aria-label='Password'></body>");
        js.evaluate("document.getElementById('pw').value = 'hunter2'").unwrap();
        assert_eq!(text(&js, "document.getElementById('pw').value"), "hunter2");
        let tree = crate::ax::to_cdp(&crate::ax::build(&state.doc.lock().unwrap()));
        assert!(!tree.to_string().contains("hunter2"), "{tree}");
    }

    #[test]
    fn the_binding_is_send() {
        // `ScriptHost` and the shared state cross a thread boundary; the raw
        // `JsRuntime` does not, and is confined to the script thread.
        fn assert_send<T: Send>() {}
        assert_send::<DomState>();
        assert_send::<crate::js::ScriptHost>();
    }
}

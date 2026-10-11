//! ES modules: `<script type=module>`, `import`, `import()` and import maps.
//!
//! QuickJS links and evaluates modules itself; what it asks the host is how
//! to turn a specifier into a module name ([`PageResolver`]) and a name into
//! source ([`PageLoader`]). Names here are absolute URLs, so a module's own
//! name is the base its imports resolve against, as in a browser.
//!
//! Resolution, as HTML specifies it in outline: a specifier that is a URL,
//! or starts with `/`, `./` or `../`, is joined to the importing module's
//! URL; anything else is a bare specifier, looked up in the page's import
//! map, and an error when the map has no entry. The importer of a classic
//! script's `import()`, or of `Runtime.evaluate`, is not a URL; the
//! document's URL stands in for it.
//!
//! Modules are fetched through the engine's client, so they carry cookies,
//! show in `Network.*`, and come from the preloads when the parser found
//! them. Not implemented: import map scopes and integrity, JSON and CSS
//! modules (`with { type: ... }`), and MIME type checks.

use std::sync::Arc;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Ctx, Error, Module, Result};

use crate::bindings::DomState;
use crate::fetch::Request;
use crate::network::ResourceType;

/// The `imports` of a page's `<script type=importmap>`: specifier, or
/// prefix ending in `/`, to URL.
#[derive(Debug, Default, Clone)]
pub struct ImportMap {
    imports: Vec<(String, String)>,
}

impl ImportMap {
    /// Merges a map's JSON, resolving its addresses against `base`, the
    /// document's URL. Entries that are not strings or not URLs are dropped,
    /// as browsers drop them with a console warning.
    pub fn merge(&mut self, json: &str, base: &str) -> std::result::Result<(), String> {
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("invalid import map: {e}"))?;
        let Some(imports) = value.get("imports").and_then(|v| v.as_object()) else { return Ok(()) };
        let base = url::Url::parse(base).ok();
        for (key, address) in imports {
            let Some(address) = address.as_str() else { continue };
            let resolved = match &base {
                Some(base) => base.join(address).ok(),
                None => url::Url::parse(address).ok(),
            };
            if let Some(resolved) = resolved {
                // A later map does not override an earlier one's entries.
                if !self.imports.iter().any(|(k, _)| k == key) {
                    self.imports.push((key.clone(), resolved.into()));
                }
            }
        }
        Ok(())
    }

    /// The URL `specifier` maps to: an exact entry, else the longest prefix
    /// entry ending in `/`.
    pub fn lookup(&self, specifier: &str) -> Option<String> {
        if let Some((_, url)) = self.imports.iter().find(|(k, _)| k == specifier) {
            return Some(url.clone());
        }
        self.imports
            .iter()
            .filter(|(k, _)| k.ends_with('/') && specifier.starts_with(k.as_str()))
            .max_by_key(|(k, _)| k.len())
            .map(|(k, url)| format!("{url}{}", &specifier[k.len()..]))
    }
}

/// The URL a specifier names, imported from `base`, or why it names none.
pub fn resolve(map: &ImportMap, base: &str, document_url: &str, specifier: &str) -> std::result::Result<String, String> {
    let base = url::Url::parse(base).or_else(|_| url::Url::parse(document_url)).ok();
    let relative = specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../");
    // The map applies to URL-like specifiers too, so a page can redirect one.
    let as_url = if relative {
        base.as_ref().and_then(|b| b.join(specifier).ok()).map(String::from)
    } else {
        url::Url::parse(specifier).ok().map(String::from)
    };
    if let Some(mapped) = map.lookup(as_url.as_deref().unwrap_or(specifier)).or_else(|| map.lookup(specifier)) {
        return Ok(mapped);
    }
    as_url.ok_or_else(|| {
        format!(
            "Failed to resolve module specifier \"{specifier}\". Relative references must start with either \"/\", \"./\", or \"../\"."
        )
    })
}

pub struct PageResolver(pub Arc<DomState>);

impl Resolver for PageResolver {
    fn resolve<'js>(&mut self, _ctx: &Ctx<'js>, base: &str, name: &str, _: Option<ImportAttributes<'js>>) -> Result<String> {
        let document_url = self.0.document_url();
        resolve(&self.0.import_map(), base, &document_url, name).map_err(|message| Error::new_resolving_message(base, name, message))
    }
}

pub struct PageLoader(pub Arc<DomState>);

impl Loader for PageLoader {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str, _: Option<ImportAttributes<'js>>) -> Result<Module<'js, Declared>> {
        let request = Request {
            headers: vec![("Accept".into(), "*/*".into())],
            ..Request::get(name).of_kind(ResourceType::Script)
        };
        let response = self.0.client().send(request).map_err(|e| Error::new_loading_message(name, e.to_string()))?;
        if !(200..300).contains(&response.status) {
            return Err(Error::new_loading_message(name, format!("HTTP {}", response.status)));
        }
        let source = String::from_utf8_lossy(&response.body).into_owned();
        declare(ctx, name, name, source)
    }
}

/// Declares a module named `name`, whose `import.meta.url` is `url`. They
/// differ for an inline module, whose name must be unique but whose URL is
/// the document's.
pub fn declare<'js>(ctx: &Ctx<'js>, name: &str, url: &str, source: String) -> Result<Module<'js, Declared>> {
    let module = Module::declare(ctx.clone(), name, source)?;
    module.meta()?.set("url", url)?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specifiers_resolve_as_in_a_browser() {
        let mut map = ImportMap::default();
        map.merge(r#"{"imports": {"react": "/vendor/react.js", "lib/": "https://cdn.test/lib/", "bad": 1}}"#, "https://site.test/app/").unwrap();
        let doc = "https://site.test/app/page.html";
        let r = |base: &str, s: &str| resolve(&map, base, doc, s);
        assert_eq!(r("https://site.test/app/main.js", "./a.js").unwrap(), "https://site.test/app/a.js");
        assert_eq!(r("https://site.test/app/x/main.js", "../b.js").unwrap(), "https://site.test/app/b.js");
        assert_eq!(r("https://site.test/app/main.js", "/c.js").unwrap(), "https://site.test/c.js");
        assert_eq!(r("https://site.test/app/main.js", "https://other.test/d.js").unwrap(), "https://other.test/d.js");
        assert_eq!(r("https://site.test/app/main.js", "react").unwrap(), "https://site.test/vendor/react.js");
        assert_eq!(r("https://site.test/app/main.js", "lib/x/y.js").unwrap(), "https://cdn.test/lib/x/y.js");
        // Not a URL: a classic script's `import()`, or Runtime.evaluate.
        assert_eq!(r("inline-script-4", "./e.js").unwrap(), "https://site.test/app/e.js");
        let error = r("https://site.test/app/main.js", "vue").unwrap_err();
        assert!(error.contains("Failed to resolve module specifier \"vue\""), "{error}");
        assert!(r("https://site.test/app/main.js", "bad").is_err());
    }
}

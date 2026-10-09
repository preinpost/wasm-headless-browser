//! The cookie jar.
//!
//! RFC 6265, reduced to what a login flow needs: parsing `Set-Cookie`, domain
//! and path matching, expiry, `Secure` and `HttpOnly`. One jar is shared by
//! every target the engine creates, the way a browser profile shares one.
//!
//! **The jar lives in memory only.** Session cookies are credentials in all but
//! name, and nothing here writes them anywhere. That is a requirement (M4), not
//! an omission.
//!
//! Not implemented: the public suffix list. A `Domain` attribute has to cover
//! the request host, which stops `example.com` from setting cookies for
//! `other.com`, but nothing stops `a.co.uk` from setting one for `co.uk`.

use crate::fetch::Url;

#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Lowercase, without a leading dot.
    pub domain: String,
    /// Set when the cookie had no `Domain` attribute: it then matches its
    /// origin host exactly and not its subdomains.
    pub host_only: bool,
    pub path: String,
    /// Seconds since the Unix epoch. `None` is a session cookie.
    pub expires: Option<f64>,
    pub secure: bool,
    pub http_only: bool,
    /// `Strict`, `Lax` or `None`, as the server spelled it, normalised.
    pub same_site: Option<String>,
    /// Order of creation, which breaks ties in the `Cookie` header.
    created: u64,
}

impl Cookie {
    fn expired(&self, now: f64) -> bool {
        self.expires.is_some_and(|at| at <= now)
    }

    fn matches(&self, url: &Url, now: f64) -> bool {
        !self.expired(now)
            && domain_matches(&self.domain, self.host_only, &url.host.to_ascii_lowercase())
            && path_matches(&self.path, request_path(url))
            && (!self.secure || is_secure(url))
    }
}

/// Who is writing a cookie. Script may neither set nor read `HttpOnly` ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Http,
    Script,
}

#[derive(Debug, Default)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
    counter: u64,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores one `Set-Cookie` header value received from `url`. Returns
    /// whether it was accepted; a rejected cookie is dropped silently, as a
    /// browser drops it.
    pub fn store(&mut self, url: &Url, header: &str, source: Source, now: f64) -> bool {
        let Some(cookie) = parse_set_cookie(url, header, now) else { return false };
        if source == Source::Script && cookie.http_only {
            return false;
        }
        self.insert(cookie, source, now)
    }

    /// Adds a cookie built elsewhere, as `Network.setCookie` does.
    pub fn set(&mut self, cookie: Cookie, now: f64) -> bool {
        self.insert(cookie, Source::Http, now)
    }

    fn insert(&mut self, mut cookie: Cookie, source: Source, now: f64) -> bool {
        let existing = self.cookies.iter().position(|c| {
            c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path
        });
        if let Some(index) = existing {
            // Script cannot overwrite an HttpOnly cookie either.
            if source == Source::Script && self.cookies[index].http_only {
                return false;
            }
            cookie.created = self.cookies[index].created;
            self.cookies.remove(index);
        } else {
            self.counter += 1;
            cookie.created = self.counter;
        }
        // An already-expired cookie is how a server deletes one.
        if !cookie.expired(now) {
            self.cookies.push(cookie);
        }
        true
    }

    /// The cookies to send to `url`, in `Cookie` header order: longer paths
    /// first, then older first.
    pub fn matching(&self, url: &Url, source: Source, now: f64) -> Vec<&Cookie> {
        let mut out: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| c.matches(url, now))
            .filter(|c| source == Source::Http || !c.http_only)
            .collect();
        out.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.created.cmp(&b.created)));
        out
    }

    /// The `Cookie` request header for `url`, or `None` when nothing matches.
    pub fn header(&self, url: &Url, now: f64) -> Option<String> {
        let pairs = self.matching(url, Source::Http, now);
        if pairs.is_empty() {
            return None;
        }
        Some(pairs.iter().map(|c| format!("{}={}", c.name, c.value)).collect::<Vec<_>>().join("; "))
    }

    /// `document.cookie` for `url`.
    pub fn script_string(&self, url: &Url, now: f64) -> String {
        self.matching(url, Source::Script, now)
            .iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Every unexpired cookie, in creation order.
    pub fn all(&self, now: f64) -> Vec<&Cookie> {
        self.cookies.iter().filter(|c| !c.expired(now)).collect()
    }

    /// Removes cookies called `name`, optionally narrowed by domain and path.
    pub fn delete(&mut self, name: &str, domain: Option<&str>, path: Option<&str>) -> usize {
        let domain = domain.map(|d| d.trim_start_matches('.').to_ascii_lowercase());
        let before = self.cookies.len();
        self.cookies.retain(|c| {
            !(c.name == name
                && domain.as_deref().is_none_or(|d| c.domain == d)
                && path.is_none_or(|p| c.path == p))
        });
        before - self.cookies.len()
    }

    pub fn clear(&mut self) {
        self.cookies.clear();
    }
}

/// Builds a cookie from parts, the way `Network.setCookie` describes one.
/// Returns `None` when the parts cannot form a cookie for that URL.
#[allow(clippy::too_many_arguments)]
pub fn from_parts(
    url: Option<&Url>,
    name: &str,
    value: &str,
    domain: Option<&str>,
    path: Option<&str>,
    secure: bool,
    http_only: bool,
    same_site: Option<&str>,
    expires: Option<f64>,
) -> Option<Cookie> {
    let (domain, host_only) = match (domain, url) {
        (Some(d), _) if !d.is_empty() => (d.trim_start_matches('.').to_ascii_lowercase(), false),
        (_, Some(url)) => (url.host.to_ascii_lowercase(), true),
        _ => return None,
    };
    let path = match (path, url) {
        (Some(p), _) if p.starts_with('/') => p.to_string(),
        (_, Some(url)) => default_path(request_path(url)),
        _ => "/".to_string(),
    };
    Some(Cookie {
        name: name.to_string(),
        value: value.to_string(),
        domain,
        host_only,
        path,
        expires,
        secure,
        http_only,
        same_site: same_site.and_then(normalise_same_site),
        created: 0,
    })
}

/// RFC 6265 §5.2, minus the parts no login flow needs.
fn parse_set_cookie(url: &Url, header: &str, now: f64) -> Option<Cookie> {
    let mut parts = header.split(';');
    let pair = parts.next()?.trim();
    let (name, value) = pair.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let value = value.trim().trim_matches('"');

    let host = url.host.to_ascii_lowercase();
    let mut domain = None;
    let mut path = None;
    let mut expires = None;
    let mut max_age = None;
    let mut secure = false;
    let mut http_only = false;
    let mut same_site = None;

    for attribute in parts {
        let (key, val) = match attribute.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (attribute.trim(), ""),
        };
        match key.to_ascii_lowercase().as_str() {
            "expires" => expires = parse_http_date(val),
            "max-age" => {
                if let Ok(seconds) = val.parse::<i64>() {
                    max_age = Some(seconds);
                }
            }
            "domain" if !val.is_empty() => {
                domain = Some(val.trim_start_matches('.').to_ascii_lowercase());
            }
            "path" if val.starts_with('/') => path = Some(val.to_string()),
            "secure" => secure = true,
            "httponly" => http_only = true,
            "samesite" => same_site = normalise_same_site(val),
            _ => {}
        }
    }

    // A Domain attribute must cover the host that set it.
    let (domain, host_only) = match domain {
        Some(d) if d == host => (d, false),
        // An IP address has no parent domain to share a cookie with.
        Some(_) if is_ip(&host) => return None,
        Some(d) if host.ends_with(&format!(".{d}")) => (d, false),
        Some(_) => return None,
        None => (host, true),
    };

    // Max-Age wins over Expires.
    let expires = match max_age {
        Some(seconds) if seconds <= 0 => Some(f64::MIN),
        Some(seconds) => Some(now + seconds as f64),
        None => expires,
    };

    // A non-secure origin may not set a Secure cookie.
    if secure && !is_secure(url) {
        return None;
    }

    Some(Cookie {
        name: name.to_string(),
        value: value.to_string(),
        domain,
        host_only,
        path: path.unwrap_or_else(|| default_path(request_path(url))),
        expires,
        secure,
        http_only,
        same_site,
        created: 0,
    })
}

fn normalise_same_site(value: &str) -> Option<String> {
    match value.to_ascii_lowercase().as_str() {
        "strict" => Some("Strict".into()),
        "lax" => Some("Lax".into()),
        "none" => Some("None".into()),
        _ => None,
    }
}

/// The path without its query or fragment.
fn request_path(url: &Url) -> &str {
    let end = url.path.find(['?', '#']).unwrap_or(url.path.len());
    &url.path[..end]
}

/// RFC 6265 §5.1.4: the directory of the request path.
fn default_path(path: &str) -> String {
    if !path.starts_with('/') {
        return "/".to_string();
    }
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(pos) => path[..pos].to_string(),
    }
}

fn domain_matches(cookie_domain: &str, host_only: bool, host: &str) -> bool {
    if host_only {
        return host == cookie_domain;
    }
    host == cookie_domain || (host.ends_with(&format!(".{cookie_domain}")) && !is_ip(host))
}

fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    let request_path = if request_path.is_empty() { "/" } else { request_path };
    if request_path == cookie_path {
        return true;
    }
    request_path.starts_with(cookie_path)
        && (cookie_path.ends_with('/') || request_path[cookie_path.len()..].starts_with('/'))
}

/// `https`, or a loopback host, which Chrome also treats as a secure context.
/// Without the second half a local test server could never set a Secure
/// cookie.
fn is_secure(url: &Url) -> bool {
    url.scheme == "https" || matches!(url.host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

fn is_ip(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[')
}

/// RFC 6265 §5.1.1, which accepts every date format servers actually send.
pub fn parse_http_date(input: &str) -> Option<f64> {
    let mut time = None;
    let mut day = None;
    let mut month = None;
    let mut year = None;

    let tokens = input
        .split(|c: char| {
            c == '\t' || (' '..='/').contains(&c) || (';'..='@').contains(&c) || ('['..='`').contains(&c)
                || ('{'..='~').contains(&c)
        })
        .filter(|t| !t.is_empty());

    for token in tokens {
        if time.is_none() {
            let fields: Vec<&str> = token.split(':').collect();
            if fields.len() == 3 {
                let numbers: Option<Vec<u32>> =
                    fields.iter().map(|f| leading_digits(f, 1, 2)).collect();
                if let Some(n) = numbers {
                    time = Some((n[0], n[1], n[2]));
                    continue;
                }
            }
        }
        if day.is_none() {
            if let Some(d) = leading_digits(token, 1, 2) {
                day = Some(d);
                continue;
            }
        }
        if month.is_none() && token.len() >= 3 {
            const MONTHS: [&str; 12] =
                ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
            let prefix = token[..3].to_ascii_lowercase();
            if let Some(m) = MONTHS.iter().position(|m| *m == prefix) {
                month = Some(m as u32 + 1);
                continue;
            }
        }
        if year.is_none() {
            if let Some(y) = leading_digits(token, 2, 4) {
                year = Some(y);
                continue;
            }
        }
    }

    let mut year = year?;
    if (70..=99).contains(&year) {
        year += 1900;
    } else if year <= 69 {
        year += 2000;
    }
    let (hour, minute, second) = time?;
    let (day, month) = (day?, month?);
    if year < 1601 || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year as i64, month, day);
    Some((days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60 + second as i64) as f64)
}

/// `min..=max` ASCII digits at the start of `token`, followed by a non-digit or
/// nothing.
fn leading_digits(token: &str, min: usize, max: usize) -> Option<u32> {
    let digits = token.bytes().take_while(u8::is_ascii_digit).count();
    if digits < min || digits > max {
        return None;
    }
    token[..digits].parse().ok()
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i64;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Seconds since the Unix epoch.
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_800_000_000.0;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn stores_and_sends_a_session_cookie() {
        let mut jar = CookieJar::new();
        assert!(jar.store(&url("http://example.test/login"), "sid=abc; Path=/", Source::Http, NOW));
        assert_eq!(jar.header(&url("http://example.test/app"), NOW).as_deref(), Some("sid=abc"));
        // Host-only: a subdomain does not get it.
        assert_eq!(jar.header(&url("http://www.example.test/"), NOW), None);
        assert_eq!(jar.header(&url("http://other.test/"), NOW), None);
    }

    #[test]
    fn a_domain_attribute_covers_subdomains_but_must_cover_the_host() {
        let mut jar = CookieJar::new();
        assert!(jar.store(&url("http://login.example.test/"), "a=1; Domain=.example.test", Source::Http, NOW));
        assert_eq!(jar.header(&url("http://app.example.test/"), NOW).as_deref(), Some("a=1"));
        assert_eq!(jar.header(&url("http://example.test/"), NOW).as_deref(), Some("a=1"));
        // Setting a cookie for an unrelated domain is refused.
        assert!(!jar.store(&url("http://example.test/"), "b=2; Domain=other.test", Source::Http, NOW));
    }

    #[test]
    fn the_default_path_is_the_request_directory() {
        let mut jar = CookieJar::new();
        jar.store(&url("http://example.test/app/login?next=1"), "p=1", Source::Http, NOW);
        assert_eq!(jar.header(&url("http://example.test/app/home"), NOW).as_deref(), Some("p=1"));
        assert_eq!(jar.header(&url("http://example.test/app"), NOW).as_deref(), Some("p=1"));
        assert_eq!(jar.header(&url("http://example.test/application"), NOW), None);
        assert_eq!(jar.header(&url("http://example.test/"), NOW), None);
    }

    #[test]
    fn longer_paths_come_first() {
        let mut jar = CookieJar::new();
        jar.store(&url("http://example.test/"), "root=1; Path=/", Source::Http, NOW);
        jar.store(&url("http://example.test/"), "deep=2; Path=/a/b", Source::Http, NOW);
        assert_eq!(jar.header(&url("http://example.test/a/b/c"), NOW).as_deref(), Some("deep=2; root=1"));
    }

    #[test]
    fn expiry_and_deletion() {
        let mut jar = CookieJar::new();
        let u = url("http://example.test/");
        jar.store(&u, "a=1; Max-Age=60", Source::Http, NOW);
        assert!(jar.header(&u, NOW + 59.0).is_some());
        assert!(jar.header(&u, NOW + 61.0).is_none());

        jar.store(&u, "b=1", Source::Http, NOW);
        // The way servers delete: an expiry in the past.
        jar.store(&u, "b=; Expires=Thu, 01 Jan 1970 00:00:00 GMT", Source::Http, NOW);
        jar.store(&u, "c=1", Source::Http, NOW);
        jar.store(&u, "c=; Max-Age=0", Source::Http, NOW);
        // Only `a`, which has not expired yet, is left.
        assert_eq!(jar.header(&u, NOW).as_deref(), Some("a=1"));
    }

    #[test]
    fn replacing_keeps_one_cookie() {
        let mut jar = CookieJar::new();
        let u = url("http://example.test/");
        jar.store(&u, "a=1", Source::Http, NOW);
        jar.store(&u, "a=2", Source::Http, NOW);
        assert_eq!(jar.header(&u, NOW).as_deref(), Some("a=2"));
        assert_eq!(jar.all(NOW).len(), 1);
    }

    #[test]
    fn secure_cookies_need_a_secure_origin() {
        let mut jar = CookieJar::new();
        assert!(!jar.store(&url("http://example.test/"), "s=1; Secure", Source::Http, NOW));
        assert!(jar.store(&url("https://example.test/"), "s=1; Secure", Source::Http, NOW));
        assert_eq!(jar.header(&url("http://example.test/"), NOW), None);
        assert_eq!(jar.header(&url("https://example.test/"), NOW).as_deref(), Some("s=1"));
        // Loopback counts as secure, as it does in Chrome.
        assert!(jar.store(&url("http://127.0.0.1:8123/"), "s=1; Secure", Source::Http, NOW));
    }

    #[test]
    fn script_cannot_see_or_touch_http_only_cookies() {
        let mut jar = CookieJar::new();
        let u = url("http://example.test/");
        jar.store(&u, "session=secret; HttpOnly", Source::Http, NOW);
        jar.store(&u, "theme=dark", Source::Http, NOW);
        assert_eq!(jar.script_string(&u, NOW), "theme=dark");
        assert!(!jar.store(&u, "session=forged", Source::Script, NOW));
        assert!(!jar.store(&u, "other=1; HttpOnly", Source::Script, NOW));
        assert_eq!(jar.header(&u, NOW).as_deref(), Some("session=secret; theme=dark"));
    }

    #[test]
    fn parses_the_date_formats_servers_send() {
        let expected = 1_445_412_480.0; // 2015-10-21 07:28:00 UTC
        assert_eq!(parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(expected));
        assert_eq!(parse_http_date("Wednesday, 21-Oct-15 07:28:00 GMT"), Some(expected));
        assert_eq!(parse_http_date("Wed Oct 21 07:28:00 2015"), Some(expected));
        assert_eq!(parse_http_date("not a date"), None);
    }

    #[test]
    fn delete_and_clear() {
        let mut jar = CookieJar::new();
        let u = url("http://example.test/");
        jar.store(&u, "a=1", Source::Http, NOW);
        jar.store(&u, "b=1", Source::Http, NOW);
        assert_eq!(jar.delete("a", Some("example.test"), None), 1);
        assert_eq!(jar.header(&u, NOW).as_deref(), Some("b=1"));
        jar.clear();
        assert!(jar.all(NOW).is_empty());
    }
}

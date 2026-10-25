//! Fetching the documents that `http://` and `https://` `$ref`s point to.
//!
//! Upstream resolves such references through jsonschema-path's default handlers: the
//! document is downloaded with `urllib.request.urlopen(url, timeout=10)` (or `requests`,
//! when that package is installed), parsed like a spec file, and any failure, whether a
//! network error, an error status or unparsable content, makes the reference
//! unresolvable. Documents are fetched lazily, when validation first reaches a reference
//! to them, and once per validation.
//!
//! [`Retrieve`] is the hook for the download itself. [`HttpRetriever`], behind the `http`
//! feature, is the built-in implementation that [`Options::default`] and the command
//! line use.
//!
//! [`Options::default`]: crate::Options::default

/// Downloads the document an `http://` or `https://` `$ref` points to.
///
/// `file://` references are read by the validator itself, so only those two schemes reach
/// a retriever. Retrievers are called from validation threads, possibly concurrently
/// when several documents are validated at once.
pub trait Retrieve: Send + Sync {
    /// The raw bytes of the document at `url`, which are then parsed like a spec file.
    ///
    /// Any error makes the reference unresolvable, which is reported with the reference
    /// as written (upstream's `Unresolvable(ref)`); the error message is not shown.
    fn retrieve(&self, url: &str) -> Result<Vec<u8>, RetrieveError>;
}

impl<F> Retrieve for F
where
    F: Fn(&str) -> Result<Vec<u8>, RetrieveError> + Send + Sync,
{
    fn retrieve(&self, url: &str) -> Result<Vec<u8>, RetrieveError> {
        self(url)
    }
}

/// A failed download.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct RetrieveError {
    pub message: String,
}

impl RetrieveError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(feature = "http")]
pub use self::http::HttpRetriever;

#[cfg(feature = "http")]
mod http {
    use std::collections::HashMap;
    use std::fmt;
    use std::io::ErrorKind;
    use std::sync::OnceLock;
    use std::time::Duration;

    use ureq::tls::{RootCerts, TlsConfig};
    use ureq::{Agent, Proxy};

    use super::{Retrieve, RetrieveError};
    use crate::pyurl;

    /// The timeout jsonschema-path's handlers pass to `urlopen` and `requests.get`.
    const TIMEOUT: Duration = Duration::from_secs(10);
    /// `HTTPRedirectHandler.max_redirections` and `.max_repeats`.
    const MAX_REDIRECTIONS: usize = 10;
    const MAX_REPEATS: usize = 4;

    /// The built-in [`Retrieve`] implementation: plain `GET` requests that behave like
    /// upstream's `urllib` handler, `urlopen(url, timeout=10)`.
    ///
    /// - The URL is requested as written (after `urljoin`), never normalised. What
    ///   `urllib` refuses is refused without a request: control characters or spaces,
    ///   non-ASCII characters, and user information before the host.
    /// - Each socket operation (connecting, sending, every read) times out after 10
    ///   seconds; see [`HttpRetriever::with_timeout`].
    /// - Redirects (301, 302, 303, 307, 308) are followed with `urllib`'s rules: the
    ///   `Location` is joined like `urljoin`, only `http` and `https` targets are allowed,
    ///   at most 10 redirections and 4 visits of the same URL. Any other status outside
    ///   200 to 299 is a failure.
    /// - Proxies come from `http_proxy`, `https_proxy` and `no_proxy` (upper-case too),
    ///   read as `urllib.request.getproxies_environment` reads them; every target is
    ///   tunnelled through the proxy with `CONNECT`.
    /// - Certificates are checked against the operating system's certificate store, as
    ///   Python's default SSL context does.
    /// - The whole body is read, whatever its size, and a body the server cuts short is
    ///   used as far as it goes; no `Accept-Encoding` is sent.
    ///
    /// The underlying HTTP agent is created on first use and shared by clones.
    #[derive(Clone)]
    pub struct HttpRetriever {
        timeout: Duration,
        proxies: bool,
        agent: OnceLock<Agent>,
    }

    impl HttpRetriever {
        /// A retriever with upstream's 10 second timeout.
        pub fn new() -> Self {
            Self::with_timeout(TIMEOUT)
        }

        /// A retriever whose socket operations time out after `timeout` instead.
        pub fn with_timeout(timeout: Duration) -> Self {
            Self {
                timeout,
                proxies: true,
                agent: OnceLock::new(),
            }
        }

        /// The same retriever, ignoring the proxy environment variables (upstream's
        /// `ProxyHandler({})`).
        pub fn without_proxies(mut self) -> Self {
            self.proxies = false;
            self
        }

        fn agent(&self) -> &Agent {
            self.agent.get_or_init(|| {
                let tls = TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build();
                let config = Agent::config_builder()
                    .user_agent(concat!(
                        "openapi-spec-validator-rs/",
                        env!("CARGO_PKG_VERSION")
                    ))
                    // `urlopen`'s timeout is the socket timeout: it applies to the
                    // connection and to every read and write separately, with no limit
                    // on the whole transfer.
                    .timeout_connect(Some(self.timeout))
                    .timeout_per_read(Some(self.timeout))
                    .timeout_per_write(Some(self.timeout))
                    // Redirects are followed here, with urllib's rules.
                    .max_redirects(0)
                    .max_redirects_will_error(false)
                    .http_status_as_error(false)
                    // Proxies are chosen per request, by scheme, like urllib does.
                    .proxy(None)
                    .tls_config(tls)
                    .build();
                Agent::new_with_config(config)
            })
        }

        /// `GET url` through the proxy the environment names for its scheme.
        fn get(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, RetrieveError> {
            let failed = |error: ureq::Error| RetrieveError::new(error.to_string());
            let proxy = if self.proxies { proxy_for(url)? } else { None };
            let request = self.agent().get(url).config().proxy(proxy).build();
            request.call().map_err(failed)
        }
    }

    impl Default for HttpRetriever {
        fn default() -> Self {
            Self::new()
        }
    }

    impl fmt::Debug for HttpRetriever {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("HttpRetriever")
                .field("timeout", &self.timeout)
                .finish_non_exhaustive()
        }
    }

    impl Retrieve for HttpRetriever {
        fn retrieve(&self, url: &str) -> Result<Vec<u8>, RetrieveError> {
            let mut url = url.to_owned();
            let mut visited: HashMap<String, usize> = HashMap::new();
            loop {
                check_like_urllib(&url)?;
                let mut response = self.get(&url)?;
                let status = response.status().as_u16();
                if matches!(status, 301 | 302 | 303 | 307 | 308) {
                    url = redirect_target(&url, &response, status)?;
                    // `HTTPRedirectHandler.http_error_302`'s loop detection: the same
                    // URL at most four times, at most ten distinct redirections.
                    let visits = visited.get(&url).copied().unwrap_or(0);
                    if visits >= MAX_REPEATS || visited.len() >= MAX_REDIRECTIONS {
                        return Err(RetrieveError::new(format!(
                            "http status: {status} - The HTTP server returned a redirect error that \
                             would lead to an infinite loop"
                        )));
                    }
                    visited.insert(url.clone(), visits + 1);
                    continue;
                }
                // `HTTPErrorProcessor`: anything outside 200-299 is an `HTTPError`.
                if !(200..300).contains(&status) {
                    return Err(RetrieveError::new(format!("http status: {status}")));
                }
                return read_body(&mut response);
            }
        }
    }

    /// Refuses what `urllib` would fail on before or while sending the request: control
    /// characters and spaces (`http.client.InvalidURL`), non-ASCII characters (the
    /// request line is ASCII) and user information before the host (which `urllib`
    /// treats as part of the host name).
    fn check_like_urllib(url: &str) -> Result<(), RetrieveError> {
        if url.chars().any(|c| c <= ' ' || c == '\x7f') {
            return Err(RetrieveError::new(
                "URL can't contain control characters".to_owned(),
            ));
        }
        if !url.is_ascii() {
            return Err(RetrieveError::new("URL contains non-ASCII characters"));
        }
        let parsed = pyurl::urlparse(url, "").map_err(RetrieveError::new)?;
        if parsed.netloc.contains('@') {
            return Err(RetrieveError::new("URL contains user information"));
        }
        Ok(())
    }

    /// The URL a redirect leads to, as `HTTPRedirectHandler.http_error_302` computes it.
    fn redirect_target(
        url: &str,
        response: &ureq::http::Response<ureq::Body>,
        status: u16,
    ) -> Result<String, RetrieveError> {
        let headers = response.headers();
        let location = headers
            .get("location")
            .or_else(|| headers.get("uri"))
            .ok_or_else(|| RetrieveError::new(format!("http status: {status}")))?;
        // http.client decodes header bytes as ISO-8859-1.
        let location: String = location.as_bytes().iter().map(|&b| b as char).collect();
        let mut parts = pyurl::urlparse(&location, "").map_err(RetrieveError::new)?;
        if !matches!(parts.scheme.as_str(), "http" | "https" | "ftp" | "") {
            return Err(RetrieveError::new(format!(
                "Redirection to url '{location}' is not allowed"
            )));
        }
        if parts.path.is_empty() && !parts.netloc.is_empty() {
            parts.path = "/".to_owned();
        }
        // `quote(newurl, encoding="iso-8859-1", safe=string.punctuation)`, then `urljoin`.
        let quoted: String = pyurl::urlunparse(&parts)
            .chars()
            .flat_map(|c| {
                if c.is_ascii_alphanumeric() || c.is_ascii_punctuation() {
                    vec![c]
                } else {
                    format!("%{:02X}", c as u32 & 0xff).chars().collect()
                }
            })
            .collect();
        let target = pyurl::urljoin(url, &quoted).map_err(RetrieveError::new)?;
        Ok(target.replace(' ', "%20"))
    }

    /// The whole body. A body the server cuts short is returned as far as it arrived,
    /// as `urlopen`'s stream yields it to PyYAML's chunked reads; other failures (a
    /// timeout, a reset) are errors, as they are there.
    fn read_body(
        response: &mut ureq::http::Response<ureq::Body>,
    ) -> Result<Vec<u8>, RetrieveError> {
        // `read_to_vec` stops at 10 MB by default; `urlopen` reads everything, as far as
        // PyYAML's reader gets through it.
        let mut reader = response.body_mut().with_config().limit(u64::MAX).reader();
        let mut body = Vec::new();
        match crate::read_until_unreadable(&mut reader, &mut body) {
            Ok(()) => Ok(body),
            // A body cut short by the server is used as far as it arrived.
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(body),
            Err(error) => Err(RetrieveError::new(error.to_string())),
        }
    }

    /// `getproxies_environment()`: scheme (lower-case, `no` for `no_proxy`) to proxy,
    /// lower-case variable names winning over upper-case ones.
    fn proxies_from_env(vars: &[(String, String)]) -> HashMap<String, String> {
        let mut proxies = HashMap::new();
        let suffixed: Vec<(&str, &str, String)> = vars
            .iter()
            .filter(|(name, _)| {
                name.len() > 5 && name[name.len() - 6..].eq_ignore_ascii_case("_proxy")
            })
            .map(|(name, value)| {
                (
                    name.as_str(),
                    value.as_str(),
                    name[..name.len() - 6].to_ascii_lowercase(),
                )
            })
            .collect();
        for (_, value, key) in &suffixed {
            if !value.is_empty() {
                proxies.insert(key.clone(), (*value).to_owned());
            }
        }
        // CVE-2016-1000110: HTTP_PROXY may come from a request header in CGI.
        if vars.iter().any(|(name, _)| name == "REQUEST_METHOD") {
            proxies.remove("http");
        }
        for (name, value, key) in &suffixed {
            if name.ends_with("_proxy") {
                if value.is_empty() {
                    proxies.remove(key);
                } else {
                    proxies.insert(key.clone(), (*value).to_owned());
                }
            }
        }
        proxies
    }

    /// `proxy_bypass_environment(host, proxies)`.
    fn bypassed(netloc: &str, proxies: &HashMap<String, String>) -> bool {
        let Some(no_proxy) = proxies.get("no") else {
            return false;
        };
        if no_proxy == "*" {
            return true;
        }
        let host = netloc.to_ascii_lowercase();
        // `_splitport`: `host:port` with a numeric or empty port.
        let hostonly = match host.rsplit_once(':') {
            Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => host.as_str(),
        };
        no_proxy
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .any(|name| {
                let name = name.trim_start_matches('.').to_ascii_lowercase();
                hostonly == name
                    || host == name
                    || hostonly.ends_with(&format!(".{name}"))
                    || host.ends_with(&format!(".{name}"))
            })
    }

    /// The proxy `urllib` would use for `url`, from the environment.
    fn proxy_for(url: &str) -> Result<Option<Proxy>, RetrieveError> {
        let parsed = pyurl::urlparse(url, "").map_err(RetrieveError::new)?;
        let vars: Vec<(String, String)> = std::env::vars().collect();
        let proxies = proxies_from_env(&vars);
        let Some(proxy) = proxies.get(&parsed.scheme) else {
            return Ok(None);
        };
        if bypassed(&parsed.netloc, &proxies) {
            return Ok(None);
        }
        // `_parse_proxy`: a bare `host:port` is an HTTP proxy.
        let proxy = if pyurl::scheme(proxy).is_empty() {
            format!("http://{proxy}")
        } else {
            proxy.to_owned()
        };
        Proxy::new(&proxy)
            .map(Some)
            .map_err(|error| RetrieveError::new(error.to_string()))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        }

        #[test]
        fn proxies_follow_getproxies_environment() {
            let proxies = proxies_from_env(&env(&[
                ("HTTP_PROXY", "http://upper:1"),
                ("http_proxy", "http://lower:1"),
                ("HTTPS_PROXY", "https-proxy:2"),
                ("ALL_PROXY", "http://all:3"),
                ("NO_PROXY", ".example.com, localhost"),
            ]));
            assert_eq!(proxies["http"], "http://lower:1");
            assert_eq!(proxies["https"], "https-proxy:2");
            assert_eq!(proxies["all"], "http://all:3");
            assert!(bypassed("api.example.com:8080", &proxies));
            assert!(bypassed("LOCALHOST", &proxies));
            assert!(!bypassed("example.org", &proxies));
            // An empty lower-case variable disables an upper-case one.
            let proxies =
                proxies_from_env(&env(&[("HTTP_PROXY", "http://p:1"), ("http_proxy", "")]));
            assert!(!proxies.contains_key("http"));
            // Under CGI, HTTP_PROXY is ignored.
            let proxies = proxies_from_env(&env(&[
                ("HTTP_PROXY", "http://p:1"),
                ("REQUEST_METHOD", "GET"),
            ]));
            assert!(!proxies.contains_key("http"));
        }

        #[test]
        fn urls_urllib_refuses_are_refused() {
            for url in [
                "http://127.0.0.1/a b.json",
                "http://127.0.0.1/tab\t.json",
                "http://127.0.0.1/caf\u{e9}.json",
                "http://u:p@127.0.0.1/x.json",
            ] {
                assert!(check_like_urllib(url).is_err(), "{url}");
            }
            assert!(check_like_urllib("http://127.0.0.1:8/sub/../x.json?q=1").is_ok());
        }
    }
}

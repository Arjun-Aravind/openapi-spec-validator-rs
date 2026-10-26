//! The parts of `urllib.parse` that upstream uses to join and split reference URIs
//! (`referencing` calls `urljoin` and `urldefrag`; `urllib.request` calls them for
//! redirects). They are string operations that neither percent-encode nor normalise
//! anything, so a reference reaches the network exactly as upstream would send it.

/// `urllib.parse.uses_relative`.
const USES_RELATIVE: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms",
    "prospero", "rtsp", "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];
/// `urllib.parse.uses_netloc`.
const USES_NETLOC: &[&str] = &[
    "",
    "ftp",
    "http",
    "gopher",
    "nntp",
    "telnet",
    "imap",
    "wais",
    "file",
    "mms",
    "https",
    "shttp",
    "snews",
    "prospero",
    "rtsp",
    "rtsps",
    "rtspu",
    "rsync",
    "svn",
    "svn+ssh",
    "sftp",
    "nfs",
    "git",
    "git+ssh",
    "ws",
    "wss",
    "itms-services",
];
/// `urllib.parse.uses_params`.
const USES_PARAMS: &[&str] = &[
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu",
    "sip", "sips", "mms", "sftp", "tel",
];

/// `urllib.parse.ParseResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub params: String,
    pub query: String,
    pub fragment: String,
}

fn is_c0_or_space(c: char) -> bool {
    c <= ' '
}

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
}

/// `urlsplit(url, scheme)` (fragments allowed). The error is the `ValueError` upstream raises.
fn urlsplit(url: &str, default_scheme: &str) -> Result<Parsed, String> {
    let url: String = url
        .trim_start_matches(is_c0_or_space)
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme: String = default_scheme
        .trim_matches(is_c0_or_space)
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = url.as_str();
    if let Some(i) = url.find(':') {
        if i > 0 && url.as_bytes()[0].is_ascii_alphabetic() && url[..i].chars().all(is_scheme_char)
        {
            scheme = url[..i].to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = "";
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = &after[..end];
        rest = &after[end..];
        if netloc.contains('[') != netloc.contains(']') {
            return Err("Invalid IPv6 URL".to_owned());
        }
    }
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    Ok(Parsed {
        scheme,
        netloc: netloc.to_owned(),
        path: path.to_owned(),
        params: String::new(),
        query: query.to_owned(),
        fragment: fragment.to_owned(),
    })
}

/// `urlparse(url, scheme)`: `urlsplit` plus the `;params` of the last path segment.
pub(crate) fn urlparse(url: &str, default_scheme: &str) -> Result<Parsed, String> {
    let mut parsed = urlsplit(url, default_scheme)?;
    if USES_PARAMS.contains(&parsed.scheme.as_str()) && parsed.path.contains(';') {
        let path = &parsed.path;
        let split = match path.rfind('/') {
            Some(slash) => path[slash..].find(';').map(|i| slash + i),
            None => path.find(';'),
        };
        if let Some(i) = split {
            let params = path[i + 1..].to_owned();
            parsed.path.truncate(i);
            parsed.params = params;
        }
    }
    Ok(parsed)
}

/// `urlunparse`.
pub(crate) fn urlunparse(parsed: &Parsed) -> String {
    let mut url = parsed.path.clone();
    if !parsed.params.is_empty() {
        url = format!("{url};{}", parsed.params);
    }
    if !parsed.netloc.is_empty() {
        if !url.is_empty() && !url.starts_with('/') {
            url.insert(0, '/');
        }
        url = format!("//{}{url}", parsed.netloc);
    } else if url.starts_with("//")
        || (!parsed.scheme.is_empty()
            && USES_NETLOC.contains(&parsed.scheme.as_str())
            && (url.is_empty() || url.starts_with('/')))
    {
        url = format!("//{url}");
    }
    if !parsed.scheme.is_empty() {
        url = format!("{}:{url}", parsed.scheme);
    }
    if !parsed.query.is_empty() {
        url = format!("{url}?{}", parsed.query);
    }
    if !parsed.fragment.is_empty() {
        url = format!("{url}#{}", parsed.fragment);
    }
    url
}

/// `urljoin(base, url)`.
pub(crate) fn urljoin(base: &str, url: &str) -> Result<String, String> {
    if base.is_empty() {
        return Ok(url.to_owned());
    }
    if url.is_empty() {
        return Ok(base.to_owned());
    }
    let b = urlparse(base, "")?;
    let mut u = urlparse(url, &b.scheme)?;
    if u.scheme != b.scheme || !USES_RELATIVE.contains(&u.scheme.as_str()) {
        return Ok(url.to_owned());
    }
    if USES_NETLOC.contains(&u.scheme.as_str()) {
        if !u.netloc.is_empty() {
            return Ok(urlunparse(&u));
        }
        u.netloc = b.netloc.clone();
    }
    if u.path.is_empty() && u.params.is_empty() {
        u.path = b.path.clone();
        u.params = b.params.clone();
        if u.query.is_empty() {
            u.query = b.query.clone();
        }
        return Ok(urlunparse(&u));
    }
    let mut base_parts: Vec<&str> = b.path.split('/').collect();
    if base_parts.last() != Some(&"") {
        base_parts.pop();
    }
    let segments: Vec<&str> = if u.path.starts_with('/') {
        u.path.split('/').collect()
    } else {
        let mut segments = base_parts;
        segments.extend(u.path.split('/'));
        let last = segments.len() - 1;
        let mut kept: Vec<&str> = Vec::with_capacity(segments.len());
        for (i, segment) in segments.iter().enumerate() {
            if i == 0 || i == last || !segment.is_empty() {
                kept.push(segment);
            }
        }
        kept
    };
    let mut resolved: Vec<&str> = Vec::with_capacity(segments.len());
    for segment in &segments {
        match *segment {
            ".." => {
                resolved.pop();
            }
            "." => {}
            other => resolved.push(other),
        }
    }
    if matches!(segments.last(), Some(&".") | Some(&"..")) {
        resolved.push("");
    }
    let path = resolved.join("/");
    u.path = if path.is_empty() {
        "/".to_owned()
    } else {
        path
    };
    Ok(urlunparse(&u))
}

/// `urldefrag(url)`: the URL without its fragment, and the fragment.
pub(crate) fn urldefrag(url: &str) -> Result<(String, String), String> {
    if !url.contains('#') {
        return Ok((url.to_owned(), String::new()));
    }
    let mut parsed = urlparse(url, "")?;
    let fragment = std::mem::take(&mut parsed.fragment);
    Ok((urlunparse(&parsed), fragment))
}

/// The scheme `urlsplit` finds in `url`, lower-cased (empty when there is none).
pub(crate) fn scheme(url: &str) -> String {
    urlsplit(url, "").map(|p| p.scheme).unwrap_or_default()
}

/// The local file `urlopen` reads for the `file:` URL `url`, as POSIX CPython's
/// `FileHandler` finds it, as bytes: `None` when it refuses the URL (a host that is not
/// this machine). Python 3.13 and earlier read the query as part of the file name; 3.14
/// drops it. A path without `//` (`file:etc/x`, what `urljoin` makes of more `..`
/// segments than the base has directories) is relative to the current directory.
///
/// The host check is an approximation: Python compares the addresses the host name
/// resolves to with this machine's; here only `localhost` and `127.0.0.1` count.
pub(crate) fn file_url_path(url: &str, python_minor: u32) -> Option<Vec<u8>> {
    // `Request` strips the URL and a `<URL:...>` wrapping.
    let url = url.trim();
    let url = url
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .map_or(url, str::trim);
    let url = url.strip_prefix("URL:").map_or(url, str::trim);
    if python_minor >= 14 {
        // `url2pathname(full_url, require_scheme=True, resolve_host=True)`.
        let parsed = urlsplit(url, "").ok()?;
        if parsed.scheme != "file" || !is_local_host(&parsed.netloc) {
            return None;
        }
        return Some(unquote_bytes(&parsed.path));
    }
    // `Request._parse`: the fragment is gone, then `_splittype` and `_splithost`.
    let url = url.rsplit_once('#').map_or(url, |(url, _)| url);
    let (_, rest) = url.split_once(':')?;
    let (host, selector) = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find(['/', '#', '?']).unwrap_or(after.len());
            let path = &after[end..];
            let path = if path.is_empty() || path.starts_with('/') {
                path.to_owned()
            } else {
                format!("/{path}")
            };
            (Some(unquote(&after[..end])), path)
        }
        None => (None, rest.to_owned()),
    };
    if let Some(host) = host.as_deref().filter(|host| !host.is_empty()) {
        // `file_open` passes such a selector on to nothing when the host is remote.
        if selector.starts_with("//") && !selector[2..].starts_with('/') && host != "localhost" {
            return None;
        }
        // `open_local_file`: `_splitport`, then a host without a port must be this one.
        let (name, port) = match host.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => {
                (name, (!port.is_empty()).then_some(port))
            }
            _ => (host, None),
        };
        if !name.is_empty() && (port.is_some() || !is_local_host(name)) {
            return None;
        }
    }
    // `url2pathname`.
    let selector = if selector.starts_with("///") {
        &selector[2..]
    } else if selector.starts_with("//localhost/") {
        &selector[11..]
    } else {
        selector.as_str()
    };
    Some(unquote_bytes(selector))
}

fn is_local_host(host: &str) -> bool {
    host.is_empty() || host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1"
}

/// The byte a `%XX` escape at the start of `text` stands for.
fn escaped_byte(text: &[u8]) -> Option<u8> {
    match text {
        [b'%', high, low, ..] => {
            let digit = |b: &u8| char::from(*b).to_digit(16);
            Some(u8::try_from(digit(high)? * 16 + digit(low)?).ok()?)
        }
        _ => None,
    }
}

/// `unquote(text, errors='surrogateescape')` encoded back to bytes the way `os.fsencode`
/// does: every valid `%XX` becomes its byte.
fn unquote_bytes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if let Some(byte) = escaped_byte(&bytes[i..]) {
            decoded.push(byte);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    decoded
}

/// `unquote(text)`: runs of `%XX` decoded as UTF-8, invalid sequences replaced.
fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut run = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(byte) = escaped_byte(&bytes[i..]) {
            run.push(byte);
            i += 3;
        } else {
            out.push_str(&String::from_utf8_lossy(&std::mem::take(&mut run)));
            let c = text[i..].chars().next().unwrap_or_default();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out.push_str(&String::from_utf8_lossy(&run));
    out
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    /// What `urlopen` reads, checked against CPython 3.13 and 3.14 on macOS.
    #[test]
    fn file_urls_open_what_urllib_opens() {
        let cases: [(&str, Option<&str>, Option<&str>); 14] = [
            ("file:///d/b.json", Some("/d/b.json"), Some("/d/b.json")),
            (
                "file:///d/b.json?x=1",
                Some("/d/b.json?x=1"),
                Some("/d/b.json"),
            ),
            (
                "file:///d/q%3F.json",
                Some("/d/q?.json"),
                Some("/d/q?.json"),
            ),
            (
                "file:///d/p%2541.json",
                Some("/d/p%41.json"),
                Some("/d/p%41.json"),
            ),
            (
                "file:///d/semi;p.json",
                Some("/d/semi;p.json"),
                Some("/d/semi;p.json"),
            ),
            (
                "file://localhost/d/b.json",
                Some("/d/b.json"),
                Some("/d/b.json"),
            ),
            (
                "file://LOCALHOST/d/b.json",
                Some("/d/b.json"),
                Some("/d/b.json"),
            ),
            (
                "file://127.0.0.1/d/b.json",
                Some("/d/b.json"),
                Some("/d/b.json"),
            ),
            ("file://otherhost/d/b.json", None, None),
            ("file://localhost:80/d/b.json", None, None),
            ("file:/d/b.json", Some("/d/b.json"), Some("/d/b.json")),
            ("file:etc/passwd", Some("etc/passwd"), Some("etc/passwd")),
            ("file:////d/b.json", Some("//d/b.json"), Some("//d/b.json")),
            ("file:///d/b.json?", Some("/d/b.json?"), Some("/d/b.json")),
        ];
        for (url, py313, py314) in cases {
            let path =
                |minor| file_url_path(url, minor).map(|bytes| String::from_utf8(bytes).unwrap());
            assert_eq!(path(13).as_deref(), py313, "3.13: {url}");
            assert_eq!(path(14).as_deref(), py314, "3.14: {url}");
        }
        // Undecodable escapes stay the bytes they stand for.
        assert_eq!(
            file_url_path("file:///d/%E9.json", 13),
            Some(b"/d/\xe9.json".to_vec())
        );
    }

    /// `[base, url, result]` triples computed by CPython (`scripts/urljoin_cases.py`);
    /// a result of `null` means `urljoin` raised.
    #[test]
    fn urljoin_matches_cpython() {
        let cases: Vec<(String, String, Option<String>)> =
            serde_json::from_str(include_str!("../tests/fixtures/urljoin.json")).unwrap();
        assert!(cases.len() > 1000);
        for (base, url, expected) in cases {
            let actual = urljoin(&base, &url).ok();
            assert_eq!(actual, expected, "urljoin({base:?}, {url:?})");
        }
    }

    #[test]
    fn urldefrag_matches_cpython() {
        let cases: Vec<(String, Value)> =
            serde_json::from_str(include_str!("../tests/fixtures/urldefrag.json")).unwrap();
        assert!(cases.len() > 100);
        for (url, expected) in cases {
            let actual = urldefrag(&url)
                .ok()
                .map(|(u, f)| Value::Array(vec![Value::String(u), Value::String(f)]))
                .unwrap_or(Value::Null);
            assert_eq!(actual, expected, "urldefrag({url:?})");
        }
    }
}

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

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

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

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

/// Whether `scheme` is in one of the lists above for the running Python: `rtsps` joined
/// them in 3.11, and `itms-services` joined `uses_netloc` in 3.12.
fn uses(list: &[&str], scheme: &str) -> bool {
    let minor = crate::context::python_minor();
    list.contains(&scheme)
        && match scheme {
            "rtsps" => minor >= 11,
            "itms-services" => minor >= 12,
            _ => true,
        }
}

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
        // A scheme starts with a letter from Python 3.11.
        let first = url.as_bytes()[0];
        let starts_well = first.is_ascii_alphabetic() || crate::context::python_minor() <= 10;
        if i > 0 && starts_well && url[..i].chars().all(is_scheme_char) {
            scheme = url[..i].to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = "";
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = &after[..end];
        rest = &after[end..];
        check_brackets(netloc)?;
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

/// The bracket checks of `urlsplit` on a netloc: brackets come in pairs, around an IPv6
/// address or an IPvFuture (`_check_bracketed_netloc`).
fn check_brackets(netloc: &str) -> Result<(), String> {
    let invalid = || Err("Invalid IPv6 URL".to_owned());
    match (netloc.contains('['), netloc.contains(']')) {
        (false, false) => return Ok(()),
        (true, true) => {}
        _ => return invalid(),
    }
    let host_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, host)| host);
    let hostname = match host_and_port.split_once('[') {
        Some((before, bracketed)) => {
            // No data before the bracket, nor between it and the port.
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !before.is_empty() || !(port.is_empty() || port.starts_with(':')) {
                return invalid();
            }
            hostname
        }
        None => host_and_port
            .split_once(':')
            .map_or(host_and_port, |(host, _)| host),
    };
    // `_check_bracketed_host`; an uppercase `V` from Python 3.13.
    let future = if crate::context::python_minor() >= 13 {
        hostname.starts_with(['v', 'V'])
    } else {
        hostname.starts_with('v')
    };
    if future {
        let version = &hostname[1..];
        let hex = version.len()
            - version
                .trim_start_matches(|c: char| c.is_ascii_hexdigit())
                .len();
        let valid = hex > 0
            && version[hex..]
                .strip_prefix('.')
                .is_some_and(|address| !address.is_empty() && !address.contains('\n'));
        return if valid {
            Ok(())
        } else {
            Err("IPvFuture address is invalid".to_owned())
        };
    }
    match crate::formats::ip_address_is_v4(hostname) {
        Some(false) => Ok(()),
        Some(true) => Err("An IPv4 address cannot be in brackets".to_owned()),
        None => Err(format!(
            "{} does not appear to be an IPv4 or IPv6 address",
            crate::pyrepr::repr_str(hostname)
        )),
    }
}

/// `urlparse(url, scheme)`: `urlsplit` plus the `;params` of the last path segment.
pub(crate) fn urlparse(url: &str, default_scheme: &str) -> Result<Parsed, String> {
    let mut parsed = urlsplit(url, default_scheme)?;
    if uses(USES_PARAMS, &parsed.scheme) && parsed.path.contains(';') {
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
    } else if crate::context::python_minor() <= 11 {
        // Until Python 3.12, any scheme of `uses_netloc` gets a (possibly empty) netloc.
        if url.starts_with("//") || (!parsed.scheme.is_empty() && uses(USES_NETLOC, &parsed.scheme))
        {
            if !url.is_empty() && !url.starts_with('/') {
                url.insert(0, '/');
            }
            url = format!("//{url}");
        }
    } else if url.starts_with("//")
        || (!parsed.scheme.is_empty()
            && uses(USES_NETLOC, &parsed.scheme)
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

/// `urljoin(base, url)`, as the Python version being followed joins.
pub(crate) fn urljoin(base: &str, url: &str) -> Result<String, String> {
    if crate::context::python_minor() >= 14 {
        urljoin_314(base, url)
    } else {
        urljoin_313(base, url)
    }
}

/// `urljoin` up to Python 3.13: more `..` segments than the base has directories remove
/// the root too (`file:x`), and an empty query or fragment is dropped.
fn urljoin_313(base: &str, url: &str) -> Result<String, String> {
    if base.is_empty() {
        return Ok(url.to_owned());
    }
    if url.is_empty() {
        return Ok(base.to_owned());
    }
    let b = urlparse(base, "")?;
    let mut u = urlparse(url, &b.scheme)?;
    if u.scheme != b.scheme || !uses(USES_RELATIVE, &u.scheme) {
        return Ok(url.to_owned());
    }
    if uses(USES_NETLOC, &u.scheme) {
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
    if crate::context::python_minor() >= 14 {
        let mut split = Split::of(url)?;
        let fragment = split.fragment.take().unwrap_or_default();
        return Ok((split.join(), fragment));
    }
    let mut parsed = urlparse(url, "")?;
    let fragment = std::mem::take(&mut parsed.fragment);
    Ok((urlunparse(&parsed), fragment))
}

/// Python 3.14's `_urlsplit(url)`: components a URL does not have are `None`, so that an
/// empty query or fragment (`x?`, `x#`) survives a split and a join.
struct Split {
    scheme: Option<String>,
    netloc: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

impl Split {
    fn of(url: &str) -> Result<Self, String> {
        let url: String = url
            .trim_start_matches(is_c0_or_space)
            .chars()
            .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
            .collect();
        let mut scheme = None;
        let mut rest = url.as_str();
        if let Some(i) = url.find(':') {
            if i > 0
                && url.as_bytes()[0].is_ascii_alphabetic()
                && url[..i].chars().all(is_scheme_char)
            {
                scheme = Some(url[..i].to_ascii_lowercase());
                rest = &url[i + 1..];
            }
        }
        let mut netloc = None;
        if let Some(after) = rest.strip_prefix("//") {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            check_brackets(&after[..end])?;
            netloc = Some(after[..end].to_owned());
            rest = &after[end..];
        }
        let (rest, fragment) = match rest.split_once('#') {
            Some((rest, fragment)) => (rest, Some(fragment.to_owned())),
            None => (rest, None),
        };
        let (path, query) = match rest.split_once('?') {
            Some((path, query)) => (path, Some(query.to_owned())),
            None => (rest, None),
        };
        Ok(Self {
            scheme,
            netloc,
            path: path.to_owned(),
            query,
            fragment,
        })
    }

    /// `_urlunsplit`.
    fn join(&self) -> String {
        let mut url = match &self.netloc {
            Some(netloc) => {
                let slash = if !self.path.is_empty() && !self.path.starts_with('/') {
                    "/"
                } else {
                    ""
                };
                format!("//{netloc}{slash}{}", self.path)
            }
            None if self.path.starts_with("//") => format!("//{}", self.path),
            None => self.path.clone(),
        };
        if let Some(scheme) = self.scheme.as_deref().filter(|scheme| !scheme.is_empty()) {
            url = format!("{scheme}:{url}");
        }
        if let Some(query) = &self.query {
            url = format!("{url}?{query}");
        }
        if let Some(fragment) = &self.fragment {
            url = format!("{url}#{fragment}");
        }
        url
    }
}

/// `urljoin` from Python 3.14: the root survives any number of `..` segments, an empty
/// query or fragment is kept, and `;params` are part of the path.
fn urljoin_314(base: &str, url: &str) -> Result<String, String> {
    if base.is_empty() {
        return Ok(url.to_owned());
    }
    if url.is_empty() {
        return Ok(base.to_owned());
    }
    let b = Split::of(base)?;
    let mut u = Split::of(url)?;
    if u.scheme.is_none() {
        u.scheme.clone_from(&b.scheme);
    }
    let scheme = u.scheme.as_deref().unwrap_or_default();
    if u.scheme != b.scheme || (!scheme.is_empty() && !uses(USES_RELATIVE, scheme)) {
        return Ok(url.to_owned());
    }
    if scheme.is_empty() || uses(USES_NETLOC, scheme) {
        if u.netloc.as_deref().is_some_and(|netloc| !netloc.is_empty()) {
            return Ok(u.join());
        }
        u.netloc.clone_from(&b.netloc);
    }
    if u.path.is_empty() {
        u.path.clone_from(&b.path);
        if u.query.is_none() {
            u.query.clone_from(&b.query);
            if u.fragment.is_none() {
                u.fragment.clone_from(&b.fragment);
            }
        }
        return Ok(u.join());
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
    Ok(u.join())
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
/// As Python, a host counts as this machine when its address is one of this machine's,
/// which takes a DNS lookup for other names than `localhost` and IP addresses.
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
        if parsed.scheme != "file" || !is_local_authority(&parsed.netloc) {
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
        if selector.starts_with("//")
            && !selector[2..].starts_with('/')
            && host != "localhost"
            && !local_addresses()
                .iter()
                .any(|address| address.to_string() == host)
        {
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
    // `url2pathname`; until Python 3.12 a plain `unquote`, whose UTF-8 decoding replaces
    // what is not UTF-8.
    if python_minor <= 11 {
        let path = String::from_utf8_lossy(&unquote_bytes(&selector)).into_owned();
        return Some(path.into_bytes());
    }
    let selector = if selector.starts_with("///") {
        &selector[2..]
    } else if selector.starts_with("//localhost/") {
        &selector[11..]
    } else {
        selector.as_str()
    };
    Some(unquote_bytes(selector))
}

/// `_safe_gethostbyname(host) in FileHandler().get_names()`: the host's address is one of
/// this machine's.
fn is_local_host(host: &str) -> bool {
    ipv4_address(host).is_some_and(|address| local_addresses().contains(&address))
}

/// Python 3.14's `_is_local_authority(authority, resolve=True)`: also this machine's name
/// as it is, even when it does not resolve.
fn is_local_authority(authority: &str) -> bool {
    authority.is_empty()
        || authority == "localhost"
        || hostname().as_deref() == Some(authority)
        || is_local_host(authority)
}

/// `socket.gethostbyname(host)`: an IPv4 address of `host`.
fn ipv4_address(host: &str) -> Option<std::net::Ipv4Addr> {
    ipv4_addresses(host).into_iter().next()
}

/// `socket.gethostbyname_ex(host)[2]`.
fn ipv4_addresses(host: &str) -> Vec<std::net::Ipv4Addr> {
    use std::net::{SocketAddr, ToSocketAddrs};
    let mut addresses = Vec::new();
    for address in (host, 0).to_socket_addrs().into_iter().flatten() {
        if let SocketAddr::V4(address) = address {
            if !addresses.contains(address.ip()) {
                addresses.push(*address.ip());
            }
        }
    }
    addresses
}

/// `FileHandler.get_names()`: the addresses of `localhost` and of this machine's name,
/// looked up once.
fn local_addresses() -> &'static [std::net::Ipv4Addr] {
    static NAMES: std::sync::OnceLock<Vec<std::net::Ipv4Addr>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = ipv4_addresses("localhost");
        names.extend(hostname().map_or_else(Vec::new, |name| ipv4_addresses(&name)));
        names
    })
}

/// `socket.gethostname()`.
#[cfg(unix)]
fn hostname() -> Option<String> {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is writable for its whole length, which is passed along.
    let found = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
    let end = buffer.iter().position(|&b| b == 0)?;
    found.then(|| String::from_utf8_lossy(&buffer[..end]).into_owned())
}

#[cfg(not(unix))]
fn hostname() -> Option<String> {
    None
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
        // What `urlopen` opens on Python 3.11 (as 3.10), 3.13 (as 3.12) and 3.14.
        let cases: [(&str, [Option<&str>; 3]); 20] = [
            (
                "file:///d/b.json",
                [Some("/d/b.json"), Some("/d/b.json"), Some("/d/b.json")],
            ),
            (
                "file:///d/b.json?x=1",
                [
                    Some("/d/b.json?x=1"),
                    Some("/d/b.json?x=1"),
                    Some("/d/b.json"),
                ],
            ),
            (
                "file:///d/q%3F.json",
                [Some("/d/q?.json"), Some("/d/q?.json"), Some("/d/q?.json")],
            ),
            (
                "file:///d/p%2541.json",
                [
                    Some("/d/p%41.json"),
                    Some("/d/p%41.json"),
                    Some("/d/p%41.json"),
                ],
            ),
            (
                "file://localhost/d/b.json",
                [Some("/d/b.json"), Some("/d/b.json"), Some("/d/b.json")],
            ),
            (
                "file://LOCALHOST/d/b.json",
                [Some("/d/b.json"), Some("/d/b.json"), Some("/d/b.json")],
            ),
            (
                "file://127.0.0.1/d/b.json",
                [Some("/d/b.json"), Some("/d/b.json"), Some("/d/b.json")],
            ),
            ("file://otherhost/d/b.json", [None, None, None]),
            ("file://localhost:80/d/b.json", [None, None, None]),
            (
                "file:/d/b.json",
                [Some("/d/b.json"), Some("/d/b.json"), Some("/d/b.json")],
            ),
            (
                "file:etc/passwd",
                [Some("etc/passwd"), Some("etc/passwd"), Some("etc/passwd")],
            ),
            (
                "file:////d/b.json",
                [Some("//d/b.json"), Some("//d/b.json"), Some("//d/b.json")],
            ),
            (
                "file://///d/b.json",
                [Some("///d/b.json"), Some("/d/b.json"), Some("///d/b.json")],
            ),
            (
                "file:///d/b.json?",
                [Some("/d/b.json?"), Some("/d/b.json?"), Some("/d/b.json")],
            ),
            (
                "file:///d/%C3%A9.json",
                [Some("/d/é.json"), Some("/d/é.json"), Some("/d/é.json")],
            ),
            (
                "file:////localhost/d/b.json",
                [
                    Some("//localhost/d/b.json"),
                    Some("/d/b.json"),
                    Some("//localhost/d/b.json"),
                ],
            ),
            (
                "file://localhost//d/b.json",
                [Some("//d/b.json"), Some("//d/b.json"), Some("//d/b.json")],
            ),
            (
                "file://localhost///d/b.json",
                [Some("///d/b.json"), Some("/d/b.json"), Some("///d/b.json")],
            ),
            (
                "file:///d/%2Fx.json",
                [Some("/d//x.json"), Some("/d//x.json"), Some("/d//x.json")],
            ),
            (
                "file:///d/b.json#f",
                [Some("/d/b.json"), Some("/d/b.json"), Some("/d/b.json")],
            ),
        ];
        for (url, [py311, py313, py314]) in cases {
            let path =
                |minor| file_url_path(url, minor).map(|bytes| String::from_utf8(bytes).unwrap());
            assert_eq!(path(11).as_deref(), py311, "3.11: {url}");
            assert_eq!(path(12).as_deref(), py313, "3.12: {url}");
            assert_eq!(path(13).as_deref(), py313, "3.13: {url}");
            assert_eq!(path(14).as_deref(), py314, "3.14: {url}");
        }
        // Undecodable escapes stay the bytes they stand for, from Python 3.12; before, each
        // invalid sequence becomes U+FFFD.
        for minor in [12, 13, 14] {
            assert_eq!(
                file_url_path("file:///d/%E9%FF.json", minor),
                Some(b"/d/\xe9\xff.json".to_vec())
            );
        }
        assert_eq!(
            file_url_path("file:///d/%E9%FF.json", 11),
            Some("/d/\u{fffd}\u{fffd}.json".as_bytes().to_vec())
        );
    }

    /// The fixtures of `scripts/urljoin_cases.py` for Python 3.10 to 3.14, by minor version.
    const FIXTURES: [(u32, &str, &str); 5] = [
        (
            10,
            include_str!("../tests/fixtures/urljoin-3.10.json"),
            include_str!("../tests/fixtures/urldefrag-3.10.json"),
        ),
        (
            11,
            include_str!("../tests/fixtures/urljoin-3.11.json"),
            include_str!("../tests/fixtures/urldefrag-3.11.json"),
        ),
        (
            12,
            include_str!("../tests/fixtures/urljoin-3.12.json"),
            include_str!("../tests/fixtures/urldefrag-3.12.json"),
        ),
        (
            13,
            include_str!("../tests/fixtures/urljoin-3.13.json"),
            include_str!("../tests/fixtures/urldefrag-3.13.json"),
        ),
        (
            14,
            include_str!("../tests/fixtures/urljoin-3.14.json"),
            include_str!("../tests/fixtures/urldefrag-3.14.json"),
        ),
    ];

    /// What CPython returned: the value, or `{"ValueError": message}`.
    fn outcome<T: Into<Value>>(result: Result<T, String>) -> Value {
        match result {
            Ok(value) => value.into(),
            Err(message) => serde_json::json!({ "ValueError": message }),
        }
    }

    /// `[base, url, result]` triples computed by CPython.
    fn check_urljoin(fixture: &str) {
        let cases: Vec<(String, String, Value)> = serde_json::from_str(fixture).unwrap();
        assert!(cases.len() > 1000);
        for (base, url, expected) in cases {
            let actual = outcome(urljoin(&base, &url));
            assert_eq!(actual, expected, "urljoin({base:?}, {url:?})");
        }
    }

    fn check_urldefrag(fixture: &str) {
        let cases: Vec<(String, Value)> = serde_json::from_str(fixture).unwrap();
        assert!(cases.len() > 100);
        for (url, expected) in cases {
            let actual = outcome(urldefrag(&url).map(|(u, f)| vec![u, f]));
            assert_eq!(actual, expected, "urldefrag({url:?})");
        }
    }

    #[test]
    fn this_machine_counts_as_local() {
        // An address of this machine.
        assert_eq!(
            file_url_path("file://127.0.0.1/d/b.json", 13),
            Some(b"/d/b.json".to_vec())
        );
        // Its name, as it is, from Python 3.14; before, when it resolves to one of its
        // addresses.
        if let Some(name) = hostname() {
            let url = format!("file://{name}/d/b.json");
            assert_eq!(file_url_path(&url, 14), Some(b"/d/b.json".to_vec()));
        }
    }

    #[test]
    fn urljoin_matches_cpython() {
        for (minor, urljoin, _) in FIXTURES {
            crate::context::with_python_minor(minor, || check_urljoin(urljoin));
        }
    }

    #[test]
    fn urldefrag_matches_cpython() {
        for (minor, _, urldefrag) in FIXTURES {
            crate::context::with_python_minor(minor, || check_urldefrag(urldefrag));
        }
    }
}

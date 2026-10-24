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
    use std::fmt;
    use std::sync::OnceLock;
    use std::time::Duration;

    use ureq::Agent;
    use ureq::tls::{RootCerts, TlsConfig};

    use super::{Retrieve, RetrieveError};

    /// The timeout jsonschema-path's handlers pass to `urlopen` and `requests.get`.
    const TIMEOUT: Duration = Duration::from_secs(10);

    /// The built-in [`Retrieve`] implementation: plain `GET` requests that behave like
    /// upstream's `urllib` handler.
    ///
    /// - Each socket operation (connecting, sending, every read) times out after 10
    ///   seconds, as `urlopen(url, timeout=10)` does; see [`HttpRetriever::with_timeout`].
    /// - Redirects are followed, up to 10 of them, and any status other than success is
    ///   a failure.
    /// - Certificates are checked against the operating system's certificate store, as
    ///   Python's default SSL context does.
    /// - Proxies come from the `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` and `NO_PROXY`
    ///   environment variables (lower-case too), which `urllib` reads as well.
    /// - The whole body is read, whatever its size, and no `Accept-Encoding` is sent.
    ///
    /// The underlying HTTP agent is created on first use and shared by clones.
    #[derive(Clone)]
    pub struct HttpRetriever {
        timeout: Duration,
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
                agent: OnceLock::new(),
            }
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
                    .tls_config(tls)
                    .build();
                Agent::new_with_config(config)
            })
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
            let failed = |error: ureq::Error| RetrieveError::new(error.to_string());
            let mut response = self.agent().get(url).call().map_err(failed)?;
            // `read_to_vec` stops at 10 MB by default; `urlopen` reads everything.
            response
                .body_mut()
                .with_config()
                .limit(u64::MAX)
                .read_to_vec()
                .map_err(failed)
        }
    }
}

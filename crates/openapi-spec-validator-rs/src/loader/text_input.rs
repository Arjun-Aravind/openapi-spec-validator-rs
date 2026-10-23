//! Emulation of PyYAML reading a text stream (`sys.stdin`).
//!
//! PyYAML's `CParser.input_handler` calls `stream.read(size)` (where `size` is the byte count
//! libyaml asked for, but `read` counts *characters*), encodes the result as UTF-8 and hands it
//! to libyaml in pieces. `sys.stdin` on POSIX is a `TextIOWrapper` with `newline="\n"` (no
//! newline translation), UTF-8, and an error handler that depends on the locale: `strict`
//! under a UTF-8 locale, `surrogateescape` under the C/POSIX locale. Valid UTF-8 therefore
//! reaches libyaml unchanged; only the chunking and the failure modes differ from binary input.

use super::pyfmt::unicode_decode_error;

/// How `sys.stdin` decodes undecodable bytes (`sys.stdin.errors`).
///
/// CPython picks `surrogateescape` when UTF-8 mode is on (`PYTHONUTF8=1`) or the `LC_CTYPE`
/// locale is `C`, `POSIX`, `C.UTF-8`, `C.utf8` or `UTF-8`, and `strict` otherwise (unless
/// `PYTHONIOENCODING` says differently). Non-UTF-8 locale encodings are not modelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextErrors {
    /// `errors="strict"`: reading fails with `UnicodeDecodeError`, positioned within the
    /// `TextIOWrapper` chunk being decoded (assuming `read()` calls return full chunks, as for a
    /// redirected file).
    Strict,
    /// `errors="surrogateescape"`: bad bytes become lone surrogates and PyYAML's UTF-8
    /// re-encoding fails with `UnicodeEncodeError`.
    SurrogateEscape,
}

/// `TextIOWrapper._CHUNK_SIZE`.
const CHUNK_SIZE: usize = 8192;

pub(super) struct TextFeed<'a> {
    raw: &'a [u8],
    errors: TextErrors,
    /// Byte offset of the next character `read()` returns.
    text_pos: usize,
    /// Bytes decoded so far (strict mode); `raw[text_pos..decoded_end]` is `_decoded_chars`.
    decoded_end: usize,
    /// Bytes handed out by `buffer.read1()`; `raw[decoded_end..raw_pos]` sits in the decoder.
    raw_pos: usize,
    b2cratio: f64,
    /// PyYAML's `stream_cache`, always a contiguous range of `raw` (valid UTF-8).
    cache: Option<(usize, usize)>,
    /// The message of the Python exception that aborted reading.
    pub failure: Option<String>,
}

impl<'a> TextFeed<'a> {
    pub fn new(raw: &'a [u8], errors: TextErrors) -> Self {
        Self {
            raw,
            errors,
            text_pos: 0,
            decoded_end: 0,
            raw_pos: 0,
            b2cratio: 0.0,
            cache: None,
            failure: None,
        }
    }

    /// PyYAML's `input_handler`: returns the number of bytes written, or `None` after recording
    /// the exception that Python would raise.
    pub fn fill(&mut self, out: &mut [u8]) -> Option<usize> {
        let (start, end) = match self.cache {
            Some(range) => range,
            None => match self.read(out.len()) {
                Ok(range) => range,
                Err(message) => {
                    self.failure = Some(message);
                    return None;
                }
            },
        };
        let n = out.len().min(end - start);
        out[..n].copy_from_slice(&self.raw[start..start + n]);
        self.cache = (start + n < end).then_some((start + n, end));
        Some(n)
    }

    /// `stream.read(n)` followed by `PyUnicode_AsUTF8String`, as a byte range of `raw`.
    fn read(&mut self, n: usize) -> Result<(usize, usize), String> {
        match self.errors {
            TextErrors::Strict => self.read_strict(n),
            TextErrors::SurrogateEscape => self.read_escaped(n),
        }
    }

    fn read_strict(&mut self, n: usize) -> Result<(usize, usize), String> {
        let start = self.text_pos;
        let mut remaining = n - self.take_decoded(n);
        while remaining > 0 {
            // textiowrapper_read_chunk(self, remaining)
            let hint = (self.b2cratio.max(1.0) * remaining as f64) as usize;
            let chunk_end = self.raw.len().min(self.raw_pos + CHUNK_SIZE.max(hint));
            let nbytes = chunk_end - self.raw_pos;
            let input = &self.raw[self.decoded_end..chunk_end];
            let consumed = decode_utf8(input, nbytes == 0)?;
            let decoded = &input[..consumed];
            let nchars = decoded.iter().filter(|&&b| !is_continuation(b)).count();
            self.decoded_end += consumed;
            self.raw_pos = chunk_end;
            self.b2cratio = if nchars > 0 {
                nbytes as f64 / nchars as f64
            } else {
                0.0
            };
            if nbytes == 0 && nchars == 0 {
                break;
            }
            remaining -= self.take_decoded(remaining);
        }
        Ok((start, self.text_pos))
    }

    /// Takes up to `n` characters from `_decoded_chars`, returning how many were taken.
    fn take_decoded(&mut self, n: usize) -> usize {
        let mut taken = 0;
        let mut pos = self.text_pos;
        while taken < n && pos < self.decoded_end {
            pos += utf8_len(self.raw[pos]);
            taken += 1;
        }
        self.text_pos = pos;
        taken
    }

    fn read_escaped(&mut self, n: usize) -> Result<(usize, usize), String> {
        // Decoding cannot fail, so the chunking inside TextIOWrapper is unobservable: read(n)
        // returns the next n characters, where each undecodable byte is one lone surrogate.
        let start = self.text_pos;
        let mut pos = start;
        let mut first_bad = None;
        // From the first surrogate on: `Some(byte)` for surrogates, `None` for real characters.
        let mut tail: Vec<Option<u8>> = Vec::new();
        let mut count = 0;
        while count < n && pos < self.raw.len() {
            match valid_seq_len(&self.raw[pos..]) {
                Some(len) => {
                    if first_bad.is_some() {
                        tail.push(None);
                    }
                    pos += len;
                }
                None => {
                    first_bad.get_or_insert(count);
                    tail.push(Some(self.raw[pos]));
                    pos += 1;
                }
            }
            count += 1;
        }
        self.text_pos = pos;
        let Some(first) = first_bad else {
            return Ok((start, pos));
        };
        // The UTF-8 encoder reports the whole run of consecutive surrogates.
        let run = tail.iter().take_while(|b| b.is_some()).count();
        Err(match tail.first() {
            Some(Some(byte)) if run == 1 => format!(
                "'utf-8' codec can't encode character '\\udc{byte:02x}' in position {first}: surrogates not allowed"
            ),
            _ => format!(
                "'utf-8' codec can't encode characters in position {first}-{}: surrogates not allowed",
                first + run - 1
            ),
        })
    }
}

fn is_continuation(b: u8) -> bool {
    b & 0xc0 == 0x80
}

/// Length of the (valid) UTF-8 sequence starting with lead byte `b`.
fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Length of a valid UTF-8 sequence at the start of `bytes`, if there is one.
fn valid_seq_len(bytes: &[u8]) -> Option<usize> {
    let b = *bytes.first()?;
    if b < 0x80 {
        return Some(1);
    }
    let len = utf8_len(b);
    let candidate = bytes.get(..len)?;
    std::str::from_utf8(candidate).ok().map(|_| len)
}

/// `codecs.utf_8_decode(input, 'strict', final)`: the number of bytes consumed, or the
/// `UnicodeDecodeError` text (positions relative to `input`).
fn decode_utf8(input: &[u8], final_: bool) -> Result<usize, String> {
    match std::str::from_utf8(input) {
        Ok(_) => Ok(input.len()),
        Err(e) => {
            let start = e.valid_up_to();
            match e.error_len() {
                None if !final_ => Ok(start),
                None => Err(unicode_decode_error(
                    input,
                    start,
                    input.len(),
                    "unexpected end of data",
                )),
                Some(1)
                    if !final_
                        && input.len() - start == 2
                        && input[start] == 0xed
                        && (0xa0..=0xbf).contains(&input[start + 1]) =>
                {
                    // CPython defers a truncated surrogate encoding at the end of a non-final
                    // chunk instead of failing right away.
                    Ok(start)
                }
                Some(len) => {
                    let reason = if len == 1 && !(0xc2..=0xf4).contains(&input[start]) {
                        "invalid start byte"
                    } else {
                        "invalid continuation byte"
                    };
                    Err(unicode_decode_error(input, start, start + len, reason))
                }
            }
        }
    }
}

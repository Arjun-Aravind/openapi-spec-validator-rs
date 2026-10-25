//! A small safe wrapper over the `unsafe-libyaml` event parser.

use std::ffi::{CStr, c_char, c_void};
use std::marker::PhantomData;
use std::mem::MaybeUninit;

use unsafe_libyaml as unsafe_yaml;

use super::pyfmt::{Mark, hex04, marked_error};
use super::text_input::TextFeed;

pub(super) enum Event {
    StreamStart,
    StreamEnd,
    DocumentStart,
    DocumentEnd,
    Alias {
        anchor: String,
    },
    Scalar {
        anchor: Option<String>,
        tag: Option<String>,
        value: String,
        plain_implicit: bool,
    },
    SequenceStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    SequenceEnd,
    MappingStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    MappingEnd,
}

/// Where the parser reads from.
pub(super) enum Source<'a> {
    /// A binary stream: PyYAML hands libyaml exactly the bytes it asks for, which is what
    /// libyaml's own string reader does.
    Bytes(&'a [u8]),
    /// A text stream (`sys.stdin`), emulated chunk by chunk.
    Text(TextFeed<'a>),
}

pub(super) struct Parser<'a> {
    // Heap-allocated and only ever touched through this raw pointer: libyaml's string reader
    // keeps a pointer to the parser inside the parser itself (`read_handler_data`), which a
    // `Box` or `&mut` reborrow would invalidate under Rust's aliasing rules. Freed in `drop`.
    raw: *mut unsafe_yaml::yaml_parser_t,
    // The read handler's data, registered with libyaml and read through that pointer only;
    // owned here so it outlives the parser. Freed in `drop`.
    feed: Option<*mut TextFeed<'a>>,
    stream_name: &'a str,
    _input: PhantomData<&'a [u8]>,
}

impl<'a> Parser<'a> {
    pub fn new(source: Source<'a>, stream_name: &'a str) -> Self {
        let raw = Box::into_raw(Box::<unsafe_yaml::yaml_parser_t>::new_uninit())
            .cast::<unsafe_yaml::yaml_parser_t>();
        // SAFETY: `yaml_parser_initialize` zeroes the whole struct and allocates its buffers,
        // leaving it fully initialised. With Rust's allocator it only fails by aborting.
        unsafe {
            let _ = unsafe_yaml::yaml_parser_initialize(raw);
        }
        let mut feed = None;
        match source {
            Source::Bytes(bytes) => {
                // SAFETY: the parser is initialised and has no input yet; `bytes` outlives the
                // parser (lifetime `'a`), and libyaml only reads `bytes.len()` bytes from it.
                unsafe {
                    unsafe_yaml::yaml_parser_set_input_string(
                        raw,
                        bytes.as_ptr(),
                        bytes.len() as u64,
                    );
                }
            }
            Source::Text(text) => {
                let data = Box::into_raw(Box::new(text));
                // SAFETY: the feed is heap-allocated and owned by `self` (freed in `drop`), so
                // `data` stays valid for as long as libyaml can call `read_handler`.
                unsafe {
                    unsafe_yaml::yaml_parser_set_input(raw, read_handler, data.cast::<c_void>());
                }
                feed = Some(data);
            }
        }
        Self {
            raw,
            feed,
            stream_name,
            _input: PhantomData,
        }
    }

    /// Parses the next event, or returns the text Python would print for the failure.
    pub fn next(&mut self) -> Result<(Event, Mark), String> {
        let mut event = MaybeUninit::<unsafe_yaml::yaml_event_t>::uninit();
        // SAFETY: the parser is initialised with an input; `yaml_parser_parse` zero-fills the
        // event before doing anything else, so it is initialised whatever the outcome.
        let failed = unsafe { unsafe_yaml::yaml_parser_parse(self.raw, event.as_mut_ptr()).fail };
        if failed {
            return Err(self.error());
        }
        // SAFETY: see above; on success the event is a valid libyaml event.
        let mut event = unsafe { event.assume_init() };
        let mark = Mark {
            line: event.start_mark.line,
            column: event.start_mark.column,
        };
        // SAFETY: the event was produced by libyaml, so its type tag matches the active union
        // member and its pointers are NUL-terminated strings or null.
        let converted = unsafe { convert(&event) };
        // SAFETY: the event is valid and is not used after this call.
        unsafe { unsafe_yaml::yaml_event_delete(&mut event) };
        converted.map(|e| (e, mark))
    }

    fn error(&self) -> String {
        // SAFETY: the feed is live until `drop`, and libyaml is not running.
        if let Some(message) = self.feed.and_then(|f| unsafe { (*f).failure.clone() }) {
            // The Python exception raised inside the stream's read() propagates unchanged.
            return message;
        }
        // SAFETY: the parser is live until `drop`, and libyaml is not running.
        let p: &unsafe_yaml::yaml_parser_t = unsafe { &*self.raw };
        // SAFETY: libyaml sets `problem`/`context` to static NUL-terminated strings or null.
        let problem = unsafe { c_str(p.problem) }.map(|problem| {
            // unsafe-libyaml fixed a typo that libyaml 0.2.5 (bundled with PyYAML) still prints.
            if problem == "did not find expected hexadecimal number" {
                "did not find expected hexdecimal number".to_owned()
            } else {
                problem
            }
        });
        // SAFETY: as above.
        let context = unsafe { c_str(p.context) };
        let mark = |m: unsafe_yaml::yaml_mark_t| Mark {
            line: m.line,
            column: m.column,
        };
        match p.error {
            unsafe_yaml::YAML_MEMORY_ERROR => String::new(),
            unsafe_yaml::YAML_READER_ERROR => format!(
                "unacceptable character #x{}: {}\n  in \"{}\", position {}",
                hex04(i64::from(p.problem_value)),
                problem.unwrap_or_default(),
                self.stream_name,
                p.problem_offset
            ),
            _ => marked_error(
                self.stream_name,
                context.as_deref(),
                context.as_ref().map(|_| mark(p.context_mark)),
                Some(problem.as_deref().unwrap_or_default()),
                Some(mark(p.problem_mark)),
            ),
        }
    }
}

impl Drop for Parser<'_> {
    fn drop(&mut self) {
        // SAFETY: the parser was initialised in `new` and is deleted exactly once; the parser
        // and the feed were allocated with `Box::into_raw` in `new` and are freed exactly once,
        // after libyaml can no longer reach them.
        unsafe {
            unsafe_yaml::yaml_parser_delete(self.raw);
            drop(Box::from_raw(self.raw));
            if let Some(feed) = self.feed {
                drop(Box::from_raw(feed));
            }
        }
    }
}

/// libyaml read callback for text streams (PyYAML's `input_handler`).
///
/// # Safety
/// `data` must be the `TextFeed` registered in `Parser::new`, and `buffer` must be valid for
/// `size` bytes, as libyaml guarantees.
unsafe fn read_handler(data: *mut c_void, buffer: *mut u8, size: u64, size_read: *mut u64) -> i32 {
    // SAFETY: guaranteed by the caller contract above.
    let (feed, out) = unsafe {
        (
            &mut *data.cast::<TextFeed<'_>>(),
            std::slice::from_raw_parts_mut(buffer, usize::try_from(size).unwrap_or(0)),
        )
    };
    match feed.fill(out) {
        Some(n) => {
            // SAFETY: libyaml passes a valid out-pointer.
            unsafe { *size_read = n as u64 };
            1
        }
        None => 0,
    }
}

/// libyaml's error strings. unsafe-libyaml declares them `*const i8` on every platform,
/// while `c_char` is unsigned on some (AArch64 Linux).
///
/// # Safety
/// `p` must be null or point to a NUL-terminated string.
unsafe fn c_str(p: *const i8) -> Option<String> {
    if p.is_null() {
        None
    } else {
        // SAFETY: non-null and NUL-terminated per the contract.
        Some(
            unsafe { CStr::from_ptr(p.cast::<c_char>()) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// # Safety
/// `p` must be null or point to a NUL-terminated string.
unsafe fn yaml_str(p: *const u8) -> Result<Option<String>, String> {
    if p.is_null() {
        return Ok(None);
    }
    // SAFETY: non-null and NUL-terminated per the contract.
    let bytes = unsafe { CStr::from_ptr(p.cast::<c_char>()) }.to_bytes();
    utf8(bytes.to_vec()).map(Some)
}

/// PyYAML decodes libyaml strings with `'strict'` UTF-8; libyaml only produces valid UTF-8,
/// but keep the failure path faithful rather than unsound.
fn utf8(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|e| {
        let bytes = e.as_bytes();
        let start = e.utf8_error().valid_up_to();
        let len = e.utf8_error().error_len().unwrap_or(bytes.len() - start);
        let reason = if e.utf8_error().error_len().is_none() {
            "unexpected end of data"
        } else if len == 1 && !(0xc2..=0xf4).contains(&bytes[start]) {
            "invalid start byte"
        } else {
            "invalid continuation byte"
        };
        super::pyfmt::unicode_decode_error(bytes, start, start + len, reason)
    })
}

/// # Safety
/// `event` must be a valid event produced by `yaml_parser_parse`.
unsafe fn convert(event: &unsafe_yaml::yaml_event_t) -> Result<Event, String> {
    // SAFETY (whole body): the union member read always matches `event.type_`.
    unsafe {
        Ok(match event.type_ {
            unsafe_yaml::YAML_STREAM_START_EVENT => Event::StreamStart,
            unsafe_yaml::YAML_STREAM_END_EVENT => Event::StreamEnd,
            unsafe_yaml::YAML_DOCUMENT_START_EVENT => Event::DocumentStart,
            unsafe_yaml::YAML_DOCUMENT_END_EVENT => Event::DocumentEnd,
            unsafe_yaml::YAML_ALIAS_EVENT => Event::Alias {
                anchor: yaml_str(event.data.alias.anchor)?.unwrap_or_default(),
            },
            unsafe_yaml::YAML_SCALAR_EVENT => {
                let s = event.data.scalar;
                let len = usize::try_from(s.length).unwrap_or(0);
                let value = if len == 0 || s.value.is_null() {
                    String::new()
                } else {
                    utf8(std::slice::from_raw_parts(s.value, len).to_vec())?
                };
                Event::Scalar {
                    anchor: yaml_str(s.anchor)?,
                    tag: yaml_str(s.tag)?,
                    value,
                    plain_implicit: s.plain_implicit,
                }
            }
            unsafe_yaml::YAML_SEQUENCE_START_EVENT => Event::SequenceStart {
                anchor: yaml_str(event.data.sequence_start.anchor)?,
                tag: yaml_str(event.data.sequence_start.tag)?,
            },
            unsafe_yaml::YAML_SEQUENCE_END_EVENT => Event::SequenceEnd,
            unsafe_yaml::YAML_MAPPING_START_EVENT => Event::MappingStart {
                anchor: yaml_str(event.data.mapping_start.anchor)?,
                tag: yaml_str(event.data.mapping_start.tag)?,
            },
            unsafe_yaml::YAML_MAPPING_END_EVENT => Event::MappingEnd,
            // libyaml only returns an empty event after the stream has ended; the composer
            // never asks for one.
            _ => Event::StreamEnd,
        })
    }
}

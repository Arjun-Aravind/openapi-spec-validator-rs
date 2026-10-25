//! Reading a stream only as far as PyYAML's reader would get.
//!
//! PyYAML parses a stream as it reads it, so a document that libyaml cannot even decode
//! fails at the first offending bytes, however much follows them: upstream's handlers
//! give up on `/dev/zero` after one buffer. The bytes libyaml rejects while decoding (a
//! sequence that is not UTF-8 or UTF-16, or a character YAML does not allow) end the
//! document as far as the parser is concerned: nothing after them can change the
//! outcome, since the scanner never gets past them. Reading stops there, so that such a
//! stream never has to fit in memory. Everything else is read whole.

use std::io::{self, ErrorKind, Read};

/// How much is read at a time.
const CHUNK_SIZE: usize = 64 * 1024;

/// Appends the content of `reader` (a binary stream) to `into` up to its end, or up to
/// and including the first bytes libyaml would refuse to decode (see the module
/// documentation). The encoding is detected as libyaml detects it: UTF-16 after a
/// byte-order mark, UTF-8 otherwise.
pub fn read_until_unreadable(reader: &mut dyn Read, into: &mut Vec<u8>) -> io::Result<()> {
    read(reader, into, false)
}

/// [`read_until_unreadable`] for the bytes behind a text stream (`sys.stdin`), which
/// Python decodes as UTF-8 before PyYAML sees them: a byte-order mark is no reason to
/// read UTF-16.
pub fn read_text_until_unreadable(reader: &mut dyn Read, into: &mut Vec<u8>) -> io::Result<()> {
    read(reader, into, true)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

fn read(reader: &mut dyn Read, into: &mut Vec<u8>, text: bool) -> io::Result<()> {
    let start = into.len();
    // Bytes from `start` on that are known to be clean; what follows them is at most an
    // incomplete character waiting for the rest of its bytes.
    let mut checked = start;
    let mut encoding = text.then_some(Encoding::Utf8);
    loop {
        crate::context::check();
        // Read straight into the buffer's spare room.
        let len = into.len();
        into.try_reserve(CHUNK_SIZE)
            .map_err(|_| io::Error::new(ErrorKind::OutOfMemory, "out of memory"))?;
        into.resize(len + CHUNK_SIZE, 0);
        let read = match reader.read(&mut into[len..]) {
            Ok(0) => {
                into.truncate(len);
                return Ok(());
            }
            Ok(read) => read,
            Err(error) if error.kind() == ErrorKind::Interrupted => {
                into.truncate(len);
                continue;
            }
            Err(error) => {
                into.truncate(len);
                return Err(error);
            }
        };
        into.truncate(len + read);
        if encoding.is_none() && into.len() - start >= 2 {
            // `yaml_parser_determine_encoding`; the BOM is skipped.
            encoding = Some(match into[start..start + 2] {
                [0xFF, 0xFE] => {
                    checked = start + 2;
                    Encoding::Utf16Le
                }
                [0xFE, 0xFF] => {
                    checked = start + 2;
                    Encoding::Utf16Be
                }
                _ => Encoding::Utf8,
            });
        }
        let clean = match encoding {
            None => continue,
            Some(Encoding::Utf8) => clean_prefix(&into[checked..]),
            Some(Encoding::Utf16Le) => clean_utf16_prefix(&into[checked..], u16::from_le_bytes),
            Some(Encoding::Utf16Be) => clean_utf16_prefix(&into[checked..], u16::from_be_bytes),
        };
        match clean {
            Some(clean) => checked += clean,
            None => return Ok(()),
        }
    }
}

/// [`clean_prefix`] for UTF-16 (`yaml_parser_update_buffer`): a lone low surrogate, a
/// high surrogate not followed by a low one, or a character YAML does not allow is
/// refused; a unit or surrogate pair cut off by the end is left for later.
fn clean_utf16_prefix(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Option<usize> {
    let mut at = 0;
    while at + 2 <= bytes.len() {
        let value = unit([bytes[at], bytes[at + 1]]);
        let (code, width) = match value & 0xFC00 {
            0xDC00 => return None,
            0xD800 => {
                if at + 4 > bytes.len() {
                    break;
                }
                let low = unit([bytes[at + 2], bytes[at + 3]]);
                if low & 0xFC00 != 0xDC00 {
                    return None;
                }
                (
                    0x10000 + ((u32::from(value) & 0x3FF) << 10) + (u32::from(low) & 0x3FF),
                    4,
                )
            }
            _ => (u32::from(value), 2),
        };
        if !libyaml_accepts(code) {
            return None;
        }
        at += width;
    }
    Some(at)
}

/// The characters libyaml's reader lets through: tab, line feed, carriage return,
/// printable ASCII, NEL and the non-control Unicode ranges.
fn libyaml_accepts(code: u32) -> bool {
    matches!(code, 0x09 | 0x0A | 0x0D | 0x20..=0x7E | 0x85 | 0xA0..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

/// How many leading bytes of `bytes` libyaml decodes without complaint, leaving out an
/// incomplete trailing sequence; `None` when it would refuse something in there.
fn clean_prefix(bytes: &[u8]) -> Option<usize> {
    let (valid, definite_error) = match std::str::from_utf8(bytes) {
        Ok(_) => (bytes, false),
        // A sequence cut off by the end of the chunk may still be completed.
        Err(error) => (&bytes[..error.valid_up_to()], error.error_len().is_some()),
    };
    if definite_error || has_control_character(valid) {
        return None;
    }
    Some(valid.len())
}

/// `yaml_parser_update_buffer`'s "control characters are not allowed" check on valid
/// UTF-8: everything outside tab, line feed, carriage return, printable ASCII, NEL and
/// the non-control Unicode ranges, which in UTF-8 is a C0 control or DEL byte, a C1
/// control (`C2 80`-`C2 9F` but NEL) or U+FFFE / U+FFFF (`EF BF BE` / `EF BF BF`).
fn has_control_character(text: &[u8]) -> bool {
    const BLOCK: usize = 64;
    // A branch-free pass over each block, which the compiler vectorises, marks blocks
    // holding a byte that may start a rejected character; only those are looked at.
    let suspect = |b: u8| -> u8 {
        let c0 = u8::from(b < 0x20) & u8::from(!matches!(b, b'\t' | b'\n' | b'\r'));
        c0 | u8::from(b == 0x7F) | u8::from(b == 0xC2) | u8::from(b == 0xEF)
    };
    let mut at = 0;
    while at < text.len() {
        let block = &text[at..text.len().min(at + BLOCK)];
        if block.iter().fold(0, |acc, &b| acc | suspect(b)) != 0
            && rejected_in(text, at, block.len())
        {
            return true;
        }
        at += BLOCK;
    }
    false
}

/// Whether a rejected character starts in `text[at..at + len]`.
fn rejected_in(text: &[u8], at: usize, len: usize) -> bool {
    let end = (at + len).min(text.len());
    (at..end).any(|i| match text[i] {
        0x7F => true,
        b if b < 0x20 => !matches!(b, b'\t' | b'\n' | b'\r'),
        0xC2 => matches!(text.get(i + 1), Some(0x80..=0x84 | 0x86..=0x9F)),
        0xEF => text.get(i + 1) == Some(&0xBF) && matches!(text.get(i + 2), Some(0xBE | 0xBF)),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that repeats `pattern` forever.
    struct Endless<'a> {
        pattern: &'a [u8],
        at: usize,
    }

    impl Read for Endless<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            for (i, byte) in buf.iter_mut().enumerate() {
                *byte = self.pattern[(self.at + i) % self.pattern.len()];
            }
            self.at += buf.len();
            Ok(buf.len())
        }
    }

    /// A stream handed out one byte per `read`.
    struct ByteByByte<'a>(&'a [u8]);

    impl Read for ByteByByte<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.split_first() {
                Some((first, rest)) if !buf.is_empty() => {
                    buf[0] = *first;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    fn read(reader: &mut dyn Read) -> Vec<u8> {
        let mut bytes = Vec::new();
        read_until_unreadable(reader, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn endless_streams_stop_at_the_first_unreadable_bytes() {
        assert_eq!(
            read(&mut Endless {
                pattern: b"\0",
                at: 0
            })
            .len(),
            CHUNK_SIZE
        );
        assert_eq!(
            read(&mut Endless {
                pattern: b"\xff",
                at: 0
            })
            .len(),
            CHUNK_SIZE
        );
        // DEL and C1 controls are rejected like C0 ones.
        assert_eq!(
            read(&mut Endless {
                pattern: b"a\x7f",
                at: 0
            })
            .len(),
            CHUNK_SIZE
        );
        assert_eq!(
            read(&mut Endless {
                pattern: "é\u{85}\u{9f}".as_bytes(),
                at: 0
            })
            .len(),
            CHUNK_SIZE
        );
        // A clean stream delivered a byte at a time: a multi-byte sequence split across
        // reads is not an error.
        let text = "openapi: 3.0.0 # 日本語 \u{85}\u{FEFF}".repeat(3);
        assert_eq!(read(&mut ByteByByte(text.as_bytes())), text.as_bytes());
        // The bad bytes are kept, so the parser meets them and reports them.
        let bad = b"openapi: \x00\x01 rest";
        assert_eq!(read(&mut ByteByByte(bad)), b"openapi: \x00");
        assert_eq!(read(&mut ByteByByte(b"a\xe6\x97b")), b"a\xe6\x97b");
    }

    fn utf16(bom: bool, text: &str) -> Vec<u8> {
        let units = text.encode_utf16().flat_map(u16::to_le_bytes);
        if bom {
            [0xFF, 0xFE].into_iter().chain(units).collect()
        } else {
            units.collect()
        }
    }

    #[test]
    fn utf16_streams_stop_where_libyaml_would() {
        let clean = utf16(true, "a: 1 # \u{1F600} \u{85}\n");
        assert_eq!(read(&mut ByteByByte(&clean)), clean);
        let big: Vec<u8> = utf16(true, &"key: value\n".repeat(20_000));
        assert_eq!(read(&mut io::Cursor::new(&big)), big);
        // An endless stream of NULs after the mark stops after one chunk.
        let mut endless = Endless {
            pattern: b"\0",
            at: 0,
        };
        let mut bytes = vec![0xFF, 0xFE];
        read_until_unreadable(&mut endless, &mut bytes).unwrap();
        assert!(bytes.len() <= 2 + CHUNK_SIZE);
        // Big-endian, and a lone low surrogate or an unpaired high one.
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("ok".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(read(&mut ByteByByte(&be)), be);
        assert_eq!(clean_utf16_prefix(&[0x00, 0xDC], u16::from_le_bytes), None);
        assert_eq!(
            clean_utf16_prefix(&[0x00, 0xD8, 0x41, 0x00], u16::from_le_bytes),
            None
        );
        assert_eq!(
            clean_utf16_prefix(&[0x41, 0x00, 0x00, 0xD8], u16::from_le_bytes),
            Some(2)
        );
        assert_eq!(
            clean_utf16_prefix(&[0x41, 0x00, 0x00], u16::from_le_bytes),
            Some(2)
        );
        assert_eq!(clean_utf16_prefix(&[0x07, 0x00], u16::from_le_bytes), None);
        // Text streams are UTF-8 whatever their first bytes: the mark is an invalid
        // sequence there.
        let mut bytes = Vec::new();
        read_text_until_unreadable(&mut io::Cursor::new(&big), &mut bytes).unwrap();
        assert!(bytes.len() < big.len());
    }

    #[test]
    fn clean_prefix_leaves_incomplete_sequences_for_later() {
        assert_eq!(clean_prefix(b"abc"), Some(3));
        assert_eq!(clean_prefix(b"abc\xe6\x97"), Some(3));
        assert_eq!(clean_prefix(b"abc\xe6\x97\xa5"), Some(6));
        assert_eq!(clean_prefix(b"abc\xe6\x97x"), None);
        assert_eq!(clean_prefix(b"ab\x0bc"), None);
        assert_eq!(clean_prefix("\u{FFFE}".as_bytes()), None);
        assert_eq!(clean_prefix("\u{FFFF}".as_bytes()), None);
        assert_eq!(clean_prefix("\u{FFFD}".as_bytes()), Some(3));
        assert_eq!(clean_prefix("a\u{9F}".as_bytes()), None);
        assert_eq!(clean_prefix("a\u{85}\u{A0}".as_bytes()), Some(5));
        assert_eq!(clean_prefix("\u{10FFFF}".as_bytes()), Some(4));
        assert_eq!(clean_prefix(b""), Some(0));
        // Across the scanning blocks' boundaries.
        for pad in [60, 61, 62, 63, 64, 65, 126, 127, 128] {
            let mut text = vec![b'a'; pad];
            text.extend_from_slice("\u{9F}".as_bytes());
            assert_eq!(clean_prefix(&text), None, "{pad}");
            let mut text = vec![b'a'; pad];
            text.extend_from_slice("\u{FFFE}".as_bytes());
            assert_eq!(clean_prefix(&text), None, "{pad}");
            let mut text = vec![b'a'; pad];
            text.extend_from_slice("\t\u{85}\u{FFFD}".as_bytes());
            assert_eq!(clean_prefix(&text), Some(text.len()), "{pad}");
        }
    }
}

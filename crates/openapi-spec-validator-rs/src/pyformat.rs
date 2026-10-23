//! `string.Formatter().parse()` field names, ported from CPython 3.13
//! `Objects/stringlib/unicode_format.h` (`MarkupIterator_next` / `parse_field`).
//!
//! Upstream extracts path-template parameters with
//! `[item[1] for item in string.Formatter().parse(url)]`.

/// `[item[1] for item in string.Formatter().parse(s)]`: the field name of each parsed item
/// (`None` for pure-literal items, `Some("")` for `{}`).
///
/// `Err` carries the message of the `ValueError` CPython raises while iterating.
pub fn formatter_field_names(s: &str) -> Result<Vec<Option<String>>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut pos = 0;
    let mut names = Vec::new();
    while pos < chars.len() {
        names.push(next_item(&chars, &mut pos)?);
    }
    Ok(names)
}

/// `MarkupIterator_next`: consume one (literal, field) item, returning its field name.
fn next_item(chars: &[char], pos: &mut usize) -> Result<Option<String>, String> {
    let end = chars.len();
    // Literal text runs up to the first '{' or '}'.
    let mut c = '\0';
    let mut markup_follows = false;
    while *pos < end {
        c = chars[*pos];
        *pos += 1;
        if c == '{' || c == '}' {
            markup_follows = true;
            break;
        }
    }
    let at_end = *pos >= end;
    if c == '}' && (at_end || chars[*pos] != '}') {
        return Err("Single '}' encountered in format string".to_owned());
    }
    if at_end && c == '{' {
        return Err("Single '{' encountered in format string".to_owned());
    }
    if !at_end && chars[*pos] == c {
        // "{{" / "}}": an escaped brace ends this literal-only item.
        *pos += 1;
        markup_follows = false;
    }
    if !markup_follows {
        return Ok(None);
    }
    parse_field(chars, pos).map(Some)
}

/// `parse_field`: read `name[!conv][:spec]}` after an opening '{'; returns the field name.
fn parse_field(chars: &[char], pos: &mut usize) -> Result<String, String> {
    let end = chars.len();
    let name_start = *pos;
    let mut c = '\0';
    while *pos < end {
        c = chars[*pos];
        *pos += 1;
        match c {
            '{' => return Err("unexpected '{' in field name".to_owned()),
            // An index skips everything up to (not past) the next ']'.
            '[' => {
                while *pos < end && chars[*pos] != ']' {
                    *pos += 1;
                }
            }
            '}' | ':' | '!' => break,
            _ => {}
        }
    }
    let name: String = chars[name_start..*pos - 1].iter().collect();
    match c {
        '!' | ':' => {
            if c == '!' {
                if *pos >= end {
                    return Err("end of string while looking for conversion specifier".to_owned());
                }
                // The conversion character itself is not validated here.
                *pos += 1;
                if *pos < end {
                    let next = chars[*pos];
                    *pos += 1;
                    if next == '}' {
                        return Ok(name);
                    }
                    if next != ':' {
                        return Err("expected ':' after conversion specifier".to_owned());
                    }
                }
            }
            // Format spec: nested braces are counted until the matching '}'.
            let mut depth = 1;
            while *pos < end {
                let c = chars[*pos];
                *pos += 1;
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(name);
                    }
                }
            }
            Err("unmatched '{' in format spec".to_owned())
        }
        '}' => Ok(name),
        _ => Err("expected '}' before end of string".to_owned()),
    }
}

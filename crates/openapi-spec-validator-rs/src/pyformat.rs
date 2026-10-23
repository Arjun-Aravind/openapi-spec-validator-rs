//! `string.Formatter().parse` field names (temporary minimal version).

pub fn formatter_field_names(s: &str) -> Result<Vec<Option<String>>, String> {
    let mut names = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
            }
            '}' => return Err("Single '}' encountered in format string".to_owned()),
            '{' => {
                let mut field = String::new();
                let mut closed = false;
                for c in chars.by_ref() {
                    if c == '}' {
                        closed = true;
                        break;
                    }
                    field.push(c);
                }
                if !closed {
                    return Err("expected '}' before end of string".to_owned());
                }
                let name = field.split([':', '!']).next().unwrap_or_default();
                names.push(Some(name.to_owned()));
            }
            _ => {}
        }
    }
    Ok(names)
}

//! PyYAML's `Resolver` as configured by jsonschema-path's `JsonschemaSafeLoader`:
//! the YAML 1.1 implicit resolvers minus `timestamp`, plus a trailing scientific-float rule.

/// A resolved (or explicit) node tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Tag {
    Null,
    Bool,
    Int,
    Float,
    Binary,
    Timestamp,
    Omap,
    Pairs,
    Set,
    Str,
    Seq,
    Map,
    Merge,
    Value,
    Yaml,
    Other(Box<str>),
}

const PREFIX: &str = "tag:yaml.org,2002:";

impl Tag {
    pub fn from_explicit(tag: &str) -> Self {
        let Some(suffix) = tag.strip_prefix(PREFIX) else {
            return Self::Other(tag.into());
        };
        match suffix {
            "null" => Self::Null,
            "bool" => Self::Bool,
            "int" => Self::Int,
            "float" => Self::Float,
            "binary" => Self::Binary,
            "timestamp" => Self::Timestamp,
            "omap" => Self::Omap,
            "pairs" => Self::Pairs,
            "set" => Self::Set,
            "str" => Self::Str,
            "seq" => Self::Seq,
            "map" => Self::Map,
            "merge" => Self::Merge,
            "value" => Self::Value,
            "yaml" => Self::Yaml,
            _ => Self::Other(tag.into()),
        }
    }

    /// The tag URI, as PyYAML stores it in `node.tag`.
    pub fn uri(&self) -> String {
        let suffix = match self {
            Self::Other(tag) => return tag.to_string(),
            Self::Null => "null",
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::Binary => "binary",
            Self::Timestamp => "timestamp",
            Self::Omap => "omap",
            Self::Pairs => "pairs",
            Self::Set => "set",
            Self::Str => "str",
            Self::Seq => "seq",
            Self::Map => "map",
            Self::Merge => "merge",
            Self::Value => "value",
            Self::Yaml => "yaml",
        };
        format!("{PREFIX}{suffix}")
    }
}

/// `Resolver.resolve(ScalarNode, value, (plain_implicit, _))`.
pub(super) fn resolve_scalar(value: &str, plain_implicit: bool) -> Tag {
    if !plain_implicit {
        return Tag::Str;
    }
    // Python's `$` also matches before a final newline; none of the patterns can consume one.
    let v = value.strip_suffix('\n').unwrap_or(value);
    // Resolvers are looked up by the first character and tried in registration order.
    let first = value.chars().next();
    let tag = match first {
        None => Some(Tag::Null),
        Some('y' | 'Y' | 't' | 'T' | 'f' | 'F' | 'o' | 'O') => is_bool(v).then_some(Tag::Bool),
        Some('n' | 'N') => {
            if is_bool(v) {
                Some(Tag::Bool)
            } else {
                matches!(v, "null" | "Null" | "NULL").then_some(Tag::Null)
            }
        }
        Some('-' | '+' | '0'..='9') => {
            if is_float(v) {
                Some(Tag::Float)
            } else if is_int(v) {
                Some(Tag::Int)
            } else {
                is_scientific(v).then_some(Tag::Float)
            }
        }
        Some('.') => (is_float(v) || is_scientific(v)).then_some(Tag::Float),
        Some('<') => (v == "<<").then_some(Tag::Merge),
        Some('~') => (v == "~").then_some(Tag::Null),
        Some('=') => (v == "=").then_some(Tag::Value),
        // Documentation-only resolver; reachable through the non-specific `!` tag.
        Some('!' | '&' | '*') => matches!(v, "!" | "&" | "*").then_some(Tag::Yaml),
        Some(_) => None,
    };
    tag.unwrap_or(Tag::Str)
}

fn is_bool(v: &str) -> bool {
    matches!(
        v,
        "yes"
            | "Yes"
            | "YES"
            | "no"
            | "No"
            | "NO"
            | "true"
            | "True"
            | "TRUE"
            | "false"
            | "False"
            | "FALSE"
            | "on"
            | "On"
            | "ON"
            | "off"
            | "Off"
            | "OFF"
    )
}

/// A cursor over ASCII bytes for the hand-written regex matchers below.
struct Cur<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            s: s.as_bytes(),
            i: 0,
        }
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, f: impl Fn(u8) -> bool) -> bool {
        match self.peek() {
            Some(b) if f(b) => {
                self.i += 1;
                true
            }
            _ => false,
        }
    }
    fn eat_byte(&mut self, b: u8) -> bool {
        self.eat(|c| c == b)
    }
    fn eat_while(&mut self, f: impl Fn(u8) -> bool) -> usize {
        let start = self.i;
        while self.eat(&f) {}
        self.i - start
    }
    fn sign(&mut self) {
        self.eat(|b| b == b'-' || b == b'+');
    }
    fn done(&self) -> bool {
        self.i == self.s.len()
    }
    /// `(?::[0-5]?[0-9])+`
    fn sexagesimal_groups(&mut self) -> bool {
        let mut groups = 0;
        while self.peek() == Some(b':') {
            let save = self.i;
            self.i += 1;
            let start = self.i;
            let n = self.eat_while(|b| b.is_ascii_digit());
            let ok = n == 1 || (n == 2 && self.s[start] <= b'5');
            if !ok {
                self.i = save;
                break;
            }
            groups += 1;
        }
        groups > 0
    }
}

fn digit(b: u8) -> bool {
    b.is_ascii_digit()
}
fn digit_(b: u8) -> bool {
    b.is_ascii_digit() || b == b'_'
}

/// `[eE][-+][0-9]+` (optional; returns false only on a malformed exponent).
fn opt_signed_exponent(c: &mut Cur<'_>) -> bool {
    if c.eat(|b| b == b'e' || b == b'E') {
        c.eat(|b| b == b'-' || b == b'+') && c.eat_while(digit) > 0
    } else {
        true
    }
}

fn is_float(v: &str) -> bool {
    let alt = |f: &dyn Fn(&mut Cur<'_>) -> bool| {
        let mut c = Cur::new(v);
        f(&mut c) && c.done()
    };
    // [-+]?(?:[0-9][0-9_]*)\.[0-9_]*(?:[eE][-+][0-9]+)?
    alt(&|c| {
        c.sign();
        c.eat(digit) && {
            c.eat_while(digit_);
            c.eat_byte(b'.')
        } && {
            c.eat_while(digit_);
            opt_signed_exponent(c)
        }
    })
    // \.[0-9][0-9_]*(?:[eE][-+][0-9]+)?
    || alt(&|c| {
        c.eat_byte(b'.') && c.eat(digit) && {
            c.eat_while(digit_);
            opt_signed_exponent(c)
        }
    })
    // [-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+\.[0-9_]*
    || alt(&|c| {
        c.sign();
        c.eat(digit) && {
            c.eat_while(digit_);
            c.sexagesimal_groups()
        } && c.eat_byte(b'.') && {
            c.eat_while(digit_);
            true
        }
    })
    // [-+]?\.(?:inf|Inf|INF) | \.(?:nan|NaN|NAN)
    || matches!(v.strip_prefix(['-', '+']).unwrap_or(v), ".inf" | ".Inf" | ".INF")
    || matches!(v, ".nan" | ".NaN" | ".NAN")
}

fn is_int(v: &str) -> bool {
    let alt = |f: &dyn Fn(&mut Cur<'_>) -> bool| {
        let mut c = Cur::new(v);
        f(&mut c) && c.done()
    };
    let body = v.strip_prefix(['-', '+']).unwrap_or(v);
    // [-+]?0b[0-1_]+
    body.strip_prefix("0b").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| matches!(b, b'0' | b'1' | b'_')))
    // [-+]?0[0-7_]+
    || body.strip_prefix('0').is_some_and(|d| !d.is_empty() && d.bytes().all(|b| matches!(b, b'0'..=b'7' | b'_')))
    // [-+]?(?:0|[1-9][0-9_]*)
    || body == "0"
    || (body.as_bytes().first().is_some_and(|b| (b'1'..=b'9').contains(b)) && body.bytes().all(digit_))
    // [-+]?0x[0-9a-fA-F_]+
    || body.strip_prefix("0x").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_hexdigit() || b == b'_'))
    // [-+]?[1-9][0-9_]*(?::[0-5]?[0-9])+
    || alt(&|c| {
        c.sign();
        c.eat(|b| (b'1'..=b'9').contains(&b)) && {
            c.eat_while(digit_);
            c.sexagesimal_groups()
        }
    })
}

/// jsonschema-path's `SCIENTIFIC_FLOAT_RE`.
fn is_scientific(v: &str) -> bool {
    let mut c = Cur::new(v);
    c.sign();
    let int_part = |c: &mut Cur<'_>| -> bool {
        if c.eat_byte(b'0') {
            true
        } else if c.eat(|b| (b'1'..=b'9').contains(&b)) {
            c.eat_while(digit);
            true
        } else {
            false
        }
    };
    let mantissa = if int_part(&mut c) {
        if c.eat_byte(b'.') {
            c.eat_while(digit);
        }
        true
    } else {
        c.eat_byte(b'.') && c.eat_while(digit) > 0
    };
    mantissa
        && c.eat(|b| b == b'e' || b == b'E')
        && {
            c.sign();
            c.eat_while(digit) > 0
        }
        && c.done()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(v: &str) -> Tag {
        resolve_scalar(v, true)
    }

    #[test]
    fn implicit_resolution_follows_yaml_1_1() {
        for v in [
            "1", "-1", "+1", "0", "0b1_0", "017", "0x_fF", "1_000", "1:30", "-1:30:00",
        ] {
            assert_eq!(tag(v), Tag::Int, "{v}");
        }
        for v in [
            "1.5", "1.", ".5", "-1.5e+3", "1:30.5", ".inf", "-.Inf", ".NaN", "1e5", "1.5e5", "1E5",
            "-.5e3", "0.1e-2", "1_0.5",
        ] {
            assert_eq!(tag(v), Tag::Float, "{v}");
        }
        for v in [
            "08", "-.5", "0X1F", "0B1", "1_0e5", "1:60", ".", "-", "1e", "-.nan", "1:3:",
        ] {
            assert_eq!(tag(v), Tag::Str, "{v}");
        }
        for v in ["yes", "No", "ON", "off", "TRUE", "False"] {
            assert_eq!(tag(v), Tag::Bool, "{v}");
        }
        for v in ["y", "n", "nULL", "tRUE"] {
            assert_eq!(tag(v), Tag::Str, "{v}");
        }
        assert_eq!(tag(""), Tag::Null);
        assert_eq!(tag("~"), Tag::Null);
        assert_eq!(tag("123\n"), Tag::Int);
        assert_eq!(tag("<<"), Tag::Merge);
        assert_eq!(tag("="), Tag::Value);
        assert_eq!(resolve_scalar("123", false), Tag::Str);
    }
}

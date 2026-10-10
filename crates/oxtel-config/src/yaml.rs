//! YAML to a plain tree, with environment substitution in scalar values.
//!
//! The tree is built from the parser's events rather than its document
//! model, because substitution depends on each scalar's style: a plain
//! scalar is typed by the YAML 1.2 core schema after substitution
//! (`${OTEL_SDK_DISABLED:-false}` becomes the boolean `false`), while a
//! quoted one stays a string.

use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

use crate::Error;

/// A YAML value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Seq(Vec<Node>),
    Map(Vec<(String, Node)>),
}

enum Frame {
    Seq(Vec<Node>),
    Map(Vec<(String, Node)>, Option<String>),
}

struct Builder<'a> {
    lookup: &'a dyn Fn(&str) -> Option<String>,
    stack: Vec<Frame>,
    root: Option<Node>,
    documents: usize,
    error: Option<Error>,
}

impl Builder<'_> {
    fn fail(&mut self, message: String, mark: Marker) {
        if self.error.is_none() {
            self.error = Some(Error::Parse {
                line: mark.line(),
                column: mark.col() + 1,
                message,
            });
        }
    }

    /// Place a finished value in its parent, or make it the root.
    fn place(&mut self, node: Node, mark: Marker) {
        match self.stack.last_mut() {
            Some(Frame::Seq(items)) => items.push(node),
            Some(Frame::Map(entries, key)) => match key.take() {
                Some(k) => {
                    let duplicate = entries.iter().any(|(existing, _)| *existing == k);
                    let message = format!("duplicate key `{k}`");
                    entries.push((k, node));
                    if duplicate {
                        self.fail(message, mark);
                    }
                }
                None => match node {
                    Node::Str(k) => *key = Some(k),
                    Node::Int(i) => *key = Some(i.to_string()),
                    Node::Bool(b) => *key = Some(b.to_string()),
                    _ => self.fail("mapping keys must be scalars".into(), mark),
                },
            },
            None if self.root.is_none() => self.root = Some(node),
            None => {}
        }
    }

    fn expecting_key(&self) -> bool {
        matches!(self.stack.last(), Some(Frame::Map(_, None)))
    }
}

impl MarkedEventReceiver for Builder<'_> {
    fn on_event(&mut self, event: Event, mark: Marker) {
        if self.error.is_some() || self.documents > 1 {
            return;
        }
        match event {
            Event::DocumentStart => {
                self.documents += 1;
                if self.documents > 1 {
                    self.fail("only one YAML document is allowed".into(), mark);
                }
            }
            Event::Scalar(value, style, _, _) => {
                // Keys aren't substituted.
                if self.expecting_key() {
                    self.place(Node::Str(value), mark);
                    return;
                }
                match substitute(&value, self.lookup) {
                    Ok(value) => {
                        let node = if style == TScalarStyle::Plain {
                            core_schema(&value)
                        } else {
                            Node::Str(value)
                        };
                        self.place(node, mark);
                    }
                    Err(message) => self.fail(message, mark),
                }
            }
            Event::SequenceStart(..) => self.stack.push(Frame::Seq(Vec::new())),
            Event::MappingStart(..) => self.stack.push(Frame::Map(Vec::new(), None)),
            Event::SequenceEnd => {
                if let Some(Frame::Seq(items)) = self.stack.pop() {
                    self.place(Node::Seq(items), mark);
                }
            }
            Event::MappingEnd => {
                if let Some(Frame::Map(entries, _)) = self.stack.pop() {
                    self.place(Node::Map(entries), mark);
                }
            }
            Event::Alias(_) => self.fail("anchors and aliases aren't supported".into(), mark),
            _ => {}
        }
    }
}

/// Parse `text`, substituting environment variables from `lookup`.
pub(crate) fn parse(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Node, Error> {
    let mut builder = Builder {
        lookup,
        stack: Vec::new(),
        root: None,
        documents: 0,
        error: None,
    };
    Parser::new_from_str(text)
        .load(&mut builder, true)
        .map_err(|e| Error::Parse {
            line: e.marker().line(),
            column: e.marker().col() + 1,
            message: e.info().to_string(),
        })?;
    if let Some(error) = builder.error {
        return Err(error);
    }
    Ok(builder.root.unwrap_or(Node::Null))
}

/// Replace `${NAME}`, `${env:NAME}` and `${NAME:-default}` with the
/// variable's value (the default when it's unset or empty, or nothing), and
/// `$$` with `$`. Replacements aren't substituted again.
pub(crate) fn substitute(
    text: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    if !text.contains('$') {
        return Ok(text.to_string());
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
        } else if let Some(inner) = after.strip_prefix('{') {
            let close = inner
                .find('}')
                .ok_or_else(|| format!("unterminated substitution in `{text}`"))?;
            let reference = &inner[..close];
            let (name, default) = match reference.split_once(":-") {
                Some((name, default)) => (name, Some(default)),
                None => (reference, None),
            };
            let name = name.strip_prefix("env:").unwrap_or(name);
            let valid = name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !valid {
                return Err(format!("invalid substitution `${{{reference}}}`"));
            }
            match lookup(name).filter(|v| !v.is_empty()) {
                Some(value) => out.push_str(&value),
                None => out.push_str(default.unwrap_or("")),
            }
            rest = &inner[close + 1..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// A plain scalar typed by the YAML 1.2 core schema.
fn core_schema(s: &str) -> Node {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Node::Null,
        "true" | "True" | "TRUE" => return Node::Bool(true),
        "false" | "False" | "FALSE" => return Node::Bool(false),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return Node::Float(f64::INFINITY);
        }
        "-.inf" | "-.Inf" | "-.INF" => return Node::Float(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return Node::Float(f64::NAN),
        _ => {}
    }
    let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
    if !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && let Ok(i) = s.parse::<i64>()
    {
        return Node::Int(i);
    }
    if let Some(hex) = s.strip_prefix("0x")
        && let Ok(i) = i64::from_str_radix(hex, 16)
    {
        return Node::Int(i);
    }
    if let Some(oct) = s.strip_prefix("0o")
        && let Ok(i) = i64::from_str_radix(oct, 8)
    {
        return Node::Int(i);
    }
    let looks_numeric = s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E'));
    if looks_numeric && let Ok(f) = s.parse::<f64>() {
        return Node::Float(f);
    }
    Node::Str(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name {
            "STRING_VALUE" => Some("value".into()),
            "BOOL_VALUE" => Some("true".into()),
            "INT_VALUE" => Some("1".into()),
            "EMPTY" => Some(String::new()),
            "STRUCTURE" => Some("{a: 1}".into()),
            "DOLLAR" => Some("${STRING_VALUE}".into()),
            _ => None,
        }
    }

    #[test]
    fn substitution_follows_the_specification() {
        let cases = [
            ("${STRING_VALUE}", "value"),
            ("${env:STRING_VALUE}", "value"),
            ("prefix-${STRING_VALUE}-suffix", "prefix-value-suffix"),
            ("${UNDEFINED}", ""),
            ("${UNDEFINED:-fallback}", "fallback"),
            ("${EMPTY:-fallback}", "fallback"),
            ("${STRING_VALUE:-fallback}", "value"),
            ("$${STRING_VALUE}", "${STRING_VALUE}"),
            ("$$$$", "$$"),
            ("cost: $5", "cost: $5"),
            ("${DOLLAR}", "${STRING_VALUE}"),
            ("${UNDEFINED:-a:-b}", "a:-b"),
        ];
        for (input, expected) in cases {
            assert_eq!(substitute(input, &env).as_deref(), Ok(expected), "{input}");
        }
        for bad in ["${STRING_VALUE", "${1BAD}", "${}", "${A-B}"] {
            assert!(substitute(bad, &env).is_err(), "{bad}");
        }
    }

    #[test]
    fn plain_scalars_are_typed_after_substitution_and_quoted_ones_are_not() {
        let text = "\
a: ${BOOL_VALUE}
b: \"${BOOL_VALUE}\"
c: ${INT_VALUE}
d: ${UNDEFINED}
e: ${STRUCTURE}
f: 0x1F
g: 1.5e3
h: 1.0.0
";
        let Node::Map(entries) = parse(text, &env).unwrap() else {
            panic!()
        };
        let get = |k: &str| entries.iter().find(|(key, _)| key == k).unwrap().1.clone();
        assert_eq!(get("a"), Node::Bool(true));
        assert_eq!(get("b"), Node::Str("true".into()));
        assert_eq!(get("c"), Node::Int(1));
        assert_eq!(get("d"), Node::Null);
        assert_eq!(get("e"), Node::Str("{a: 1}".into()), "never YAML structure");
        assert_eq!(get("f"), Node::Int(31));
        assert_eq!(get("g"), Node::Float(1500.0));
        assert_eq!(get("h"), Node::Str("1.0.0".into()));
    }

    #[test]
    fn keys_are_not_substituted_and_bad_documents_are_errors() {
        let Node::Map(entries) = parse("${STRING_VALUE}: 1\n", &env).unwrap() else {
            panic!()
        };
        assert_eq!(entries[0].0, "${STRING_VALUE}");
        assert!(matches!(
            parse("a: 1\na: 2\n", &env),
            Err(Error::Parse { line: 2, .. })
        ));
        assert!(parse("a: &x 1\nb: *x\n", &env).is_err());
        assert!(parse("a: [1\n", &env).is_err());
        assert!(parse("a: 1\n---\nb: 2\n", &env).is_err());
    }
}

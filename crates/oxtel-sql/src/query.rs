//! What the database conventions want from a query's text, without a full
//! SQL parser: a low-cardinality summary (`db.query.summary`) and the text
//! with its literals replaced (`db.query.text`).
//!
//! Both work on a lexer that knows SQL's comments, string and identifier
//! quoting, dollar-quoted strings, numbers and placeholders, so neither can
//! mistake a literal for a table or let one through.

/// A piece of SQL text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A keyword or bare identifier.
    Word,
    /// An identifier in `"…"`, `` `…` `` or `[…]`.
    QuotedIdent,
    /// A string, number, blob, dollar-quoted or boolean literal.
    Literal,
    /// A parameter: `?`, `?1`, `$1`, `:name`, `@name`.
    Placeholder,
    /// `--` or `/* */`.
    Comment,
    /// Spaces, tabs and newlines.
    Space,
    /// Anything else: `(`, `)`, `,`, `.`, `;`, `*`, operators.
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The end of a quoted run starting at `start` (the opening quote), where a
/// doubled `close` is an escaped one.
fn quoted_end(s: &str, start: usize, close: char) -> usize {
    let mut chars = s[start + 1..].char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == close {
            if chars.peek().map(|&(_, n)| n) == Some(close) && close != ']' {
                chars.next();
                continue;
            }
            return start + 1 + i + c.len_utf8();
        }
    }
    s.len()
}

fn lex(sql: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < sql.len() {
        let rest = &sql[i..];
        let c = rest.chars().next().unwrap_or(' ');
        let (kind, len) = if c.is_whitespace() {
            let len = rest
                .char_indices()
                .find(|&(_, c)| !c.is_whitespace())
                .map_or(rest.len(), |(j, _)| j);
            (Kind::Space, len)
        } else if rest.starts_with("--") {
            (Kind::Comment, rest.find('\n').unwrap_or(rest.len()))
        } else if rest.starts_with("/*") {
            // Postgres comments nest.
            let mut depth = 0usize;
            let mut j = 0;
            let mut end = rest.len();
            while j < rest.len() {
                if rest[j..].starts_with("/*") {
                    depth += 1;
                    j += 2;
                } else if rest[j..].starts_with("*/") {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        end = j;
                        break;
                    }
                } else {
                    j += rest[j..].chars().next().map_or(1, char::len_utf8);
                }
            }
            (Kind::Comment, end)
        } else if c == '\'' {
            (Kind::Literal, quoted_end(sql, i, '\'') - i)
        } else if matches!(c, 'E' | 'e' | 'B' | 'b' | 'X' | 'x' | 'N' | 'n' | 'U' | 'u')
            && rest[c.len_utf8()..].starts_with('\'')
        {
            // E'…', B'…', X'…', N'…' and U&-less prefixed strings.
            let open = i + c.len_utf8();
            (Kind::Literal, quoted_end(sql, open, '\'') - i)
        } else if c == '"' {
            (Kind::QuotedIdent, quoted_end(sql, i, '"') - i)
        } else if c == '`' {
            (Kind::QuotedIdent, quoted_end(sql, i, '`') - i)
        } else if c == '[' {
            (Kind::QuotedIdent, quoted_end(sql, i, ']') - i)
        } else if c == '$' {
            let after = &rest[1..];
            let digits = after.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 {
                (Kind::Placeholder, 1 + digits)
            } else {
                // $tag$ … $tag$, or $$ … $$, or SQLite's $name parameter.
                let tag_len = after
                    .char_indices()
                    .find(|&(_, c)| !is_ident_char(c) || c == '$')
                    .map_or(after.len(), |(j, _)| j);
                if after[tag_len..].starts_with('$') {
                    let delim = &rest[..tag_len + 2];
                    let body = &rest[delim.len()..];
                    let end = body
                        .find(delim)
                        .map_or(rest.len(), |j| delim.len() + j + delim.len());
                    (Kind::Literal, end)
                } else if tag_len > 0 {
                    (Kind::Placeholder, 1 + tag_len)
                } else {
                    (Kind::Punct, 1)
                }
            }
        } else if c == '?' {
            let digits = rest[1..].bytes().take_while(u8::is_ascii_digit).count();
            (Kind::Placeholder, 1 + digits)
        } else if (c == ':' || c == '@')
            && rest[1..].chars().next().is_some_and(is_ident_start)
            && !(c == ':' && i > 0 && bytes[i - 1] == b':')
        {
            let len = rest[1..]
                .char_indices()
                .find(|&(_, c)| !is_ident_char(c))
                .map_or(rest.len() - 1, |(j, _)| j);
            (Kind::Placeholder, 1 + len)
        } else if c.is_ascii_digit()
            || (c == '.' && rest[1..].bytes().next().is_some_and(|b| b.is_ascii_digit()))
        {
            let b = rest.as_bytes();
            let mut j = 0;
            if rest.starts_with("0x") || rest.starts_with("0X") {
                j = 2;
                while j < b.len() && b[j].is_ascii_hexdigit() {
                    j += 1;
                }
            } else {
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.' || b[j] == b'_') {
                    j += 1;
                }
                if j < b.len() && (b[j] == b'e' || b[j] == b'E') {
                    let mut k = j + 1;
                    if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
                        k += 1;
                    }
                    if k < b.len() && b[k].is_ascii_digit() {
                        j = k;
                        while j < b.len() && b[j].is_ascii_digit() {
                            j += 1;
                        }
                    }
                }
            }
            (Kind::Literal, j.max(1))
        } else if is_ident_start(c) {
            let len = rest
                .char_indices()
                .find(|&(_, c)| !is_ident_char(c))
                .map_or(rest.len(), |(j, _)| j);
            let word = &rest[..len];
            if word.eq_ignore_ascii_case("true") || word.eq_ignore_ascii_case("false") {
                (Kind::Literal, len)
            } else {
                (Kind::Word, len)
            }
        } else {
            (Kind::Punct, c.len_utf8())
        };
        tokens.push(Token {
            kind,
            text: &sql[i..i + len],
        });
        i += len;
    }
    tokens
}

/// `sql` with every literal replaced by `?`, comments removed and runs of
/// space made one, as the database conventions ask before recording
/// `db.query.text`. Placeholders stay, and an `IN` list of literals
/// collapses to `IN (?)`.
pub fn sanitize(sql: &str) -> String {
    let tokens = lex(sql);
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        match t.kind {
            Kind::Literal => out.push('?'),
            // Comments may hold anything; runs of space become one.
            Kind::Comment | Kind::Space => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            Kind::Word if t.text.eq_ignore_ascii_case("in") => {
                out.push_str(t.text);
                if let Some(end) = literal_list(&tokens, i + 1) {
                    if tokens[i + 1].kind == Kind::Space {
                        out.push(' ');
                    }
                    out.push_str("(?)");
                    i = end;
                    continue;
                }
            }
            _ => out.push_str(t.text),
        }
        i += 1;
    }
    out.trim().to_string()
}

/// If the tokens from `start` are `( literal, literal, … )`, the index after
/// the closing parenthesis.
fn literal_list(tokens: &[Token<'_>], start: usize) -> Option<usize> {
    let mut i = start;
    while tokens.get(i)?.kind == Kind::Space {
        i += 1;
    }
    if tokens[i].text != "(" {
        return None;
    }
    i += 1;
    let mut items = 0;
    loop {
        let t = tokens.get(i)?;
        match t.kind {
            Kind::Space | Kind::Comment => {}
            Kind::Literal => items += 1,
            Kind::Punct if t.text == "," => {}
            Kind::Punct if t.text == ")" => return (items > 0).then_some(i + 1),
            _ => return None,
        }
        i += 1;
    }
}

const OPERATIONS: &[&str] = &[
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "REPLACE",
    "UPSERT",
    "CALL",
    "EXEC",
    "EXECUTE",
    "CREATE",
    "ALTER",
    "DROP",
    "TRUNCATE",
    "BEGIN",
    "START",
    "COMMIT",
    "END",
    "ROLLBACK",
    "SAVEPOINT",
    "RELEASE",
    "PRAGMA",
    "VACUUM",
    "ANALYZE",
    "REINDEX",
    "ATTACH",
    "DETACH",
    "COPY",
    "LISTEN",
    "NOTIFY",
    "UNLISTEN",
    "SET",
    "SHOW",
    "GRANT",
    "REVOKE",
    "LOCK",
    "REFRESH",
    "VALUES",
];

/// Words that end the list of tables after `FROM`.
const LIST_END: &[&str] = &[
    "WHERE",
    "GROUP",
    "ORDER",
    "LIMIT",
    "OFFSET",
    "HAVING",
    "WINDOW",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "RETURNING",
    "SET",
    "VALUES",
    "ON",
    "USING",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "NATURAL",
    "OUTER",
    "FETCH",
    "FOR",
    "INTO",
    "SELECT",
    "LATERAL",
];

/// Object kinds after `CREATE`, `ALTER` and `DROP`.
const OBJECTS: &[&str] = &[
    "TABLE",
    "INDEX",
    "VIEW",
    "TRIGGER",
    "SCHEMA",
    "DATABASE",
    "SEQUENCE",
    "FUNCTION",
    "PROCEDURE",
    "EXTENSION",
    "TYPE",
    "ROLE",
    "USER",
    "DOMAIN",
    "POLICY",
];

/// Words that may sit between `CREATE` and the object kind.
const OBJECT_MODIFIERS: &[&str] = &[
    "OR",
    "REPLACE",
    "TEMP",
    "TEMPORARY",
    "UNIQUE",
    "VIRTUAL",
    "MATERIALIZED",
    "UNLOGGED",
    "GLOBAL",
    "LOCAL",
    "CONCURRENTLY",
];

const MAX_SUMMARY: usize = 255;

fn is(word: &str, set: &[&str]) -> bool {
    set.iter().any(|w| word.eq_ignore_ascii_case(w))
}

/// A low-cardinality summary of `sql` for `db.query.summary` and the span
/// name, as the database conventions describe it: each operation and the
/// tables it names, in the order they appear, as written, such as
/// `SELECT users` or `INSERT shipping_details SELECT orders`. `None` when
/// `sql` names no operation this recognizes.
pub fn summarize(sql: &str) -> Option<String> {
    let tokens: Vec<Token<'_>> = lex(sql)
        .into_iter()
        .filter(|t| !matches!(t.kind, Kind::Space | Kind::Comment))
        .collect();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t.kind != Kind::Word {
            i += 1;
            continue;
        }
        let w = t.text;
        if is(w, &["CREATE", "ALTER", "DROP"]) {
            // CREATE [OR REPLACE] [TEMP …] TABLE [IF [NOT] EXISTS] name
            let mut j = i + 1;
            while j < tokens.len()
                && tokens[j].kind == Kind::Word
                && is(tokens[j].text, OBJECT_MODIFIERS)
            {
                j += 1;
            }
            match tokens.get(j) {
                Some(o) if o.kind == Kind::Word && is(o.text, OBJECTS) => {
                    parts.push(format!("{w} {}", o.text));
                    j += 1;
                    while j < tokens.len()
                        && tokens[j].kind == Kind::Word
                        && is(tokens[j].text, &["IF", "NOT", "EXISTS"])
                    {
                        j += 1;
                    }
                    if let Some((name, next)) = name_at(&tokens, j) {
                        parts.push(name);
                        j = next;
                    }
                }
                _ => parts.push(w.to_string()),
            }
            i = j;
            continue;
        }
        if is(w, OPERATIONS) {
            // `END` closes a CASE; only a statement's first word is the operation.
            if is(w, &["END", "START", "SET", "VALUES"]) && i != 0 {
                i += 1;
                continue;
            }
            // `REPLACE` is also a function, and an `INSERT OR REPLACE` modifier.
            if w.eq_ignore_ascii_case("REPLACE") && i != 0 {
                i += 1;
                continue;
            }
            parts.push(w.to_string());
            i += 1;
            if is(w, &["UPDATE"]) {
                // UPDATE [OR ROLLBACK|ABORT|REPLACE|FAIL|IGNORE] [ONLY] name
                if tokens
                    .get(i)
                    .is_some_and(|t| t.text.eq_ignore_ascii_case("OR"))
                {
                    i += 2;
                }
                if tokens
                    .get(i)
                    .is_some_and(|t| t.text.eq_ignore_ascii_case("ONLY"))
                {
                    i += 1;
                }
                if let Some((name, next)) = name_at(&tokens, i) {
                    parts.push(name);
                    i = next;
                }
            } else if is(
                w,
                &[
                    "TRUNCATE", "LOCK", "VACUUM", "ANALYZE", "REINDEX", "PRAGMA", "CALL", "EXEC",
                    "EXECUTE",
                ],
            ) {
                if tokens
                    .get(i)
                    .is_some_and(|t| t.text.eq_ignore_ascii_case("TABLE"))
                {
                    i += 1;
                }
                if let Some((name, next)) = name_at(&tokens, i) {
                    parts.push(name);
                    i = next;
                }
            }
            continue;
        }
        if is(w, &["FROM", "JOIN", "INTO", "USING"]) {
            if w.eq_ignore_ascii_case("USING")
                && !parts.first().is_some_and(|p| is(p, &["MERGE", "DELETE"]))
            {
                // `JOIN … USING (col)` names columns, not tables.
                i += 1;
                continue;
            }
            let from = w.eq_ignore_ascii_case("FROM");
            i += 1;
            if tokens
                .get(i)
                .is_some_and(|t| t.text.eq_ignore_ascii_case("ONLY"))
            {
                i += 1;
            }
            loop {
                if tokens.get(i).is_some_and(|t| t.text == "(") {
                    // A subquery or a VALUES list: carry on inside it.
                    break;
                }
                let Some((name, next)) = name_at(&tokens, i) else {
                    break;
                };
                i = next;
                // A function call, such as FROM generate_series(1, 10). After
                // INTO, the parenthesis lists columns instead.
                if !w.eq_ignore_ascii_case("INTO") && tokens.get(i).is_some_and(|t| t.text == "(") {
                    break;
                }
                parts.push(name);
                // An alias: [AS] alias.
                if tokens
                    .get(i)
                    .is_some_and(|t| t.text.eq_ignore_ascii_case("AS"))
                {
                    i += 1;
                    if name_at(&tokens, i).is_some() {
                        i += 1;
                    }
                } else if tokens.get(i).is_some_and(|t| {
                    matches!(t.kind, Kind::Word | Kind::QuotedIdent) && !is(t.text, LIST_END)
                }) {
                    i += 1;
                }
                if from && tokens.get(i).is_some_and(|t| t.text == ",") {
                    i += 1;
                    continue;
                }
                break;
            }
            continue;
        }
        i += 1;
    }
    if parts.is_empty() {
        return None;
    }
    let mut summary = String::new();
    for part in parts {
        let extra = usize::from(!summary.is_empty()) + part.len();
        if summary.len() + extra > MAX_SUMMARY {
            break;
        }
        if !summary.is_empty() {
            summary.push(' ');
        }
        summary.push_str(&part);
    }
    (!summary.is_empty()).then_some(summary)
}

/// A table name at `i`: a word, quoted identifier or (as SQLite allows)
/// string, possibly qualified with dots. The name as written, and the index
/// after it.
fn name_at(tokens: &[Token<'_>], mut i: usize) -> Option<(String, usize)> {
    let first = tokens.get(i)?;
    let ok = |t: &Token<'_>| {
        matches!(t.kind, Kind::QuotedIdent)
            || (t.kind == Kind::Word && !is(t.text, LIST_END) && !is(t.text, OPERATIONS))
            || (t.kind == Kind::Literal && t.text.starts_with('\''))
    };
    if !ok(first) {
        return None;
    }
    let mut name = first.text.to_string();
    i += 1;
    while tokens.get(i).is_some_and(|t| t.text == ".") && tokens.get(i + 1).is_some_and(ok) {
        name.push('.');
        name.push_str(tokens[i + 1].text);
        i += 2;
    }
    Some((name, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_follow_the_conventions_examples() {
        for (sql, summary) in [
            (
                "SELECT * FROM wuser_table WHERE username = ?",
                "SELECT wuser_table",
            ),
            (
                "INSERT INTO shipping_details (order_id, address) SELECT order_id, address FROM orders WHERE order_id = ?",
                "INSERT shipping_details SELECT orders",
            ),
            (
                "SELECT * FROM songs, artists WHERE songs.artist_id == artists.id",
                "SELECT songs artists",
            ),
            (
                "SELECT order_date FROM (SELECT * FROM orders o JOIN customers c ON o.customer_id = c.customer_id)",
                "SELECT SELECT orders customers",
            ),
            (
                r#"SELECT * FROM "song list", 'artists'"#,
                r#"SELECT "song list" 'artists'"#,
            ),
        ] {
            assert_eq!(summarize(sql).as_deref(), Some(summary), "{sql}");
        }
    }

    #[test]
    fn summaries_of_everyday_statements() {
        for (sql, summary) in [
            ("select id from users where id = $1", "select users"),
            (
                "UPDATE accounts SET balance = balance - $1 WHERE id = $2",
                "UPDATE accounts",
            ),
            ("UPDATE OR REPLACE kv SET v = ?1", "UPDATE kv"),
            (
                "DELETE FROM sessions WHERE expires < now()",
                "DELETE sessions",
            ),
            (
                "INSERT OR REPLACE INTO kv (k, v) VALUES (?, ?)",
                "INSERT kv",
            ),
            ("REPLACE INTO kv VALUES (1, 2)", "REPLACE kv"),
            (
                "CREATE TABLE IF NOT EXISTS objects (id BLOB PRIMARY KEY)",
                "CREATE TABLE objects",
            ),
            (
                "CREATE UNIQUE INDEX idx_refs ON refs (name)",
                "CREATE INDEX idx_refs",
            ),
            ("DROP VIEW public.recent", "DROP VIEW public.recent"),
            ("BEGIN IMMEDIATE", "BEGIN"),
            ("COMMIT", "COMMIT"),
            ("PRAGMA journal_mode = WAL", "PRAGMA journal_mode"),
            ("SELECT 1", "SELECT"),
            (
                "SELECT * FROM a LEFT JOIN b ON a.id = b.id JOIN c USING (id)",
                "SELECT a b c",
            ),
            (
                "SELECT * FROM public.users AS u WHERE u.id = 1",
                "SELECT public.users",
            ),
            ("SELECT * FROM generate_series(1, 10)", "SELECT"),
            (
                "WITH recent AS (SELECT * FROM commits) SELECT * FROM recent",
                "SELECT commits SELECT recent",
            ),
            ("SELECT CASE WHEN x THEN 1 END FROM t", "SELECT t"),
            ("-- note\nSELECT /* hint */ id FROM t", "SELECT t"),
            ("TRUNCATE TABLE logs", "TRUNCATE logs"),
            ("SELECT replace(name, 'a', 'b') FROM t", "SELECT t"),
            (
                "MERGE INTO target USING source ON (target.id = source.id)",
                "MERGE target source",
            ),
        ] {
            assert_eq!(summarize(sql).as_deref(), Some(summary), "{sql}");
        }
        assert_eq!(summarize("not sql at all"), None);
        assert_eq!(summarize(""), None);
    }

    #[test]
    fn long_summaries_stop_at_a_whole_name() {
        let tables: Vec<String> = (0..60).map(|i| format!("table_number_{i}")).collect();
        let sql = format!("SELECT * FROM {}", tables.join(", "));
        let summary = summarize(&sql).unwrap();
        assert!(summary.len() <= MAX_SUMMARY);
        assert!(
            summary
                .split(' ')
                .skip(1)
                .all(|t| tables.iter().any(|n| n == t)),
            "{summary}"
        );
    }

    #[test]
    fn sanitizing_replaces_literals_and_keeps_placeholders() {
        for (sql, clean) in [
            (
                "SELECT * FROM users WHERE name = 'Ann' AND age > 30",
                "SELECT * FROM users WHERE name = ? AND age > ?",
            ),
            (
                "SELECT * FROM t WHERE id = $1 AND x = ?2 AND y = :name AND z = @p",
                "SELECT * FROM t WHERE id = $1 AND x = ?2 AND y = :name AND z = @p",
            ),
            (
                "SELECT 'it''s', 1.5e10, .5, 0xFF, X'0102', E'a\\nb' FROM t",
                "SELECT ?, ?, ?, ?, ?, ? FROM t",
            ),
            ("SELECT $$secret$$, $tag$also secret$tag$", "SELECT ?, ?"),
            (
                "SELECT * FROM t WHERE active = TRUE",
                "SELECT * FROM t WHERE active = ?",
            ),
            (
                "SELECT * FROM t WHERE id IN (1, 2, 3)",
                "SELECT * FROM t WHERE id IN (?)",
            ),
            (
                "SELECT * FROM t WHERE id IN ($1, $2)",
                "SELECT * FROM t WHERE id IN ($1, $2)",
            ),
            (
                "SELECT * FROM t -- tail 'secret'\nWHERE x = 1",
                "SELECT * FROM t WHERE x = ?",
            ),
            ("SELECT /* user: bob */ col2 FROM t2", "SELECT col2 FROM t2"),
            (
                r#"SELECT "column 1", t1.col2 FROM "my table" t1"#,
                r#"SELECT "column 1", t1.col2 FROM "my table" t1"#,
            ),
            ("SELECT id::text FROM t", "SELECT id::text FROM t"),
        ] {
            assert_eq!(sanitize(sql), clean, "{sql}");
        }
    }

    #[test]
    fn unterminated_input_is_still_safe() {
        assert_eq!(sanitize("SELECT 'unterminated secret"), "SELECT ?");
        assert_eq!(sanitize("SELECT $$ never closed"), "SELECT ?");
        assert_eq!(sanitize("SELECT /* never closed"), "SELECT");
        // Anything goes in, nothing panics.
        for sql in [
            "'", "\"", "$", "$$", "?", ":", "@", "/*", "--", "[", "E'", "0x", ".", "IN (", "IN (1,",
        ] {
            let _ = sanitize(sql);
            let _ = summarize(sql);
        }
    }
}

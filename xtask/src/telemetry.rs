//! The site's page of every span attribute and metric each crate records.
//! It's made from what the crates' own tests saw recorded, which each test
//! keeps in `tests/recorded/*.txt` and fails on when it changes, with each
//! name's description, unit and stability from oxtel-semconv's generated
//! code. `gen-telemetry` writes the page; `check-telemetry` checks it's what
//! those make.

use crate::WEIGHT;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The crates on the page, in order, with the scopes their spans are
/// recorded under, and what their tests can't show: how spans are named.
const CRATES: &[(&str, &[&str], &str)] = &[
    (
        "oxtel-tower",
        &["oxtel_tower"],
        "HTTP servers and clients built on tower, axum and hyper. A server span is named by its method and route, such as `GET /items/{id}`; a client span by its method alone. The server also writes a `request` log line as each request ends.",
    ),
    (
        "oxtel-reqwest",
        &["oxtel_reqwest", "oxtel_tower"],
        "reqwest clients. A span is named by its method, as oxtel-tower's client spans are, and records the same names.",
    ),
    (
        "oxtel-sql",
        &["oxtel_sql"],
        "SQLite through rusqlite, PostgreSQL through tokio-postgres and deadpool-postgres, and sqlx's pools. A span is named by its statement's summary: its operation and table, such as `SELECT orders`. The connection pool metrics come from deadpool-postgres and sqlx pools.",
    ),
    (
        "oxtel-rmcp",
        &["oxtel_rmcp"],
        "MCP servers and clients built with rmcp. A span is named by its method and, for a tool or prompt, its name, such as `tools/call add`.",
    ),
    (
        "oxtel-genai",
        &["oxtel_genai"],
        "Calls to generative AI models. A span is named by its operation and the model asked for, such as `chat llama3.2`.",
    ),
];

/// Where the generated page goes.
pub const PAGE: &str = "site/src/content/docs/reference/telemetry.md";

/// A name's definition in the conventions.
#[derive(Clone, Debug, Default)]
struct Definition {
    brief: String,
    stability: String,
    /// For metrics: the instrument and unit.
    instrument: Option<String>,
    unit: Option<String>,
}

/// What one crate's tests saw recorded.
#[derive(Default)]
struct Recorded {
    /// By span kind: the attribute keys of the crate's own spans.
    spans: BTreeMap<String, BTreeSet<String>>,
    /// By metric name: instrument, unit and label keys.
    metrics: BTreeMap<String, (String, Option<String>, BTreeSet<String>)>,
}

/// What a crate's tests recorded, from its `tests/recorded/` files, keeping
/// only spans under the crate's own scopes, such as `oxtel_tower`.
fn read_recorded(dir: &Path, own: &[&str]) -> Result<Recorded, Box<dyn std::error::Error>> {
    let mut recorded = Recorded::default();
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    files.sort();
    for file in files {
        let text = std::fs::read_to_string(&file)?;
        let mut keys: Option<&mut BTreeSet<String>> = None;
        for line in text.lines() {
            if let Some(key) = line.strip_prefix("  ") {
                if let Some(keys) = keys.as_mut() {
                    keys.insert(key.to_string());
                }
            } else if let Some(rest) = line.strip_prefix("span [") {
                let (scope, rest) = rest.split_once("] ").unwrap_or((rest, ""));
                let (kind, _name) = rest.split_once(' ').unwrap_or((rest, ""));
                // Only the crate's own spans: not the test's, or those of
                // the library it instruments.
                let crate_scope = scope.split("::").next().unwrap_or(scope);
                if !own.contains(&crate_scope) {
                    keys = None;
                    continue;
                }
                keys = Some(recorded.spans.entry(kind.to_string()).or_default());
            } else if let Some(rest) = line.strip_prefix("metric ") {
                let mut parts = rest.split(' ');
                let instrument = parts.next().unwrap_or_default().to_string();
                let name = parts.next().unwrap_or_default().to_string();
                let unit = parts.next().map(str::to_string);
                let entry =
                    recorded
                        .metrics
                        .entry(name)
                        .or_insert((instrument, unit, BTreeSet::new()));
                keys = Some(&mut entry.2);
            } else if !line.is_empty() {
                return Err(format!("{}: can't read {line:?}", file.display()).into());
            }
        }
    }
    Ok(recorded)
}

/// The definitions in one of oxtel-semconv's generated files: each
/// constant's documentation, and its name, instrument and unit.
fn read_definitions(path: &Path, into: &mut BTreeMap<String, Definition>) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut doc: Vec<String> = Vec::new();
    let mut current: Option<(String, Definition)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(d) = trimmed.strip_prefix("///") {
            doc.push(d.strip_prefix(' ').unwrap_or(d).to_string());
            continue;
        }
        if trimmed.starts_with("#[") {
            continue;
        }
        if trimmed.starts_with("pub const ") {
            let definition = Definition {
                brief: brief(&doc),
                stability: doc
                    .iter()
                    .find_map(|l| l.strip_prefix("Stability: "))
                    .map(|s| s.trim_end_matches('.').to_string())
                    .unwrap_or_default(),
                ..Definition::default()
            };
            // An attribute: `pub const X: &str = "name";`.
            if let Some((_, value)) = trimmed.split_once("&str = \"") {
                let name = value.trim_end_matches("\";").to_string();
                into.insert(name, definition);
            } else {
                current = Some((String::new(), definition));
            }
            doc.clear();
            continue;
        }
        if let Some((name, definition)) = current.as_mut() {
            if let Some(v) = trimmed.strip_prefix("name: \"") {
                *name = v.trim_end_matches("\",").to_string();
            } else if let Some(v) = trimmed.strip_prefix("instrument: Instrument::") {
                definition.instrument = Some(v.trim_end_matches(',').to_ascii_lowercase());
            } else if let Some(v) = trimmed.strip_prefix("unit: \"") {
                definition.unit = Some(v.trim_end_matches("\",").to_string());
            } else if trimmed == "};" {
                let (name, definition) = current.take().expect("inside a definition");
                into.insert(name, definition);
            }
        }
        doc.clear();
    }
    Ok(())
}

/// A definition's first paragraph, on one line.
fn brief(doc: &[String]) -> String {
    let paragraph: Vec<&str> = doc
        .iter()
        .map(|l| l.trim())
        .take_while(|l| !l.is_empty())
        .collect();
    paragraph.join(" ").replace('|', "\\|")
}

fn definitions(root: &Path) -> std::io::Result<BTreeMap<String, Definition>> {
    let generated = root.join("crates/oxtel-semconv/src/generated");
    let mut out = BTreeMap::new();
    for file in [
        "attribute.rs",
        "metric.rs",
        "genai/attribute.rs",
        "genai/metric.rs",
    ] {
        read_definitions(&generated.join(file), &mut out)?;
    }
    Ok(out)
}

fn stability(definition: Option<&Definition>) -> String {
    match definition.map(|d| d.stability.as_str()) {
        Some("stable") => "stable".into(),
        Some("") | None => "not in the conventions".into(),
        Some(other) => format!("in {other}"),
    }
}

/// The page, as Markdown.
pub fn page(root: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let definitions = definitions(root)?;
    let mut out = String::from(
        "---\n\
         title: Every span and metric\n\
         description: Every span attribute and metric each oxtel crate records, with its unit and the conventions that define it.\n\
         editUrl: false\n\
         ---\n\n\
         <!-- Generated by `cargo run -p xtask -- gen-telemetry` from the crates' tests/recorded/ files and oxtel-semconv. Edit those, not this. -->\n\n\
         What each instrumentation crate records, under which names, so you can find them in your backend, write dashboards and alerts against them, or check them against what another language records. Every name is defined by the OpenTelemetry semantic conventions; descriptions are theirs. **Stable** names don't change. Names **in development** can change in a later release of the conventions, and oxtel follows them when they do.\n\n\
         Values that may hold personal or secret data, such as tool arguments, aren't recorded unless asked for, and aren't listed here.\n",
    );
    for (krate, scopes, intro) in CRATES {
        let dir = root.join("crates").join(krate).join("tests/recorded");
        let recorded = read_recorded(&dir, scopes)?;
        out.push_str(&format!("\n## {krate}\n\n{intro}\n"));
        for (kind, keys) in &recorded.spans {
            out.push_str(&format!(
                "\n### {} spans\n\n| Attribute | What it is | Stability |\n|---|---|---|\n",
                capitalized(kind)
            ));
            for key in keys {
                let d = definitions.get(key);
                out.push_str(&format!(
                    "| {WEIGHT}`{key}`</span> | {} | {} |\n",
                    d.map(|d| d.brief.as_str()).unwrap_or(""),
                    stability(d)
                ));
            }
        }
        if !recorded.metrics.is_empty() {
            out.push_str(
                "\n### Metrics\n\n| Metric | Instrument | What it measures, and its labels | Stability |\n|---|---|---|---|\n",
            );
            for (name, (instrument, unit, labels)) in &recorded.metrics {
                let d = definitions.get(name);
                // A `metrics` gauge moved up and down is the conventions'
                // up-down counter.
                let instrument = d
                    .and_then(|d| d.instrument.clone())
                    .filter(|_| instrument == "gauge")
                    .map(|i| words(&i))
                    .unwrap_or_else(|| instrument.clone());
                let unit = unit
                    .clone()
                    .or_else(|| d.and_then(|d| d.unit.clone()))
                    .unwrap_or_default();
                let labels: Vec<String> = labels.iter().map(|l| format!("`{l}`")).collect();
                let labels = match labels.as_slice() {
                    [] => String::new(),
                    labels => format!(" Labels: {}.", labels.join(", ")),
                };
                out.push_str(&format!(
                    "| {WEIGHT}`{name}`</span> | {instrument}, in `{unit}` | {}{labels} | {} |\n",
                    d.map(|d| d.brief.as_str()).unwrap_or(""),
                    stability(d)
                ));
            }
        }
    }
    out.push_str(
        "\n## How this page is kept true\n\n\
         Each crate's tests run its instrumentation, check that every name it records is one the conventions define, with the conventions' instrument and unit, and keep the names in a file that the test fails on when they change. This page is generated from those files, and CI checks that it's up to date, so it can't list a name the code doesn't record, or miss one it does.\n\n\
         oxtel-tokio's metrics aren't in the conventions, which define none for tokio; [its documentation](https://docs.rs/oxtel-tokio) lists them.\n",
    );
    Ok(out)
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `UpDownCounter` as `up-down counter`.
fn words(instrument: &str) -> String {
    match instrument {
        "updowncounter" => "up-down counter".into(),
        other => other.into(),
    }
}

/// Checks the page is what the recorded names and the conventions make.
pub fn check(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let expected = page(root)?;
    match std::fs::read_to_string(root.join(PAGE)) {
        Ok(text) if text == expected => {
            println!("{PAGE} matches the names the crates' tests recorded");
            Ok(())
        }
        _ => Err(format!(
            "{PAGE} is out of date; `cargo run -p xtask -- gen-telemetry` rewrites it"
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_are_read_with_their_stability() {
        let dir = std::env::temp_dir().join(format!("xtask-defs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("defs.rs");
        std::fs::write(
            &file,
            "/// HTTP request method.\n///\n/// More.\n///\n/// Stability: stable.\npub const HTTP_REQUEST_METHOD: &str = \"http.request.method\";\n\n/// Number of active requests.\n///\n/// Stability: development.\n#[cfg(feature = \"experimental\")]\npub const HTTP_SERVER_ACTIVE_REQUESTS: Metric = Metric {\n    name: \"http.server.active_requests\",\n    instrument: Instrument::UpDownCounter,\n    unit: \"{request}\",\n    brief: r#\"Number of active requests.\"#,\n};\n",
        )
        .unwrap();
        let mut defs = BTreeMap::new();
        read_definitions(&file, &mut defs).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let method = &defs["http.request.method"];
        assert_eq!(
            (method.brief.as_str(), method.stability.as_str()),
            ("HTTP request method.", "stable")
        );
        let active = &defs["http.server.active_requests"];
        assert_eq!(active.stability, "development");
        assert_eq!(active.instrument.as_deref(), Some("updowncounter"));
        assert_eq!(active.unit.as_deref(), Some("{request}"));
    }
}

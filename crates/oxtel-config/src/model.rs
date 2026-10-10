//! The configuration model: the parts of the declarative configuration
//! schema oxtel acts on, read from the YAML tree. Unknown keys and features
//! oxtel doesn't support are reported as warnings; values of the wrong type
//! are errors.

use crate::Error;
use crate::yaml::Node;

/// A whole configuration file.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Config {
    /// `file_format`, such as `1.2`.
    pub file_format: String,
    /// `disabled`: no telemetry at all.
    pub disabled: bool,
    /// `resource`.
    pub resource: ResourceConfig,
    /// `attribute_limits`: defaults for every signal.
    pub attribute_limits: Limits,
    /// `propagator`: the propagators' names, if set.
    pub propagators: Option<Vec<String>>,
    /// `tracer_provider`, if set. Without it there are no traces.
    pub tracer_provider: Option<TracerProviderConfig>,
    /// `meter_provider`, if set.
    pub meter_provider: Option<MeterProviderConfig>,
    /// `logger_provider`, if set.
    pub logger_provider: Option<LoggerProviderConfig>,
}

/// An attribute's value.
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeValue {
    /// `string`.
    String(String),
    /// `bool`.
    Bool(bool),
    /// `int`.
    Int(i64),
    /// `double`.
    Double(f64),
    /// `string_array`.
    StringArray(Vec<String>),
    /// `bool_array`.
    BoolArray(Vec<bool>),
    /// `int_array`.
    IntArray(Vec<i64>),
    /// `double_array`.
    DoubleArray(Vec<f64>),
}

/// `resource`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ResourceConfig {
    /// `attributes`.
    pub attributes: Vec<(String, AttributeValue)>,
    /// `attributes_list`: `key=value,...`, like `OTEL_RESOURCE_ATTRIBUTES`.
    /// `attributes` win over it.
    pub attributes_list: Option<String>,
    /// `schema_url`.
    pub schema_url: Option<String>,
    /// `detection/development.detectors`: the detectors' names, if set.
    pub detectors: Option<Vec<String>>,
}

/// Attribute, event and link limits.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Limits {
    /// `attribute_count_limit`.
    pub attribute_count: Option<usize>,
    /// `event_count_limit` (spans).
    pub event_count: Option<usize>,
    /// `link_count_limit` (spans).
    pub link_count: Option<usize>,
}

/// An exporter.
#[derive(Clone, Debug, PartialEq)]
pub enum ExporterConfig {
    /// `otlp_http`.
    OtlpHttp(OtlpHttpConfig),
    /// `console`.
    Console,
}

/// `otlp_http`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct OtlpHttpConfig {
    /// `endpoint`: the full URL, such as `http://localhost:4318/v1/traces`.
    pub endpoint: Option<String>,
    /// `headers`.
    pub headers: Vec<(String, String)>,
    /// `headers_list`: `key=value,...`. `headers` win over it.
    pub headers_list: Option<String>,
    /// `compression`: `gzip` or `none`.
    pub gzip: bool,
    /// `timeout`, in milliseconds.
    pub timeout_ms: Option<u64>,
    /// `encoding`: `json` instead of the default `protobuf`.
    pub json: bool,
}

/// A span or log record processor.
#[derive(Clone, Debug, PartialEq)]
pub enum ProcessorConfig {
    /// `batch`.
    Batch {
        /// Where it exports.
        exporter: ExporterConfig,
        /// `schedule_delay`, in milliseconds.
        schedule_delay_ms: Option<u64>,
        /// `export_timeout`, in milliseconds.
        export_timeout_ms: Option<u64>,
        /// `max_queue_size`.
        max_queue_size: Option<usize>,
        /// `max_export_batch_size`.
        max_export_batch_size: Option<usize>,
    },
    /// `simple`.
    Simple {
        /// Where it exports.
        exporter: ExporterConfig,
    },
}

/// A sampler.
#[derive(Clone, Debug, PartialEq)]
pub enum SamplerConfig {
    /// `always_on`.
    AlwaysOn,
    /// `always_off`.
    AlwaysOff,
    /// `trace_id_ratio_based`.
    TraceIdRatio(f64),
    /// `parent_based`, with its `root`.
    ParentBased(Box<SamplerConfig>),
}

/// `tracer_provider`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TracerProviderConfig {
    /// `processors`.
    pub processors: Vec<ProcessorConfig>,
    /// `limits`.
    pub limits: Limits,
    /// `sampler`, if set.
    pub sampler: Option<SamplerConfig>,
}

/// A metric reader.
#[derive(Clone, Debug, PartialEq)]
pub enum ReaderConfig {
    /// `periodic`.
    Periodic {
        /// Where it exports.
        exporter: ExporterConfig,
        /// `interval`, in milliseconds.
        interval_ms: Option<u64>,
        /// `timeout`, in milliseconds.
        timeout_ms: Option<u64>,
    },
    /// `pull` with `prometheus/development`.
    Prometheus {
        /// `host`.
        host: Option<String>,
        /// `port`.
        port: Option<u16>,
        /// `without_scope_info`.
        without_scope_info: bool,
        /// `without_target_info`.
        without_target_info: bool,
    },
}

/// `meter_provider`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MeterProviderConfig {
    /// `readers`.
    pub readers: Vec<ReaderConfig>,
    /// `exemplar_filter`: `trace_based`, `always_on` or `always_off`.
    pub exemplar_filter: Option<String>,
    /// The smallest `cardinality_limits.default` of the readers.
    pub cardinality_limit: Option<usize>,
}

/// `logger_provider`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LoggerProviderConfig {
    /// `processors`.
    pub processors: Vec<ProcessorConfig>,
    /// `limits`.
    pub limits: Limits,
}

/// A mapping being read: tracks which keys were used, to warn about the
/// rest.
struct Fields<'n> {
    path: String,
    entries: &'n [(String, Node)],
    used: Vec<bool>,
}

impl<'n> Fields<'n> {
    fn of(node: &'n Node, path: &str) -> Result<Self, Error> {
        static EMPTY: Vec<(String, Node)> = Vec::new();
        let entries: &'n [(String, Node)] = match node {
            Node::Map(entries) => entries,
            // `always_on:` with no value is an empty mapping.
            Node::Null => &EMPTY,
            _ => return Err(invalid(path, "a mapping")),
        };
        Ok(Fields {
            path: path.to_string(),
            entries,
            used: vec![false; entries.len()],
        })
    }

    fn get(&mut self, key: &str) -> Option<&'n Node> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        self.used[i] = true;
        Some(&self.entries[i].1).filter(|n| **n != Node::Null)
    }

    fn sub(&self, key: &str) -> String {
        if self.path.is_empty() {
            key.to_string()
        } else {
            format!("{}.{key}", self.path)
        }
    }

    fn str(&mut self, key: &str) -> Result<Option<String>, Error> {
        let path = self.sub(key);
        self.get(key).map(|n| string(n, &path)).transpose()
    }

    fn bool(&mut self, key: &str) -> Result<Option<bool>, Error> {
        let path = self.sub(key);
        match self.get(key) {
            None => Ok(None),
            Some(Node::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(invalid(&path, "a boolean")),
        }
    }

    fn uint(&mut self, key: &str) -> Result<Option<u64>, Error> {
        let path = self.sub(key);
        match self.get(key) {
            None => Ok(None),
            Some(Node::Int(i)) if *i >= 0 => Ok(Some(*i as u64)),
            Some(_) => Err(invalid(&path, "a non-negative integer")),
        }
    }

    fn usize(&mut self, key: &str) -> Result<Option<usize>, Error> {
        Ok(self
            .uint(key)?
            .map(|v| usize::try_from(v).unwrap_or(usize::MAX)))
    }

    fn float(&mut self, key: &str) -> Result<Option<f64>, Error> {
        let path = self.sub(key);
        match self.get(key) {
            None => Ok(None),
            Some(Node::Float(f)) => Ok(Some(*f)),
            Some(Node::Int(i)) => Ok(Some(*i as f64)),
            Some(_) => Err(invalid(&path, "a number")),
        }
    }

    fn map(&mut self, key: &str) -> Result<Option<Fields<'n>>, Error> {
        let path = self.sub(key);
        // A present key with no value is an empty mapping.
        let i = self.entries.iter().position(|(k, _)| k == key);
        match i {
            None => Ok(None),
            Some(i) => {
                self.used[i] = true;
                Fields::of(&self.entries[i].1, &path).map(Some)
            }
        }
    }

    fn list(&mut self, key: &str) -> Result<&'n [Node], Error> {
        let path = self.sub(key);
        match self.get(key) {
            None => Ok(&[]),
            Some(Node::Seq(items)) => Ok(items),
            Some(_) => Err(invalid(&path, "a list")),
        }
    }

    /// Report keys nobody read.
    fn finish(self, warnings: &mut Vec<String>) {
        for ((key, _), used) in self.entries.iter().zip(&self.used) {
            if !*used {
                warnings.push(format!("`{}` isn't supported; ignored", self.sub(key)));
            }
        }
    }
}

fn invalid(path: &str, expected: &str) -> Error {
    Error::Invalid {
        path: path.to_string(),
        expected: expected.to_string(),
    }
}

fn string(node: &Node, path: &str) -> Result<String, Error> {
    match node {
        Node::Str(s) => Ok(s.clone()),
        Node::Int(i) => Ok(i.to_string()),
        Node::Float(f) => Ok(f.to_string()),
        Node::Bool(b) => Ok(b.to_string()),
        _ => Err(invalid(path, "a string")),
    }
}

/// A list item that's a mapping with one key naming its type, such as
/// `- batch: {...}`.
fn one_of<'n>(item: &'n Node, path: &str) -> Result<(&'n str, &'n Node), Error> {
    match item {
        Node::Map(entries) if entries.len() == 1 => Ok((entries[0].0.as_str(), &entries[0].1)),
        _ => Err(invalid(path, "a mapping with exactly one key")),
    }
}

/// Read a whole configuration.
pub(crate) fn config(root: &Node, warnings: &mut Vec<String>) -> Result<Config, Error> {
    let mut f = Fields::of(root, "")?;
    let file_format = f.str("file_format")?.ok_or(Error::FileFormat(None))?;
    let major = file_format.split('.').next().unwrap_or_default();
    if major != "1" {
        return Err(Error::FileFormat(Some(file_format)));
    }
    let mut config = Config {
        file_format,
        disabled: f.bool("disabled")?.unwrap_or(false),
        ..Config::default()
    };
    // The SDK's own log level: oxtel logs its own problems as warnings.
    f.get("log_level");
    if let Some(r) = f.map("resource")? {
        config.resource = resource(r, warnings)?;
    }
    if let Some(mut l) = f.map("attribute_limits")? {
        config.attribute_limits.attribute_count = l.usize("attribute_count_limit")?;
        // oxtel's values from `tracing` are never nested, so any depth
        // limit holds.
        l.get("attribute_value_depth_limit");
        if l.get("attribute_value_length_limit").is_some() {
            warnings.push(
                "`attribute_limits.attribute_value_length_limit` isn't supported yet; ignored"
                    .into(),
            );
        }
        l.finish(warnings);
    }
    if let Some(p) = f.map("propagator")? {
        config.propagators = Some(propagators(p, warnings)?);
    }
    if let Some(t) = f.map("tracer_provider")? {
        config.tracer_provider = Some(tracer_provider(t, warnings)?);
    }
    if let Some(m) = f.map("meter_provider")? {
        config.meter_provider = Some(meter_provider(m, warnings)?);
    }
    if let Some(l) = f.map("logger_provider")? {
        config.logger_provider = Some(logger_provider(l, warnings)?);
    }
    // Instrumentation settings are for instrumentation libraries.
    f.get("instrumentation/development");
    f.finish(warnings);
    Ok(config)
}

fn resource(mut f: Fields<'_>, warnings: &mut Vec<String>) -> Result<ResourceConfig, Error> {
    let mut r = ResourceConfig {
        attributes_list: f.str("attributes_list")?,
        schema_url: f.str("schema_url")?,
        ..ResourceConfig::default()
    };
    for (i, item) in f.list("attributes")?.iter().enumerate() {
        let path = format!("{}.attributes[{i}]", f.path);
        let mut a = Fields::of(item, &path)?;
        let name = a
            .str("name")?
            .ok_or_else(|| invalid(&format!("{path}.name"), "a name"))?;
        let kind = a.str("type")?.unwrap_or_else(|| "string".into());
        let value_path = format!("{path}.value");
        let value = a
            .get("value")
            .ok_or_else(|| invalid(&value_path, "a value"))?;
        r.attributes
            .push((name, attribute_value(value, &kind, &value_path)?));
        a.finish(warnings);
    }
    if let Some(mut d) = f.map("detection/development")? {
        let mut names = Vec::new();
        for (i, item) in d.list("detectors")?.iter().enumerate() {
            let (name, _) = one_of(item, &format!("{}.detectors[{i}]", d.path))?;
            names.push(name.to_string());
        }
        r.detectors = Some(names);
        if d.get("attributes").is_some() {
            warnings.push(
                "`resource.detection/development.attributes` isn't supported yet; every detected attribute is kept"
                    .into(),
            );
        }
        d.finish(warnings);
    }
    f.finish(warnings);
    Ok(r)
}

fn attribute_value(node: &Node, kind: &str, path: &str) -> Result<AttributeValue, Error> {
    let items = || match node {
        Node::Seq(items) => Ok(items.as_slice()),
        _ => Err(invalid(path, "a list")),
    };
    Ok(match kind {
        "string" => AttributeValue::String(string(node, path)?),
        "bool" => match node {
            Node::Bool(b) => AttributeValue::Bool(*b),
            _ => return Err(invalid(path, "a boolean")),
        },
        "int" => match node {
            Node::Int(i) => AttributeValue::Int(*i),
            _ => return Err(invalid(path, "an integer")),
        },
        "double" => match node {
            Node::Float(f) => AttributeValue::Double(*f),
            Node::Int(i) => AttributeValue::Double(*i as f64),
            _ => return Err(invalid(path, "a number")),
        },
        "string_array" => AttributeValue::StringArray(
            items()?
                .iter()
                .map(|n| string(n, path))
                .collect::<Result<_, _>>()?,
        ),
        "bool_array" => AttributeValue::BoolArray(
            items()?
                .iter()
                .map(|n| match n {
                    Node::Bool(b) => Ok(*b),
                    _ => Err(invalid(path, "booleans")),
                })
                .collect::<Result<_, _>>()?,
        ),
        "int_array" => AttributeValue::IntArray(
            items()?
                .iter()
                .map(|n| match n {
                    Node::Int(i) => Ok(*i),
                    _ => Err(invalid(path, "integers")),
                })
                .collect::<Result<_, _>>()?,
        ),
        "double_array" => AttributeValue::DoubleArray(
            items()?
                .iter()
                .map(|n| match n {
                    Node::Float(f) => Ok(*f),
                    Node::Int(i) => Ok(*i as f64),
                    _ => Err(invalid(path, "numbers")),
                })
                .collect::<Result<_, _>>()?,
        ),
        other => return Err(invalid(path, &format!("a known type, not `{other}`"))),
    })
}

fn propagators(mut f: Fields<'_>, warnings: &mut Vec<String>) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for (i, item) in f.list("composite")?.iter().enumerate() {
        let (name, _) = one_of(item, &format!("{}.composite[{i}]", f.path))?;
        names.push(name.to_string());
    }
    if let Some(list) = f.str("composite_list")? {
        for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    }
    f.finish(warnings);
    Ok(names)
}

fn limits(f: &mut Fields<'_>, warnings: &mut Vec<String>) -> Result<Limits, Error> {
    let limits = Limits {
        attribute_count: f.usize("attribute_count_limit")?,
        event_count: f.usize("event_count_limit")?,
        link_count: f.usize("link_count_limit")?,
    };
    f.get("attribute_value_depth_limit");
    for key in [
        "attribute_value_length_limit",
        "event_attribute_count_limit",
        "link_attribute_count_limit",
    ] {
        if f.get(key).is_some() {
            warnings.push(format!("`{}` isn't supported yet; ignored", f.sub(key)));
        }
    }
    Ok(limits)
}

fn tracer_provider(
    mut f: Fields<'_>,
    warnings: &mut Vec<String>,
) -> Result<TracerProviderConfig, Error> {
    let mut t = TracerProviderConfig {
        processors: processors(&mut f, warnings)?,
        ..TracerProviderConfig::default()
    };
    if let Some(mut l) = f.map("limits")? {
        t.limits = limits(&mut l, warnings)?;
        l.finish(warnings);
    }
    let sampler_path = f.sub("sampler");
    if let Some(s) = f.get("sampler") {
        t.sampler = sampler(s, &sampler_path, warnings)?;
    }
    f.finish(warnings);
    Ok(t)
}

fn sampler(
    node: &Node,
    path: &str,
    warnings: &mut Vec<String>,
) -> Result<Option<SamplerConfig>, Error> {
    let (name, body) = one_of(node, path)?;
    let path = format!("{path}.{name}");
    Ok(Some(match name {
        "always_on" => SamplerConfig::AlwaysOn,
        "always_off" => SamplerConfig::AlwaysOff,
        "trace_id_ratio_based" => {
            let mut f = Fields::of(body, &path)?;
            let ratio = f.float("ratio")?.unwrap_or(1.0);
            f.finish(warnings);
            SamplerConfig::TraceIdRatio(ratio)
        }
        "parent_based" => {
            let mut f = Fields::of(body, &path)?;
            let root_path = f.sub("root");
            let root = match f.get("root") {
                Some(r) => sampler(r, &root_path, warnings)?.unwrap_or(SamplerConfig::AlwaysOn),
                None => SamplerConfig::AlwaysOn,
            };
            // oxtel follows a parent's decision; other choices aren't
            // supported yet.
            for (key, expected) in [
                ("remote_parent_sampled", "always_on"),
                ("remote_parent_not_sampled", "always_off"),
                ("local_parent_sampled", "always_on"),
                ("local_parent_not_sampled", "always_off"),
            ] {
                if let Some(child) = f.get(key)
                    && one_of(child, &f.sub(key))?.0 != expected
                {
                    warnings.push(format!(
                        "`{}` other than `{expected}` isn't supported yet; the parent's decision is followed",
                        f.sub(key)
                    ));
                }
            }
            f.finish(warnings);
            SamplerConfig::ParentBased(Box::new(root))
        }
        other => {
            warnings.push(format!(
                "the `{other}` sampler isn't supported; using parent_based with always_on"
            ));
            return Ok(None);
        }
    }))
}

fn processors(
    f: &mut Fields<'_>,
    warnings: &mut Vec<String>,
) -> Result<Vec<ProcessorConfig>, Error> {
    let mut out = Vec::new();
    for (i, item) in f.list("processors")?.iter().enumerate() {
        let path = format!("{}.processors[{i}]", f.path);
        let (kind, body) = one_of(item, &path)?;
        let path = format!("{path}.{kind}");
        let mut p = Fields::of(body, &path)?;
        let Some(exporter) = exporter(&mut p, warnings)? else {
            p.finish(warnings);
            continue;
        };
        match kind {
            "batch" => out.push(ProcessorConfig::Batch {
                exporter,
                schedule_delay_ms: p.uint("schedule_delay")?,
                export_timeout_ms: p.uint("export_timeout")?,
                max_queue_size: p.usize("max_queue_size")?,
                max_export_batch_size: p.usize("max_export_batch_size")?,
            }),
            "simple" => out.push(ProcessorConfig::Simple { exporter }),
            other => warnings.push(format!("the `{other}` processor isn't supported; ignored")),
        }
        p.finish(warnings);
    }
    Ok(out)
}

/// The `exporter` of a processor or reader; `None`, with a warning, if
/// oxtel doesn't support it.
fn exporter(
    f: &mut Fields<'_>,
    warnings: &mut Vec<String>,
) -> Result<Option<ExporterConfig>, Error> {
    let path = f.sub("exporter");
    let Some(node) = f.get("exporter") else {
        return Err(invalid(&path, "an exporter"));
    };
    let (kind, body) = one_of(node, &path)?;
    let path = format!("{path}.{kind}");
    match kind {
        "otlp_http" => {
            let mut e = Fields::of(body, &path)?;
            let mut otlp = OtlpHttpConfig {
                endpoint: e.str("endpoint")?,
                headers_list: e.str("headers_list")?,
                timeout_ms: e.uint("timeout")?,
                ..OtlpHttpConfig::default()
            };
            for (i, item) in e.list("headers")?.iter().enumerate() {
                let hp = format!("{path}.headers[{i}]");
                let mut h = Fields::of(item, &hp)?;
                let name = h.str("name")?.ok_or_else(|| invalid(&hp, "a name"))?;
                let value = h.str("value")?.unwrap_or_default();
                otlp.headers.push((name, value));
                h.finish(warnings);
            }
            match e.str("compression")?.as_deref() {
                None | Some("none") => {}
                Some("gzip") => otlp.gzip = true,
                Some(other) => {
                    return Err(invalid(
                        &e.sub("compression"),
                        &format!("gzip or none, not `{other}`"),
                    ));
                }
            }
            match e.str("encoding")?.as_deref() {
                None | Some("protobuf") => {}
                Some("json") => otlp.json = true,
                Some(other) => {
                    return Err(invalid(
                        &e.sub("encoding"),
                        &format!("protobuf or json, not `{other}`"),
                    ));
                }
            }
            if let Some(mut tls) = e.map("tls")? {
                for key in ["ca_file", "key_file", "cert_file"] {
                    if tls.get(key).is_some() {
                        warnings.push(format!(
                            "`{}` isn't supported yet; the system's roots are used",
                            tls.sub(key)
                        ));
                    }
                }
                tls.finish(warnings);
            }
            match e.str("temporality_preference")?.as_deref() {
                None | Some("cumulative") => {}
                Some(other) => warnings.push(format!(
                    "`{}: {other}` isn't supported yet; metrics are cumulative",
                    e.sub("temporality_preference")
                )),
            }
            match e.str("default_histogram_aggregation")?.as_deref() {
                None | Some("explicit_bucket_histogram") => {}
                Some(other) => warnings.push(format!(
                    "`{}: {other}` isn't supported yet; histograms use explicit buckets",
                    e.sub("default_histogram_aggregation")
                )),
            }
            e.finish(warnings);
            Ok(Some(ExporterConfig::OtlpHttp(otlp)))
        }
        "console" => {
            Fields::of(body, &path)?.finish(warnings);
            Ok(Some(ExporterConfig::Console))
        }
        other => {
            warnings.push(format!("the `{other}` exporter isn't supported; ignored"));
            Ok(None)
        }
    }
}

fn meter_provider(
    mut f: Fields<'_>,
    warnings: &mut Vec<String>,
) -> Result<MeterProviderConfig, Error> {
    let mut m = MeterProviderConfig {
        exemplar_filter: f.str("exemplar_filter")?,
        ..MeterProviderConfig::default()
    };
    for (i, item) in f.list("readers")?.iter().enumerate() {
        let path = format!("{}.readers[{i}]", f.path);
        let (kind, body) = one_of(item, &path)?;
        let path = format!("{path}.{kind}");
        let mut r = Fields::of(body, &path)?;
        if let Some(mut c) = r.map("cardinality_limits")? {
            if let Some(limit) = c.usize("default")? {
                m.cardinality_limit = Some(m.cardinality_limit.map_or(limit, |l| l.min(limit)));
            }
            c.finish(warnings);
        }
        match kind {
            "periodic" => {
                let interval_ms = r.uint("interval")?;
                let timeout_ms = r.uint("timeout")?;
                if let Some(exporter) = exporter(&mut r, warnings)? {
                    m.readers.push(ReaderConfig::Periodic {
                        exporter,
                        interval_ms,
                        timeout_ms,
                    });
                }
            }
            "pull" => {
                let ep = r.sub("exporter");
                let node = r
                    .get("exporter")
                    .ok_or_else(|| invalid(&ep, "an exporter"))?;
                let (name, body) = one_of(node, &ep)?;
                if name == "prometheus/development" {
                    let mut p = Fields::of(body, &format!("{ep}.{name}"))?;
                    let port = p.uint("port")?;
                    m.readers.push(ReaderConfig::Prometheus {
                        host: p.str("host")?,
                        port: port.map(|v| u16::try_from(v).unwrap_or(9464)),
                        without_scope_info: p.bool("without_scope_info")?.unwrap_or(false),
                        without_target_info: p.bool("without_target_info")?.unwrap_or(false),
                    });
                    p.finish(warnings);
                } else {
                    warnings.push(format!(
                        "the `{name}` pull exporter isn't supported; ignored"
                    ));
                }
            }
            other => warnings.push(format!("the `{other}` reader isn't supported; ignored")),
        }
        r.finish(warnings);
    }
    if f.get("views").is_some() {
        warnings.push("`meter_provider.views` isn't supported yet; ignored".into());
    }
    f.finish(warnings);
    Ok(m)
}

fn logger_provider(
    mut f: Fields<'_>,
    warnings: &mut Vec<String>,
) -> Result<LoggerProviderConfig, Error> {
    let mut l = LoggerProviderConfig {
        processors: processors(&mut f, warnings)?,
        ..LoggerProviderConfig::default()
    };
    if let Some(mut lim) = f.map("limits")? {
        l.limits = limits(&mut lim, warnings)?;
        lim.finish(warnings);
    }
    f.finish(warnings);
    Ok(l)
}

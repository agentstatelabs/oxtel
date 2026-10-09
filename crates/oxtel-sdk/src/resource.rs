//! The resource: attributes describing what produced the telemetry, such as
//! `service.name`.

use crate::value::{KeyValue, Value, upsert};

/// Attributes that describe the entity producing telemetry.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Resource {
    attributes: Vec<KeyValue>,
    schema_url: Option<String>,
}

impl Resource {
    /// A builder. Its resource includes the SDK's own attributes and the
    /// environment (`OTEL_SERVICE_NAME`, `OTEL_RESOURCE_ATTRIBUTES`) unless
    /// told otherwise.
    pub fn builder() -> ResourceBuilder {
        ResourceBuilder::default()
    }

    /// The resource from the environment and the SDK's defaults alone.
    pub fn from_env() -> Self {
        Resource::builder().build()
    }

    /// The attributes.
    pub fn attributes(&self) -> &[KeyValue] {
        &self.attributes
    }

    /// The value for `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.attributes
            .iter()
            .find(|kv| kv.key == key)
            .map(|kv| &kv.value)
    }

    /// The schema URL of the attributes' semantic conventions.
    pub fn schema_url(&self) -> Option<&str> {
        self.schema_url.as_deref()
    }
}

/// Builds a [`Resource`].
///
/// Precedence, highest first: attributes set on the builder; then
/// `OTEL_SERVICE_NAME`; then `OTEL_RESOURCE_ATTRIBUTES`; then a default
/// service name set with [`ResourceBuilder::with_default_service_name`];
/// then `unknown_service:<executable>`.
#[derive(Debug)]
pub struct ResourceBuilder {
    attributes: Vec<KeyValue>,
    default_service_name: Option<String>,
    schema_url: Option<String>,
    read_env: bool,
}

impl Default for ResourceBuilder {
    fn default() -> Self {
        ResourceBuilder {
            attributes: Vec::new(),
            default_service_name: None,
            schema_url: None,
            read_env: true,
        }
    }
}

impl ResourceBuilder {
    /// Set `service.name`, overriding the environment.
    pub fn with_service_name(self, name: impl Into<String>) -> Self {
        self.with_attribute(KeyValue::new("service.name", name.into()))
    }

    /// The service name to use when the environment doesn't name one. Use
    /// this in a program that should let operators rename it.
    pub fn with_default_service_name(mut self, name: impl Into<String>) -> Self {
        self.default_service_name = Some(name.into());
        self
    }

    /// Set an attribute, overriding the environment.
    pub fn with_attribute(mut self, kv: KeyValue) -> Self {
        upsert(&mut self.attributes, kv);
        self
    }

    /// Set the schema URL.
    pub fn with_schema_url(mut self, url: impl Into<String>) -> Self {
        self.schema_url = Some(url.into());
        self
    }

    /// Don't read `OTEL_SERVICE_NAME` or `OTEL_RESOURCE_ATTRIBUTES`.
    pub fn without_env(mut self) -> Self {
        self.read_env = false;
        self
    }

    /// The resource.
    pub fn build(self) -> Resource {
        let mut attributes = vec![
            KeyValue::new("telemetry.sdk.name", "oxtel"),
            KeyValue::new("telemetry.sdk.language", "rust"),
            KeyValue::new("telemetry.sdk.version", env!("CARGO_PKG_VERSION")),
        ];
        let service_name = self.default_service_name.unwrap_or_else(|| {
            let exe = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            format!("unknown_service:{exe}")
        });
        upsert(&mut attributes, KeyValue::new("service.name", service_name));
        if self.read_env {
            if let Ok(raw) = std::env::var("OTEL_RESOURCE_ATTRIBUTES") {
                for kv in parse_resource_attributes(&raw) {
                    upsert(&mut attributes, kv);
                }
            }
            if let Ok(name) = std::env::var("OTEL_SERVICE_NAME")
                && !name.trim().is_empty()
            {
                upsert(&mut attributes, KeyValue::new("service.name", name.trim()));
            }
        }
        for kv in self.attributes {
            upsert(&mut attributes, kv);
        }
        Resource {
            attributes,
            schema_url: self.schema_url,
        }
    }
}

/// `key1=value1,key2=value2`, values percent-decoded. One malformed pair
/// discards the whole value, as the specification asks.
pub fn parse_resource_attributes(raw: &str) -> Vec<KeyValue> {
    let mut out = Vec::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((key, value)) = pair.split_once('=') else {
            return Vec::new();
        };
        let key = key.trim();
        let Some(value) = percent_decode(value.trim()) else {
            return Vec::new();
        };
        if key.is_empty() {
            return Vec::new();
        }
        out.push(KeyValue::new(key.to_string(), value));
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(s.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_attributes_parse_and_discard_on_error() {
        let kvs =
            parse_resource_attributes("service.name=checkout, deployment.environment=prod%20eu");
        assert_eq!(kvs.len(), 2);
        assert_eq!(kvs[1].value, Value::String("prod eu".into()));
        assert!(parse_resource_attributes("a=1,broken").is_empty());
        assert!(parse_resource_attributes("a=%zz").is_empty());
    }

    #[test]
    fn builder_precedence_without_env() {
        let r = Resource::builder()
            .without_env()
            .with_default_service_name("asg")
            .with_attribute(KeyValue::new("deployment.environment.name", "dev"))
            .build();
        assert_eq!(r.get("service.name"), Some(&Value::String("asg".into())));
        assert_eq!(
            r.get("telemetry.sdk.name"),
            Some(&Value::String("oxtel".into()))
        );
        let r = Resource::builder()
            .without_env()
            .with_default_service_name("asg")
            .with_service_name("forced")
            .build();
        assert_eq!(r.get("service.name"), Some(&Value::String("forced".into())));
        let r = Resource::builder().without_env().build();
        let Some(Value::String(name)) = r.get("service.name") else {
            panic!("no service.name")
        };
        assert!(name.starts_with("unknown_service:"), "{name}");
    }
}

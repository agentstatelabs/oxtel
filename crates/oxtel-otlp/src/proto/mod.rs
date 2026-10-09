//! OTLP protocol types, generated from the vendored definitions (release
//! v1.11.1) by `cargo run -p xtask -- gen-proto`. Modules mirror the
//! protobuf packages under `opentelemetry.proto`.

#![allow(missing_docs, clippy::all, rustdoc::all)]

pub mod common {
    pub mod v1 {
        include!("gen/opentelemetry.proto.common.v1.rs");
    }
}

pub mod resource {
    pub mod v1 {
        include!("gen/opentelemetry.proto.resource.v1.rs");
    }
}

pub mod trace {
    pub mod v1 {
        include!("gen/opentelemetry.proto.trace.v1.rs");
    }
}

pub mod metrics {
    pub mod v1 {
        include!("gen/opentelemetry.proto.metrics.v1.rs");
    }
}

pub mod logs {
    pub mod v1 {
        include!("gen/opentelemetry.proto.logs.v1.rs");
    }
}

pub mod collector {
    pub mod trace {
        pub mod v1 {
            include!("gen/opentelemetry.proto.collector.trace.v1.rs");
        }
    }

    pub mod metrics {
        pub mod v1 {
            include!("gen/opentelemetry.proto.collector.metrics.v1.rs");
        }
    }

    pub mod logs {
        pub mod v1 {
            include!("gen/opentelemetry.proto.collector.logs.v1.rs");
        }
    }
}

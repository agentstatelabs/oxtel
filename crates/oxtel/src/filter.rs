//! The log layer's filter: events by the log directives, spans by the
//! trace directives.

use tracing_core::subscriber::Interest;
use tracing_core::{LevelFilter, Metadata};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Context, Filter};

/// Lets through the events the log directives allow, and the spans the
/// trace directives allow. A log record finds its trace context through the
/// spans its layer sees, so it must see the same spans the trace layer
/// records: no fewer, or records would name an ancestor instead of their
/// span; no more, or the registry would keep spans nobody records.
pub(crate) struct LogFilter {
    pub(crate) events: Targets,
    pub(crate) spans: Targets,
}

impl LogFilter {
    fn allows(&self, meta: &Metadata<'_>) -> bool {
        let directives = if meta.is_span() {
            &self.spans
        } else {
            &self.events
        };
        directives.would_enable(meta.target(), meta.level())
    }
}

impl<S> Filter<S> for LogFilter {
    fn enabled(&self, meta: &Metadata<'_>, _: &Context<'_, S>) -> bool {
        self.allows(meta)
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> Interest {
        // Directives depend only on the target and level, so the answer for
        // a callsite never changes.
        if self.allows(meta) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        let events = <Targets as Filter<S>>::max_level_hint(&self.events)?;
        let spans = <Targets as Filter<S>>::max_level_hint(&self.spans)?;
        Some(events.max(spans))
    }
}

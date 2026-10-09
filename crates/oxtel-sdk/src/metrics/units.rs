//! The `metrics` facade's units, in UCUM as OpenTelemetry writes them.

use metrics::Unit;

/// `unit` in UCUM, or `None` for a plain count.
pub fn ucum(unit: Unit) -> Option<&'static str> {
    Some(match unit {
        Unit::Count => return None,
        Unit::Percent => "%",
        Unit::Seconds => "s",
        Unit::Milliseconds => "ms",
        Unit::Microseconds => "us",
        Unit::Nanoseconds => "ns",
        Unit::Tebibytes => "TiBy",
        Unit::Gibibytes => "GiBy",
        Unit::Mebibytes => "MiBy",
        Unit::Kibibytes => "KiBy",
        Unit::Bytes => "By",
        Unit::TerabitsPerSecond => "Tbit/s",
        Unit::GigabitsPerSecond => "Gbit/s",
        Unit::MegabitsPerSecond => "Mbit/s",
        Unit::KilobitsPerSecond => "kbit/s",
        Unit::BitsPerSecond => "bit/s",
        Unit::CountPerSecond => "{count}/s",
    })
}

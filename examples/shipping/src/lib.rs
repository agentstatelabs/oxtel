//! Shipping quotes: an example library, instrumented with `tracing` and
//! `metrics` alone. It doesn't depend on oxtel, and doesn't choose where
//! its telemetry goes: the application that uses it does.

use std::fmt;

/// Why a parcel can't be quoted.
#[derive(Debug)]
pub enum Error {
    /// The country isn't one this library ships to.
    UnknownCountry(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnknownCountry(country) => write!(f, "no shipping to {country}"),
        }
    }
}

impl std::error::Error for Error {}

/// The price, in cents, of sending a parcel of `grams` to `country`.
///
/// Each call is a span named `quote`. A parcel heavy enough for the
/// freight surcharge is logged as a warning, and every call is counted in
/// `shipping.quotes`, by outcome.
#[tracing::instrument]
pub fn quote(grams: u32, country: &str) -> Result<u32, Error> {
    let (base, per_kilogram) = match country {
        "FR" | "DE" | "NL" => (450, 120),
        "US" | "CA" => (1200, 300),
        _ => {
            metrics::counter!("shipping.quotes", "outcome" => "unknown_country").increment(1);
            // Returned, not logged: the caller decides whether it matters.
            return Err(Error::UnknownCountry(country.to_string()));
        }
    };
    let mut cents = base + grams.div_ceil(1000) * per_kilogram;
    if grams > 20_000 {
        tracing::warn!("over 20 kg; adding the freight surcharge");
        cents += 2500;
    }
    // Details, for when someone turns this library's logs up to `debug`.
    tracing::debug!(base, per_kilogram, cents, "priced");
    metrics::counter!("shipping.quotes", "outcome" => "ok").increment(1);
    Ok(cents)
}

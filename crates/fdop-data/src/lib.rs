//! fdop-data: ADS-B archive discovery and streaming ingest (adsb.lol `globe_history`),
//! track-store building with altitude reconciliation, a per-day cache, terrain tiles and
//! geoid lookups.
//!
//! Data flow for a search window: [`fetch::fetch_window`] -> per UTC day
//! [`adsblol::find_day_releases`] -> [`stream::BigStreamReader`] over the release parts ->
//! [`ingest::ingest_reader`] (tar walk, gunzip, parallel JSON parse, bbox/time filter) ->
//! [`build::build_store`] (merge feeds, split legs, geom/baro reconciliation) -> cached
//! `.fdtrk` -> [`build::merge_stores`] across days -> `TrackStore::filtered` to the window.

pub mod adsblol;
pub mod build;
pub mod fetch;
pub mod geoid;
pub mod ingest;
pub mod stream;
pub mod terrain;
pub mod weather;

pub use adsblol::{AssetPart, DayRelease, Feed, find_day_releases};
pub use build::{build_store, merge_stores};
pub use fetch::{FetchOptions, fetch_window, published_until};
pub use geoid::{geoid_height, geoid_height_or_default, geoid_height_with_options};
pub use ingest::{
    IngestProgress, IngestSpec, IngestStats, RawAircraft, RawPoint, ingest_path, ingest_reader,
};
pub use terrain::TerrariumDem;
pub use weather::{Weather, fetch_atmosphere};

use std::fmt;

/// Error type for every fallible call in this crate.
#[derive(Debug, Clone)]
pub struct DataError(pub String);

impl DataError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DataError {}

impl From<fdop_http::HttpError> for DataError {
    fn from(e: fdop_http::HttpError) -> Self {
        Self(e.to_string())
    }
}

impl From<std::io::Error> for DataError {
    fn from(e: std::io::Error) -> Self {
        Self(format!("io: {e}"))
    }
}

impl From<String> for DataError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for DataError {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

pub(crate) fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

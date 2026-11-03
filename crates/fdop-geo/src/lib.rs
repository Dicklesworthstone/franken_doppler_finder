//! fdop-geo: region parsing, Census gazetteer and reverse geocoding, reports and maps.
//!
//! Every API here is blocking. Network access goes through [`fdop_http::Net`]; every
//! network-backed function has an offline fallback (embedded state table, cached gazetteer)
//! so a run without connectivity still produces an answer and a map.

pub mod gazetteer;
pub mod geocode;
pub mod region;
pub mod report;
pub mod states;
pub mod staticmap;

mod font;

use std::fmt;
use std::path::PathBuf;

/// Errors from this crate. `Display` is meant for end users.
#[derive(Debug, Clone)]
pub enum GeoError {
    /// Input text could not be understood as a region.
    Parse(String),
    /// Input matched several things; the message lists the candidates.
    Ambiguous(String),
    /// A lookup found nothing.
    NotFound(String),
    /// The requested lookup needs data that is not cached and no network was given.
    Offline(String),
    /// Network or HTTP failure.
    Network(String),
    /// Malformed downloaded data.
    Data(String),
    /// Local filesystem failure.
    Io(String),
}

impl fmt::Display for GeoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GeoError::Parse(m) => write!(f, "could not parse location: {m}"),
            GeoError::Ambiguous(m) => write!(f, "ambiguous location: {m}"),
            GeoError::NotFound(m) => write!(f, "not found: {m}"),
            GeoError::Offline(m) => write!(f, "needs network or a cached file: {m}"),
            GeoError::Network(m) => write!(f, "network error: {m}"),
            GeoError::Data(m) => write!(f, "bad data: {m}"),
            GeoError::Io(m) => write!(f, "i/o error: {m}"),
        }
    }
}

impl std::error::Error for GeoError {}

impl From<fdop_http::HttpError> for GeoError {
    fn from(e: fdop_http::HttpError) -> Self {
        GeoError::Network(e.to_string())
    }
}

impl From<std::io::Error> for GeoError {
    fn from(e: std::io::Error) -> Self {
        GeoError::Io(e.to_string())
    }
}

/// Default cache root, shared with every crate: see [`fdop_types::default_cache_dir`].
pub fn default_cache_dir() -> PathBuf {
    fdop_types::default_cache_dir()
}

/// Minimal standard base64 encoder (RFC 4648, with padding).
pub(crate) fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Escape text for HTML / XML element content and attribute values.
pub(crate) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn escape() {
        assert_eq!(esc("a<b & \"c\""), "a&lt;b &amp; &quot;c&quot;");
    }
}

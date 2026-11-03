//! Resumable streaming reader for multi-GB release assets.
//!
//! `fdop_http::StreamReader` streams with a 1 TiB body cap (adsb.lol prod parts are 2 GB) and
//! resumes dropped connections with `Range`; this alias keeps the historical name.

pub use fdop_http::StreamReader as BigStreamReader;

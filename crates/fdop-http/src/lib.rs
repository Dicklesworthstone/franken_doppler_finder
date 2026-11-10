//! Blocking facade over the asupersync HTTP/1.1 client.
//!
//! Every call runs on a private current-thread asupersync runtime owned by a [`Net`], so a
//! `Net` is cheap to create per worker thread. Large downloads (multi-GB ADS-B archives) are
//! consumed as a [`StreamReader`] (`std::io::Read`) fed by a background download thread that
//! resumes with HTTP `Range` requests after a dropped connection.

use asupersync::bytes::Buf;
use asupersync::cx::Cx;
use asupersync::http::body::{Body, Frame};
use asupersync::http::h1::HttpClient;
use asupersync::http::h1::types::Method;
use asupersync::runtime::{Runtime, RuntimeBuilder};
use std::fmt;
use std::future::poll_fn;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

/// Generic client identity (no personal contact data is ever sent).
pub const USER_AGENT: &str =
    "franken_doppler_finder/0.1 (aircraft-doppler geolocation research tool)";

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);

/// Reserve a private sibling of the destination. Exclusive creation also skips files
/// left by an interrupted process whose PID has since been reused.
fn staging_file(path: &Path) -> io::Result<(PathBuf, std::fs::File)> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache path has no filename"))?;
    loop {
        let id = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
        let mut temporary = name.to_os_string();
        temporary.push(format!(".part.{}.{id}", std::process::id()));
        let tmp = path.with_file_name(temporary);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Publish a complete cache response with a same-filesystem rename. Concurrent
/// writers own separate staging files; readers see one complete response at a time.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (tmp, mut file) = staging_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)
}

#[derive(Debug, Clone)]
pub struct HttpError {
    pub url: String,
    pub status: Option<u16>,
    pub message: String,
}

impl HttpError {
    fn new(url: &str, status: Option<u16>, message: impl Into<String>) -> Self {
        Self {
            url: url.to_string(),
            status,
            message: message.into(),
        }
    }
    /// Worth retrying: transport failures, 408, 429 and 5xx.
    pub fn transient(&self) -> bool {
        match self.status {
            None => true,
            Some(s) => s == 408 || s == 429 || s >= 500,
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(s) => write!(f, "HTTP {s} for {}: {}", self.url, self.message),
            None => write!(f, "request to {} failed: {}", self.url, self.message),
        }
    }
}

impl std::error::Error for HttpError {}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        header_of(&self.headers, name)
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("invalid JSON: {e}"))
    }
}

fn header_of<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    if end < start || end.checked_add(1).is_none() {
        return None;
    }
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    if total.is_some_and(|n| end >= n) {
        return None;
    }
    Some((start, end, total))
}

/// A strong entity tag identifies the exact representation across resumed ranges.
/// Weak tags and Last-Modified alone cannot prove byte-for-byte identity.
fn strong_etag(headers: &[(String, String)]) -> Option<&str> {
    // ETag is a singleton field. Even equal duplicates are ambiguous metadata,
    // not one strong validator; only HTTP optional whitespace may surround it.
    let mut tags = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("etag"));
    let tag = tags.next()?.1.trim_matches([' ', '\t']);
    if tags.next().is_some() {
        return None;
    }
    let opaque = tag.strip_prefix('"')?.strip_suffix('"')?;
    opaque
        .bytes()
        .all(|b| b == 0x21 || (0x23..=0x7e).contains(&b) || b >= 0x80)
        .then_some(tag)
}

#[derive(Default)]
struct StreamIdentity {
    etag: Option<String>,
}

impl StreamIdentity {
    fn resume_etag(&self) -> Result<&str, String> {
        self.etag.as_deref().ok_or_else(|| {
            "cannot safely resume without a single strong initial ETag; restart the download"
                .to_string()
        })
    }

    fn accept(&mut self, start_at: u64, headers: &[(String, String)]) -> Result<(), String> {
        let current = strong_etag(headers);
        if start_at == 0 {
            // No byte from an earlier response reached the sink, so this response
            // can establish a new identity even after a failed initial request.
            self.etag = current.map(str::to_owned);
        } else if current != Some(self.resume_etag()?) {
            // The sink may already be an ingesting tar reader: it cannot rewind
            // the delivered prefix. Refuse before forwarding any replacement bytes,
            // including a full 200 response after an If-Range mismatch.
            return Err("response ETag changed or disappeared during resume".into());
        }
        Ok(())
    }
}

/// Exclusive byte offsets in the representation, not HTTP message-body lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamExtent {
    end: Option<u64>,
    total: Option<u64>,
    partial: bool,
}

impl StreamExtent {
    fn from_response(
        status: u16,
        headers: &[(String, String)],
        start_at: u64,
        previous_total: Option<u64>,
    ) -> Result<Self, String> {
        if status == 416 && start_at > 0 {
            let total = header_of(headers, "content-range")
                .and_then(|v| v.trim().strip_prefix("bytes */"))
                .and_then(|n| n.parse::<u64>().ok());
            if total != Some(start_at) || previous_total.is_some_and(|old| total != Some(old)) {
                return Err("unsatisfied range does not prove the downloaded length".into());
            }
            return Ok(Self {
                end: total,
                total,
                partial: false,
            });
        }
        if start_at > 0 && !matches!(status, 200 | 206) {
            return Err("resume refused".into());
        }
        if !(200..300).contains(&status) {
            return Err("unexpected status".into());
        }
        let content_length =
            header_of(headers, "content-length").and_then(|v| v.trim().parse::<u64>().ok());
        let (end, total) = if status == 206 {
            let raw = header_of(headers, "content-range")
                .ok_or("partial response missing Content-Range")?;
            let (start, end, total) =
                parse_content_range(raw).ok_or_else(|| format!("invalid Content-Range: {raw}"))?;
            if start != start_at {
                return Err(format!(
                    "resume started at byte {start}, expected {start_at}"
                ));
            }
            let range_len = end - start + 1;
            if let Some(cl) = content_length
                && cl != range_len
            {
                return Err(format!(
                    "partial Content-Length {cl} disagrees with Content-Range length {range_len}"
                ));
            }
            // Content-Length describes this range only. Even an open-ended
            // request may be fulfilled in smaller pieces (RFC 9110, 15.3.7).
            (Some(end + 1), total)
        } else {
            (content_length, content_length)
        };
        if let (Some(old), Some(new)) = (previous_total, total)
            && old != new
        {
            return Err(format!("response total changed from {old} to {new} bytes"));
        }
        let total = total.or(previous_total);
        let end = if status == 206 { end } else { total };
        if end.is_some_and(|n| n < start_at || total.is_some_and(|t| n > t)) {
            return Err("response byte range lies outside the declared object length".into());
        }
        Ok(Self {
            end,
            total,
            partial: status == 206,
        })
    }

    fn check_position(self, next: u64) -> Result<(), String> {
        if let Some(end) = self.end
            && next > end
        {
            return Err(format!("response body exceeds declared end at byte {end}"));
        }
        Ok(())
    }

    fn complete(self, delivered: u64, skip: u64) -> Result<bool, String> {
        self.check_position(delivered)?;
        if skip > 0 || self.end.is_some_and(|end| delivered < end) {
            return Err(format!("truncated response at byte {delivered}"));
        }
        // A clean partial-response EOF proves only that this range ended. An
        // unknown total needs another range, or a matching 416 bytes */N response.
        Ok(!self.partial || self.total == Some(delivered))
    }
}

/// One asupersync runtime + one pooled HTTP client.
pub struct Net {
    rt: Runtime,
    /// Buffered requests: bodies capped at 1 GiB.
    client: HttpClient,
    /// Streaming requests: asupersync applies `max_body_size` to streamed bodies too, and
    /// adsb.lol prod parts are 2 GB each, so streams get a 1 TiB cap.
    stream_client: HttpClient,
    retries: u32,
    /// A streamed body that sends nothing for this long is treated as a dropped
    /// connection and resumed: `request_timeout` does not cover streamed bodies, and a
    /// CDN connection can stall with the socket still open.
    body_idle: Duration,
}

/// Default for [`Net::with_body_idle`].
pub const BODY_IDLE: Duration = Duration::from_secs(60);

impl Net {
    pub fn new() -> Result<Self, HttpError> {
        Self::with_timeout(Duration::from_secs(120))
    }

    /// Client whose buffered requests give up after `timeout`.
    pub fn with_timeout(timeout: Duration) -> Result<Self, HttpError> {
        let reactor = asupersync::runtime::reactor::create_reactor()
            .map_err(|e| HttpError::new("-", None, format!("reactor: {e}")))?;
        let rt = RuntimeBuilder::current_thread()
            .with_reactor(reactor)
            .build()
            .map_err(|e| HttpError::new("-", None, format!("runtime: {e:?}")))?;
        let builder = || {
            HttpClient::builder()
                .user_agent(USER_AGENT)
                .max_redirects(10)
        };
        let client = builder()
            .request_timeout(timeout)
            .max_body_size(1 << 30)
            .build();
        let stream_client = builder()
            .request_timeout(Duration::from_secs(120))
            .max_body_size(1 << 40)
            .build();
        Ok(Self {
            rt,
            client,
            stream_client,
            retries: 4,
            body_idle: BODY_IDLE,
        })
    }

    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    /// How long a streamed body may send nothing before the stream resumes with `Range`.
    pub fn with_body_idle(mut self, idle: Duration) -> Self {
        self.body_idle = idle;
        self
    }

    /// Buffered GET. Non-2xx statuses are returned as errors (body text in the message).
    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, HttpError> {
        let hdrs: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut last = None;
        for attempt in 0..=self.retries {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(500 * (1 << attempt.min(5))));
            }
            let client = self.client.clone();
            let h = hdrs.clone();
            let u = url.to_string();
            let res = self.rt.block_on(async move {
                let cx = Cx::current()
                    .ok_or_else(|| HttpError::new(&u, None, "no asupersync context"))?;
                let r = client
                    .request(&cx, Method::Get, &u, h, Vec::new())
                    .await
                    .map_err(|e| HttpError::new(&u, None, format!("{e:?}")))?;
                Ok::<_, HttpError>(HttpResponse {
                    status: r.status,
                    headers: r.headers,
                    body: r.body,
                })
            });
            match res {
                Ok(r) if (200..300).contains(&r.status) => return Ok(r),
                Ok(r) => {
                    let msg: String = r.text().chars().take(300).collect();
                    let e = HttpError::new(url, Some(r.status), msg);
                    if !e.transient() {
                        return Err(e);
                    }
                    last = Some(e);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| HttpError::new(url, None, "no attempt made")))
    }

    /// GET into `path` atomically (private temp file + rename). Returns bytes written.
    pub fn download_to(&self, url: &str, path: &Path) -> Result<u64, HttpError> {
        self.download_to_validated(url, path, |_| Ok(()))
            .map(|(n, ())| n)
    }

    /// Validate the completed private download before replacing a published cache.
    /// Return the parsed value from that same file, even if another writer publishes
    /// a newer response immediately afterward. Validation failures preserve the cache.
    pub fn download_to_validated<T>(
        &self,
        url: &str,
        path: &Path,
        validate: impl FnOnce(&Path) -> Result<T, String>,
    ) -> Result<(u64, T), HttpError> {
        let (tmp, mut f) = staging_file(path)
            .map_err(|e| HttpError::new(url, None, format!("create: {e}")))?;
        let n = self.stream(url, &[], &AtomicBool::new(false), &mut |chunk| {
            f.write_all(chunk).map_err(|e| format!("write: {e}"))
        })?;
        f.sync_all()
            .map_err(|e| HttpError::new(url, None, format!("sync: {e}")))?;
        drop(f);
        let value = validate(&tmp)
            .map_err(|e| HttpError::new(url, None, format!("download validation: {e}")))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| HttpError::new(url, None, format!("rename: {e}")))?;
        Ok((n, value))
    }

    /// Stream a GET body into `sink`, resuming with `Range` after transport errors.
    /// `cancel` aborts between chunks. Returns the total body length delivered.
    /// Caller-supplied `Range` and `If-Range` headers are ignored: resumption owns them.
    /// A strong initial ETag is pinned with If-Range and checked before resumed bytes
    /// reach the sink. Without one, a complete response is usable but an interrupted
    /// or partial response fails: a later tag cannot authenticate an earlier prefix.
    pub fn stream(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<u64, HttpError> {
        let mut delivered: u64 = 0;
        let mut expected_total: Option<u64> = None;
        let mut identity = StreamIdentity::default();
        let mut attempt = 0u32;
        loop {
            let mut hdrs: Vec<(String, String)> = headers
                .iter()
                .filter(|(k, _)| {
                    !k.eq_ignore_ascii_case("range") && !k.eq_ignore_ascii_case("if-range")
                })
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            if delivered > 0 {
                // The consumer cannot retract its prefix. Do not even request an
                // unverified suffix; 499 is the existing terminal client-error marker.
                let tag = identity
                    .resume_etag()
                    .map_err(|m| HttpError::new(url, Some(499), m))?;
                hdrs.push(("Range".into(), format!("bytes={delivered}-")));
                hdrs.push(("If-Range".into(), tag.to_string()));
            }
            let client = self.stream_client.clone();
            let u = url.to_string();
            let start_at = delivered;
            let idle = self.body_idle;
            let mut got_this_attempt: u64 = 0;
            let mut skip: u64 = 0;
            let outcome: Result<bool, HttpError> = self.rt.block_on(async {
                let cx = Cx::current()
                    .ok_or_else(|| HttpError::new(&u, None, "no asupersync context"))?;
                let mut resp = client
                    .request_streaming(&cx, Method::Get, &u, hdrs, Vec::new())
                    .await
                    .map_err(|e| HttpError::new(&u, None, format!("{e:?}")))?;
                let status = resp.head.status;
                let extent = StreamExtent::from_response(
                    status,
                    &resp.head.headers,
                    start_at,
                    if start_at == 0 { None } else { expected_total },
                )
                .map_err(|m| HttpError::new(&u, Some(status), m))?;
                identity
                    .accept(start_at, &resp.head.headers)
                    .map_err(|m| HttpError::new(&u, Some(status), m))?;
                expected_total = extent.total;
                if status == 416 {
                    return Ok(true); // validated bytes */N proves that the prefix is complete
                }
                if status == 200 {
                    skip = start_at; // server ignored Range: discard what we already have
                }
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(HttpError::new(&u, Some(499), "cancelled"));
                    }
                    let next = poll_fn(|task| Pin::new(&mut resp.body).poll_frame(task));
                    let frame = asupersync::time::timeout(asupersync::time::wall_now(), idle, next)
                        .await
                        .map_err(|_| HttpError::new(&u, None, format!("body idle for {idle:?}")))?;
                    match frame {
                        None => {
                            return extent
                                .complete(start_at + got_this_attempt, skip)
                                .map_err(|m| HttpError::new(&u, None, m));
                        }
                        Some(Ok(Frame::Data(mut data))) => {
                            while data.has_remaining() {
                                let chunk = data.chunk();
                                let n = chunk.len();
                                let mut piece = chunk;
                                if skip > 0 {
                                    let k = skip.min(piece.len() as u64) as usize;
                                    piece = &piece[k..];
                                    skip -= k as u64;
                                }
                                if !piece.is_empty() {
                                    let next = start_at
                                        .checked_add(got_this_attempt)
                                        .and_then(|n| n.checked_add(piece.len() as u64))
                                        .ok_or_else(|| {
                                            HttpError::new(&u, Some(status), "body offset overflow")
                                        })?;
                                    extent
                                        .check_position(next)
                                        .map_err(|m| HttpError::new(&u, Some(status), m))?;
                                    sink(piece).map_err(|m| {
                                        HttpError::new(&u, Some(499), format!("sink: {m}"))
                                    })?;
                                    got_this_attempt += piece.len() as u64;
                                }
                                data.advance(n);
                            }
                        }
                        Some(Ok(Frame::Trailers(_))) => {}
                        Some(Err(e)) => {
                            return Err(HttpError::new(&u, None, format!("body: {e:?}")));
                        }
                    }
                }
            });
            delivered += got_this_attempt;
            if got_this_attempt > 0 {
                attempt = 0;
            }
            match outcome {
                Ok(true) => return Ok(delivered),
                Ok(false) => continue, // a complete range, with more of the object still to fetch
                Err(e) if e.status == Some(499) => return Err(e),
                Err(e) if e.transient() => {
                    if got_this_attempt == 0 {
                        attempt += 1;
                    }
                    if attempt > self.retries + 4 {
                        return Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(500 * (1 << attempt.min(5))));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// `std::io::Read` over one or more URLs fetched back to back (e.g. a tar split into
/// `.tar.aa` + `.tar.ab`). The download runs on its own thread with its own runtime;
/// chunks arrive through a bounded channel, so a slow reader applies backpressure.
pub struct StreamReader {
    rx: Receiver<Result<Vec<u8>, String>>,
    cur: Vec<u8>,
    pos: usize,
    finished: bool,
    worker: Option<JoinHandle<()>>,
    progress: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl StreamReader {
    pub fn open(urls: Vec<String>) -> Self {
        let (tx, rx): (SyncSender<Result<Vec<u8>, String>>, _) = sync_channel(32);
        let progress = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p2, c2) = (progress.clone(), cancel.clone());
        let worker = std::thread::Builder::new()
            .name("fdop-http-stream".into())
            .spawn(move || {
                let net = match Net::new() {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                };
                const BATCH: usize = 1 << 20;
                let mut buf: Vec<u8> = Vec::with_capacity(BATCH);
                for url in urls {
                    let res = net.stream(&url, &[], &c2, &mut |chunk| {
                        buf.extend_from_slice(chunk);
                        p2.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                        if buf.len() >= BATCH {
                            let full = std::mem::replace(&mut buf, Vec::with_capacity(BATCH));
                            tx.send(Ok(full))
                                .map_err(|_| "reader dropped".to_string())?;
                        }
                        Ok(())
                    });
                    if let Err(e) = res {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
                if !buf.is_empty() {
                    let _ = tx.send(Ok(buf));
                }
            })
            .expect("spawn download thread");
        Self {
            rx,
            cur: Vec::new(),
            pos: 0,
            finished: false,
            worker: Some(worker),
            progress,
            cancel,
        }
    }

    /// Bytes received from the network so far (may run ahead of what has been read).
    pub fn progress(&self) -> Arc<AtomicU64> {
        self.progress.clone()
    }
}

impl Read for StreamReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.pos < self.cur.len() {
                let n = (self.cur.len() - self.pos).min(out.len());
                out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            if self.finished {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok(Ok(v)) => {
                    self.cur = v;
                    self.pos = 0;
                }
                Ok(Err(e)) => {
                    self.finished = true;
                    return Err(io::Error::other(e));
                }
                Err(_) => {
                    self.finished = true;
                    return Ok(0);
                }
            }
        }
    }
}

impl Drop for StreamReader {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // Drain so a sender blocked on a full channel can observe the cancel and exit.
        while self.rx.try_recv().is_ok() {}
        if let Some(h) = self.worker.take() {
            // Do not block forever on a wedged socket: the thread exits at its next chunk.
            if h.is_finished() {
                let _ = h.join();
            }
        }
    }
}

/// Percent-encode a query component (RFC 3986 unreserved set kept).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_basic() {
        assert_eq!(urlencode("New York, NY"), "New%20York%2C%20NY");
    }

    #[test]
    fn transient_classification() {
        assert!(HttpError::new("u", Some(503), "").transient());
        assert!(HttpError::new("u", None, "").transient());
        assert!(!HttpError::new("u", Some(404), "").transient());
    }

    fn extent_headers(range: &str, length: Option<u64>) -> Vec<(String, String)> {
        let mut headers = vec![("Content-Range".into(), range.into())];
        if let Some(n) = length {
            headers.push(("Content-Length".into(), n.to_string()));
        }
        headers
    }

    #[test]
    fn stream_identity_uses_only_strong_initial_etags() {
        let headers = |tag: &str| vec![("eTaG".to_string(), tag.to_string())];
        for tag in ["\"v1\"", "\"\"", " \t\"v1\"\t "] {
            assert_eq!(strong_etag(&headers(tag)), Some(tag.trim_matches([' ', '\t'])));
        }
        for tag in [
            "W/\"v1\"",
            "v1",
            "\"unfinished",
            "\"a b\"",
            "\"a\"b\"",
            "\"a\nb\"",
            "\u{00a0}\"v1\"",
            "\"v1\"\u{2003}",
            "\"v1\", \"v2\"",
        ] {
            assert!(strong_etag(&headers(tag)).is_none(), "{tag:?}");
        }
        let mut identity = StreamIdentity::default();
        identity.accept(0, &headers("\"v1\"")).unwrap();
        identity.accept(10, &headers("\"v1\"")).unwrap();
        for other in ["\"v1\"", "\"v2\""] {
            let mut duplicates = headers("\"v1\"");
            duplicates.push(("ETAG".to_string(), other.to_string()));
            assert!(strong_etag(&duplicates).is_none());
            assert!(identity.accept(10, &duplicates).is_err());
            assert_eq!(identity.etag.as_deref(), Some("\"v1\""));
        }
        for tags in [headers("\"v2\""), headers("W/\"v1\""), Vec::new()] {
            assert!(identity.accept(10, &tags).is_err());
            assert_eq!(identity.etag.as_deref(), Some("\"v1\""));
        }
        // A failed attempt which delivered nothing has no prefix to protect.
        identity.accept(0, &headers("\"v2\"")).unwrap();
        assert_eq!(identity.etag.as_deref(), Some("\"v2\""));
        identity.accept(0, &[]).unwrap();
        assert!(identity.accept(10, &headers("\"v3\"")).is_err());
        assert!(identity.etag.is_none());
    }

    #[test]
    fn streams_without_a_strong_initial_validator_cannot_resume() {
        for extra in [
            "",
            "ETag: W/\"v1\"\r\n",
            "ETag: bare\r\n",
            "ETag: \"v1\"\r\neTaG: \"v1\"\r\n",
            "ETag: \"v1\"\r\nETag: \"v2\"\r\n",
            "ETag: \"v1\", \"v2\"\r\n",
            "Last-Modified: Tue, 08 Sep 2026 12:00:00 GMT\r\n",
        ] {
            // A complete untagged body stays usable. Neither a dropped 200 nor a
            // complete 206 covering only a prefix may append a later response.
            for (status, length, complete) in
                [(200, 10, true), (200, 100, false), (206, 10, false)]
            {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let addr = listener.local_addr().unwrap();
                let server = std::thread::spawn(move || {
                    let (mut sock, _) = listener.accept().unwrap();
                    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    let request = read_head(&mut sock);
                    assert!(!request.contains("range:"), "{request}");
                    let reason = if status == 206 { "Partial Content" } else { "OK" };
                    let range = if status == 206 {
                        "Content-Range: bytes 0-9/100\r\n"
                    } else {
                        ""
                    };
                    write!(
                        sock,
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: {length}\r\n{extra}{range}Connection: close\r\n\r\n0123456789"
                    )
                    .unwrap();
                    // This fixture accepts exactly one request and closes its listener;
                    // a correctly refused resume must not leave a server waiting.
                });
                let mut got = Vec::new();
                let result = Net::new().unwrap().with_retries(0).stream(
                    &format!("http://{addr}/blob"),
                    &[],
                    &AtomicBool::new(false),
                    &mut |chunk| {
                        got.extend_from_slice(chunk);
                        Ok(())
                    },
                );
                server.join().unwrap();
                assert_eq!(got, b"0123456789");
                if complete {
                    assert_eq!(result.unwrap(), 10);
                } else {
                    let error = result.unwrap_err();
                    assert!(
                        error.message.contains("strong initial ETag"),
                        "{extra:?}: {error}"
                    );
                    assert!(!error.transient());
                }
            }
        }
    }

    #[test]
    fn resumed_streams_keep_the_original_representation() {
        for (status, tag, succeeds) in [
            (206, Some("\"Version-One\""), true),
            (200, Some("\"Version-One\""), true),
            (206, Some("\"version-one\""), false),
            (200, Some("\"v2\""), false),
            (206, None, false),
            (206, Some("W/\"Version-One\""), false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut first, _) = listener.accept().unwrap();
                let h1 = read_raw_head(&mut first);
                first
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nETag: \"Version-One\"\r\nConnection: close\r\n\r\n")
                    .unwrap();
                first.write_all(&(0u8..10).collect::<Vec<_>>()).unwrap();
                drop(first);
                let (mut second, _) = listener.accept().unwrap();
                let h2 = read_raw_head(&mut second);
                let start = if status == 206 { 10u8 } else { 0u8 };
                let reason = if status == 206 { "Partial Content" } else { "OK" };
                let mut reply = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    100 - start
                );
                if status == 206 {
                    reply.push_str("Content-Range: bytes 10-99/100\r\n");
                }
                if let Some(tag) = tag {
                    reply.push_str(&format!("ETag: {tag}\r\n"));
                }
                reply.push_str("\r\n");
                let mut reply = reply.into_bytes();
                reply.extend((start..100).map(|n| if succeeds { n } else { 255 }));
                // A refused identity may close the socket before its body is sent.
                let _ = second.write_all(&reply);
                (h1, h2)
            });
            let mut got = Vec::new();
            let result = Net::new()
                .unwrap()
                .with_retries(0)
                .stream(
                    &format!("http://{addr}/blob"),
                    &[("rAnGe", "bytes=42-"), ("If-Range", "\"unrelated\"")],
                    &AtomicBool::new(false),
                    &mut |chunk| {
                        got.extend_from_slice(chunk);
                        Ok(())
                    },
                );
            let (h1, h2) = server.join().unwrap();
            assert!(!h1.to_ascii_lowercase().contains("range:"), "{h1}");
            assert!(h2.to_ascii_lowercase().contains("range: bytes=10-\r\n"), "{h2}");
            let conditions: Vec<_> = h2
                .lines()
                .filter_map(|line| line.split_once(':'))
                .filter(|(name, _)| name.eq_ignore_ascii_case("if-range"))
                .map(|(_, value)| value.trim())
                .collect();
            assert_eq!(conditions, ["\"Version-One\""], "{h2}");
            if succeeds {
                assert_eq!(result.unwrap(), 100);
                assert_eq!(got, (0u8..100).collect::<Vec<_>>());
            } else {
                let error = result.unwrap_err();
                assert!(error.message.contains("ETag"), "{error}");
                assert_eq!(got, (0u8..10).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn range_completion_requires_the_pinned_etag() {
        for tag in [
            Some("\"Version-One\""),
            None,
            Some("\"other\""),
            Some("W/\"Version-One\""),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut first, _) = listener.accept().unwrap();
                read_head(&mut first);
                first
                    .write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 10\r\nContent-Range: bytes 0-9/*\r\nETag: \"Version-One\"\r\nConnection: close\r\n\r\n0123456789")
                    .unwrap();
                drop(first);
                let (mut second, _) = listener.accept().unwrap();
                let request = read_head(&mut second);
                assert!(request.contains("range: bytes=10-"), "{request}");
                let mut reply = String::from(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */10\r\nContent-Length: 0\r\nConnection: close\r\n",
                );
                if let Some(tag) = tag {
                    reply.push_str(&format!("ETag: {tag}\r\n"));
                }
                reply.push_str("\r\n");
                let _ = second.write_all(reply.as_bytes());
            });
            let mut got = Vec::new();
            let result = Net::new().unwrap().stream(
                &format!("http://{addr}/blob"),
                &[],
                &AtomicBool::new(false),
                &mut |chunk| {
                    got.extend_from_slice(chunk);
                    Ok(())
                },
            );
            server.join().unwrap();
            assert_eq!(got, b"0123456789");
            if tag == Some("\"Version-One\"") {
                assert_eq!(result.unwrap(), 10);
            } else {
                let error = result.unwrap_err();
                assert!(error.message.contains("ETag"), "{error}");
            }
        }
    }

    #[test]
    fn partial_ranges_keep_object_length_separate_from_body_length() {
        for previous in [None, Some(100)] {
            let extent = StreamExtent::from_response(
                206,
                &extent_headers("bytes 10-49/*", Some(40)),
                10,
                previous,
            )
            .unwrap();
            assert_eq!(extent.total, previous);
            assert_eq!(extent.end, Some(50));
            assert!(!extent.complete(50, 0).unwrap());
        }
    }

    #[test]
    fn stream_extents_enforce_declared_response_boundaries() {
        for range in [
            "bytes 10-9/100",
            "bytes 10-100/100",
            "bytes 0-0/0",
            "bytes 0-18446744073709551615/*",
        ] {
            assert!(parse_content_range(range).is_none(), "{range}");
        }
        let extent = StreamExtent::from_response(
            206,
            &extent_headers("bytes 10-19/100", None),
            10,
            Some(100),
        )
        .unwrap();
        assert!(extent.check_position(20).is_ok());
        assert!(extent.check_position(21).is_err());
        assert!(extent.complete(19, 0).is_err());
        assert!(!extent.complete(20, 0).unwrap());
        assert!(
            StreamExtent::from_response(200, &extent_headers("", Some(101)), 10, Some(100))
                .is_err()
        );
        let full = StreamExtent::from_response(200, &[], 10, None).unwrap();
        assert!(full.complete(10, 1).is_err()); // ignored Range ended before the skipped prefix
        assert!(full.complete(100, 0).unwrap()); // a complete 200 body does prove EOF
    }

    #[test]
    fn range_eof_requires_a_matching_object_total() {
        let headers = extent_headers("bytes */100", Some(0));
        for previous in [None, Some(100)] {
            let extent = StreamExtent::from_response(416, &headers, 100, previous).unwrap();
            assert!(extent.complete(100, 0).unwrap());
        }
        for (range, start, previous) in [
            ("bytes */99", 100, None),
            ("bytes */101", 100, None),
            ("bytes */100", 100, Some(101)),
            ("bytes */*", 100, None),
            ("bytes */0", 0, None),
        ] {
            assert!(
                StreamExtent::from_response(416, &extent_headers(range, Some(0)), start, previous)
                    .is_err(),
                "{range} at {start} with previous {previous:?}"
            );
        }
        let extent = StreamExtent::from_response(
            206,
            &extent_headers("bytes 90-99/100", Some(10)),
            90,
            None,
        )
        .unwrap();
        assert!(extent.complete(100, 0).unwrap());
    }

    /// Read one request head; return it lower-cased.
    fn read_head(sock: &mut std::net::TcpStream) -> String {
        read_raw_head(sock).to_ascii_lowercase()
    }

    fn read_raw_head(sock: &mut std::net::TcpStream) -> String {
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match sock.read(&mut b) {
                Ok(1) => head.push(b[0]),
                _ => break,
            }
        }
        String::from_utf8_lossy(&head).into_owned()
    }

    fn test_cache_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fdop-http-{label}-{}-{}",
            std::process::id(),
            NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn concurrent_downloads_publish_and_validate_their_own_complete_response() {
        let path = test_cache_path("concurrent").join("table.zip");
        write_atomic(&path, b"previous valid cache").unwrap();
        let old_part = path.with_extension("zip.part");
        std::fs::write(&old_part, b"an interrupted older download").unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let bodies = [vec![b'a'; 128 * 1024], vec![b'b'; 96 * 1024]];
        let mut workers = Vec::new();
        for body in bodies.clone() {
            let barrier = barrier.clone();
            let path = path.clone();
            workers.push(std::thread::spawn(move || {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let addr = listener.local_addr().unwrap();
                let served = body.clone();
                let server = std::thread::spawn(move || {
                    let (mut sock, _) = listener.accept().unwrap();
                    read_head(&mut sock);
                    write!(
                        sock,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        served.len()
                    )
                    .unwrap();
                    let middle = served.len() / 2;
                    sock.write_all(&served[..middle]).unwrap();
                    sock.flush().unwrap();
                    // Both downloads have opened their output and received part of the
                    // response before either can publish it.
                    barrier.wait();
                    sock.write_all(&served[middle..]).unwrap();
                });
                let got = Net::new()
                    .unwrap()
                    .download_to_validated(&format!("http://{addr}/table"), &path, |tmp| {
                        std::fs::read(tmp).map_err(|e| e.to_string())
                    });
                server.join().unwrap();
                let (size, parsed) = got.unwrap();
                assert_eq!(size, body.len() as u64);
                assert_eq!(parsed, body);
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let published = std::fs::read(&path).unwrap();
        assert!(bodies.contains(&published));
        assert_eq!(
            std::fs::read(old_part).unwrap(),
            b"an interrupted older download"
        );
    }

    #[test]
    fn failed_download_or_validation_preserves_the_published_cache() {
        for truncated in [false, true] {
            let path = test_cache_path("failed").join("table.zip");
            write_atomic(&path, b"previous valid cache").unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut sock, _) = listener.accept().unwrap();
                read_head(&mut sock);
                write!(
                    sock,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\ninvalid",
                    if truncated { 70 } else { 7 }
                )
                .unwrap();
            });
            let mut validated = false;
            let error = Net::new()
                .unwrap()
                .with_retries(0)
                .download_to_validated(&format!("http://{addr}/table"), &path, |_| {
                    validated = true;
                    Err::<(), _>("not a valid table".to_string())
                })
                .unwrap_err();
            server.join().unwrap();
            assert_eq!(validated, !truncated);
            if truncated {
                assert!(error.message.contains("strong initial ETag"), "{error}");
                assert!(!error.transient());
            } else {
                assert!(error.message.contains("not a valid table"), "{error}");
            }
            assert_eq!(std::fs::read(path).unwrap(), b"previous valid cache");
        }
    }

    #[test]
    fn concurrent_cache_writes_never_expose_partial_payloads() {
        let path = test_cache_path("atomic").join("weather.json");
        write_atomic(&path, &vec![0; 64 * 1024]).unwrap();
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for byte in 1..=4 {
                let path = &path;
                workers.push(scope.spawn(move || {
                    for _ in 0..8 {
                        write_atomic(path, &vec![byte; 64 * 1024]).unwrap();
                    }
                }));
            }
            while workers.iter().any(|worker| !worker.is_finished()) {
                let bytes = std::fs::read(&path).unwrap();
                assert_eq!(bytes.len(), 64 * 1024);
                assert!(bytes[0] <= 4 && bytes.iter().all(|&b| b == bytes[0]));
                std::thread::yield_now();
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
    }

    /// Serve 10 of 100 bytes and drop, then answer the resume (`Range: bytes=10-`) with
    /// `second_head` and `second_body`. Returns the stream's error text and the bytes the
    /// sink accepted.
    fn serve_truncated_then_partial(
        second_head: &'static [u8],
        second_body: Vec<u8>,
    ) -> (String, Vec<u8>) {
        let body: Vec<u8> = (0..100u8).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let served = body.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("accept 1");
            let _ = read_head(&mut first);
            first
                .write_all(b"HTTP/1.1 200 OK\r\nETag: \"stable\"\r\nContent-Length: 100\r\nConnection: close\r\n\r\n")
                .expect("head 1");
            first.write_all(&served[..10]).expect("part 1");
            first.flush().expect("flush 1");
            drop(first);
            let (mut second, _) = listener.accept().expect("accept 2");
            let h2 = read_head(&mut second);
            second.write_all(second_head).expect("head 2");
            second.write_all(&second_body).expect("part 2");
            second.flush().expect("flush 2");
            h2
        });
        let net = Net::new()
            .expect("net")
            .with_retries(0)
            .with_body_idle(Duration::from_millis(300));
        let mut got = Vec::new();
        let err = net
            .stream(
                &format!("http://{addr}/blob"),
                &[],
                &AtomicBool::new(false),
                &mut |c| {
                    got.extend_from_slice(c);
                    Ok(())
                },
            )
            .expect_err("invalid resume must fail");
        let h2 = server.join().expect("server");
        assert!(h2.contains("range: bytes=10-"), "resume request: {h2}");
        assert!(h2.contains("if-range: \"stable\""), "resume request: {h2}");
        (err.to_string(), got)
    }

    #[test]
    fn resume_rejects_wrong_content_range_start() {
        let (err, got) = serve_truncated_then_partial(b"HTTP/1.1 206 Partial Content\r\nETag: \"stable\"\r\nContent-Length: 91\r\nContent-Range: bytes 9-99/100\r\nConnection: close\r\n\r\n", (9u8..100).collect());
        assert!(err.contains("started at byte 9, expected 10"), "{err}");
        assert_eq!(got, (0u8..10).collect::<Vec<_>>());
    }

    #[test]
    fn resume_rejects_changed_total_length() {
        let (err, got) = serve_truncated_then_partial(b"HTTP/1.1 206 Partial Content\r\nETag: \"stable\"\r\nContent-Length: 90\r\nContent-Range: bytes 10-99/101\r\nConnection: close\r\n\r\n", (10u8..100).collect());
        assert!(err.contains("total changed from 100 to 101 bytes"), "{err}");
        assert_eq!(got, (0u8..10).collect::<Vec<_>>());
    }

    #[test]
    fn resume_rejects_partial_length_mismatch() {
        let (err, got) = serve_truncated_then_partial(b"HTTP/1.1 206 Partial Content\r\nETag: \"stable\"\r\nContent-Length: 89\r\nContent-Range: bytes 10-99/100\r\nConnection: close\r\n\r\n", (10u8..99).collect());
        assert!(
            err.contains("disagrees with Content-Range length 90"),
            "{err}"
        );
        assert_eq!(got, (0u8..10).collect::<Vec<_>>());
    }

    #[test]
    fn resume_rejects_chunked_body_beyond_content_range() {
        let mut body = b"b\r\n".to_vec();
        body.extend(10u8..21);
        body.extend_from_slice(b"\r\n0\r\n\r\n");
        let (err, got) = serve_truncated_then_partial(
            b"HTTP/1.1 206 Partial Content\r\nETag: \"stable\"\r\nTransfer-Encoding: chunked\r\nContent-Range: bytes 10-19/100\r\nConnection: close\r\n\r\n",
            body,
        );
        assert!(err.contains("exceeds declared end at byte 20"), "{err}");
        // A frame may contain some or all of the valid range before the overrun.
        // No byte outside that range may reach the sink.
        assert!((10..=20).contains(&got.len()), "{} bytes", got.len());
        assert_eq!(got, (0u8..got.len() as u8).collect::<Vec<_>>());
    }

    #[test]
    fn resumes_capped_ranges_until_the_whole_object_is_proven_complete() {
        for known_length in [true, false] {
            let body: Vec<u8> = (0..100).collect();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let served = body.clone();
            let server = std::thread::spawn(move || {
                let (mut first, _) = listener.accept().expect("accept first");
                let _ = read_head(&mut first);
                if known_length {
                    first
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nETag: \"stable\"\r\nContent-Length: 100\r\nConnection: close\r\n\r\n",
                        )
                        .expect("head first");
                } else {
                    first.write_all(b"HTTP/1.1 200 OK\r\nETag: \"stable\"\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\na\r\n").expect("head first");
                }
                first.write_all(&served[..10]).expect("first bytes");
                if !known_length {
                    first.write_all(b"\r\n").expect("chunk end");
                }
                drop(first); // incomplete body, including missing final chunk when length is unknown
                // More successful ranges than the no-progress retry budget. Their
                // message lengths must neither become object lengths nor use up retries.
                for start in (10..100).step_by(10) {
                    let (mut sock, _) = listener.accept().expect("accept range");
                    let request = read_head(&mut sock);
                    assert!(
                        request.contains(&format!("range: bytes={start}-")),
                        "{request}"
                    );
                    write!(sock, "HTTP/1.1 206 Partial Content\r\nETag: \"stable\"\r\nContent-Length: 10\r\nContent-Range: bytes {start}-{}/*\r\nConnection: close\r\n\r\n", start + 9).expect("range head");
                    sock.write_all(&served[start..start + 10])
                        .expect("range bytes");
                }
                if !known_length {
                    let (mut sock, _) = listener.accept().expect("accept final probe");
                    let request = read_head(&mut sock);
                    assert!(request.contains("range: bytes=100-"), "{request}");
                    sock.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nETag: \"stable\"\r\nContent-Range: bytes */100\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").expect("complete length");
                }
            });
            let net = Net::new().expect("net").with_retries(0);
            let mut got = Vec::new();
            let n = net
                .stream(
                    &format!("http://{addr}/blob"),
                    &[],
                    &AtomicBool::new(false),
                    &mut |c| {
                        got.extend_from_slice(c);
                        Ok(())
                    },
                )
                .expect("segmented download");
            assert_eq!(n, 100);
            assert_eq!(got, body);
            server.join().expect("server");
        }
    }

    #[test]
    fn ignored_range_resumes_without_duplicating_the_prefix() {
        let body: Vec<u8> = (0..100).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let served = body.clone();
        let server = std::thread::spawn(move || {
            for n in [10, 100] {
                let (mut sock, _) = listener.accept().expect("accept");
                let request = read_head(&mut sock);
                if n == 100 {
                    assert!(request.contains("range: bytes=10-"), "{request}");
                }
                sock.write_all(
                    b"HTTP/1.1 200 OK\r\nETag: \"stable\"\r\nContent-Length: 100\r\nConnection: close\r\n\r\n",
                )
                .expect("head");
                sock.write_all(&served[..n]).expect("bytes");
            }
        });
        let mut got = Vec::new();
        let n = Net::new()
            .expect("net")
            .stream(
                &format!("http://{addr}/blob"),
                &[],
                &AtomicBool::new(false),
                &mut |c| {
                    got.extend_from_slice(c);
                    Ok(())
                },
            )
            .expect("ignored Range download");
        assert_eq!(n, 100);
        assert_eq!(got, body);
        server.join().expect("server");
    }

    /// A server that sends 10 of 100 bytes and then goes silent with the socket open
    /// (the hang seen on a GitHub release download) must not hang `stream`: the idle
    /// timeout fires and the download resumes with `Range`.
    #[test]
    fn stalled_body_resumes_after_idle_timeout() {
        let body: Vec<u8> = (0..100u8).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let served = body.clone();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().expect("accept 1");
            let h1 = read_head(&mut first);
            first
                .write_all(b"HTTP/1.1 200 OK\r\nETag: \"stable\"\r\nContent-Length: 100\r\n\r\n")
                .expect("head 1");
            first.write_all(&served[..10]).expect("part 1");
            first.flush().expect("flush 1");
            // Stall: keep `first` open while the client times out and reconnects.
            let (mut second, _) = listener.accept().expect("accept 2");
            let h2 = read_head(&mut second);
            second
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nETag: \"stable\"\r\nContent-Length: 90\r\n\
                      Content-Range: bytes 10-99/100\r\n\r\n",
                )
                .expect("head 2");
            second.write_all(&served[10..]).expect("part 2");
            second.flush().expect("flush 2");
            drop(first);
            (h1, h2)
        });
        let net = Net::new()
            .expect("net")
            .with_body_idle(Duration::from_millis(300));
        let mut got = Vec::new();
        let t0 = std::time::Instant::now();
        let n = net
            .stream(
                &format!("http://{addr}/blob"),
                &[],
                &AtomicBool::new(false),
                &mut |c| {
                    got.extend_from_slice(c);
                    Ok(())
                },
            )
            .expect("stream");
        let (h1, h2) = server.join().expect("server");
        assert_eq!(n, 100);
        assert_eq!(got, body);
        assert!(!h1.contains("range:"), "first request: {h1}");
        assert!(h2.contains("range: bytes=10-"), "resume request: {h2}");
        assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
    }

    /// `bytes=N-` from a lower-cased request head.
    fn range_start(head: &str) -> u64 {
        head.split("range: bytes=")
            .nth(1)
            .and_then(|r| r.split('-').next())
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    }

    /// A server that drops every connection after 15 bytes needs 7 connections for 100
    /// bytes. Each drop delivered bytes, so the retry budget must not run out (with
    /// `retries = 0` the 5th counted drop ends the stream): only drops without progress count.
    #[test]
    fn drops_with_progress_do_not_exhaust_the_retry_budget() {
        let body: Vec<u8> = (0..100u8).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let served = body.clone();
        let server = std::thread::spawn(move || {
            let mut conns = 0;
            loop {
                let (mut s, _) = listener.accept().expect("accept");
                conns += 1;
                let from = range_start(&read_head(&mut s)) as usize;
                let head = if from == 0 {
                    "HTTP/1.1 200 OK\r\nETag: \"stable\"\r\nContent-Length: 100\r\n\r\n".to_string()
                } else {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nETag: \"stable\"\r\nContent-Length: {}\r\n\
                         Content-Range: bytes {from}-99/100\r\n\r\n",
                        100 - from
                    )
                };
                s.write_all(head.as_bytes()).expect("head");
                let to = (from + 15).min(100);
                s.write_all(&served[from..to]).expect("part");
                s.flush().expect("flush");
                if to == 100 {
                    return conns;
                }
            }
        });
        let net = Net::new().expect("net").with_retries(0);
        let mut got = Vec::new();
        let n = net
            .stream(
                &format!("http://{addr}/blob"),
                &[],
                &AtomicBool::new(false),
                &mut |c| {
                    got.extend_from_slice(c);
                    Ok(())
                },
            )
            .expect("stream");
        assert_eq!(server.join().expect("server"), 7);
        assert_eq!(n, 100);
        assert_eq!(got, body);
    }
}

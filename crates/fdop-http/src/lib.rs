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
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

/// Generic client identity (no personal contact data is ever sent).
pub const USER_AGENT: &str =
    "franken_doppler_finder/0.1 (aircraft-doppler geolocation research tool)";

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
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    if end < start {
        return None;
    }
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    Some((start, end, total))
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

    /// GET into `path` atomically (temp file + rename). Returns bytes written.
    pub fn download_to(&self, url: &str, path: &Path) -> Result<u64, HttpError> {
        let tmp = path.with_extension(format!(
            "{}part",
            path.extension()
                .map(|e| format!("{}.", e.to_string_lossy()))
                .unwrap_or_default()
        ));
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| HttpError::new(url, None, format!("mkdir: {e}")))?;
        }
        let mut f = std::fs::File::create(&tmp)
            .map_err(|e| HttpError::new(url, None, format!("create: {e}")))?;
        let n = self.stream(url, &[], &AtomicBool::new(false), &mut |chunk| {
            f.write_all(chunk).map_err(|e| format!("write: {e}"))
        })?;
        f.sync_all()
            .map_err(|e| HttpError::new(url, None, format!("sync: {e}")))?;
        drop(f);
        std::fs::rename(&tmp, path)
            .map_err(|e| HttpError::new(url, None, format!("rename: {e}")))?;
        Ok(n)
    }

    /// Stream a GET body into `sink`, resuming with `Range` after transport errors.
    /// `cancel` aborts between chunks. Returns the total body length delivered.
    /// Any caller-supplied `Range` header is ignored: resumption owns that header.
    pub fn stream(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        cancel: &AtomicBool,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<u64, HttpError> {
        let mut delivered: u64 = 0;
        let mut expected_total: Option<u64> = None;
        let mut attempt = 0u32;
        loop {
            let mut hdrs: Vec<(String, String)> = headers
                .iter()
                .filter(|(k, _)| !k.eq_ignore_ascii_case("range"))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            if delivered > 0 {
                hdrs.push(("Range".into(), format!("bytes={delivered}-")));
            }
            let client = self.stream_client.clone();
            let u = url.to_string();
            let start_at = delivered;
            let idle = self.body_idle;
            let mut got_this_attempt: u64 = 0;
            let mut skip: u64 = 0;
            let outcome: Result<(), HttpError> = self.rt.block_on(async {
                let cx = Cx::current()
                    .ok_or_else(|| HttpError::new(&u, None, "no asupersync context"))?;
                let mut resp = client
                    .request_streaming(&cx, Method::Get, &u, hdrs, Vec::new())
                    .await
                    .map_err(|e| HttpError::new(&u, None, format!("{e:?}")))?;
                let status = resp.head.status;
                if start_at > 0 {
                    match status {
                        206 => {}
                        200 => skip = start_at, // server ignored Range: discard what we already have
                        s => return Err(HttpError::new(&u, Some(s), "resume refused")),
                    }
                } else if !(200..300).contains(&status) {
                    return Err(HttpError::new(&u, Some(status), "unexpected status"));
                }
                let content_length = header_of(&resp.head.headers, "content-length")
                    .and_then(|v| v.trim().parse::<u64>().ok());
                if status == 206 {
                    let raw = header_of(&resp.head.headers, "content-range").ok_or_else(|| {
                        HttpError::new(&u, Some(status), "partial response missing Content-Range")
                    })?;
                    let (range_start, range_end, range_total) =
                        parse_content_range(raw).ok_or_else(|| {
                            HttpError::new(
                                &u,
                                Some(status),
                                format!("invalid Content-Range: {raw}"),
                            )
                        })?;
                    if range_start != start_at {
                        return Err(HttpError::new(
                            &u,
                            Some(status),
                            format!("resume started at byte {range_start}, expected {start_at}"),
                        ));
                    }
                    let range_len = range_end - range_start + 1;
                    if let Some(cl) = content_length
                        && cl != range_len
                    {
                        return Err(HttpError::new(
                            &u,
                            Some(status),
                            format!(
                                "partial Content-Length {cl} disagrees with Content-Range length {range_len}"
                            ),
                        ));
                    }
                    let total = range_total.or_else(|| content_length.map(|cl| start_at + cl));
                    if let (Some(old), Some(new)) = (expected_total, total)
                        && old != new
                    {
                        return Err(HttpError::new(
                            &u,
                            Some(status),
                            format!("partial response total changed from {old} to {new} bytes"),
                        ));
                    }
                    if expected_total.is_none() {
                        expected_total = total;
                    }
                } else if expected_total.is_none() {
                    expected_total = content_length;
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
                        None => return Ok(()),
                        Some(Ok(Frame::Data(mut data))) => {
                            while data.has_remaining() {
                                let chunk = data.chunk();
                                let n = chunk.len();
                                let mut piece = chunk;
                                if skip > 0 {
                                    let k = (skip as usize).min(piece.len());
                                    piece = &piece[k..];
                                    skip -= k as u64;
                                }
                                if !piece.is_empty() {
                                    if let Some(total) = expected_total
                                        && start_at + got_this_attempt + piece.len() as u64 > total
                                    {
                                        return Err(HttpError::new(
                                            &u,
                                            Some(status),
                                            format!("response body exceeds declared total of {total} bytes"),
                                        ));
                                    }
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
            match outcome {
                Ok(()) => {
                    if let Some(t) = expected_total
                        && delivered < t
                    {
                        // Clean EOF before Content-Length: treat as a dropped connection.
                        attempt += 1;
                        if attempt > self.retries + 4 {
                            return Err(HttpError::new(
                                url,
                                None,
                                format!("truncated at {delivered}/{t} bytes"),
                            ));
                        }
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                    return Ok(delivered);
                }
                Err(e) if e.status == Some(499) => return Err(e),
                Err(e) if e.transient() => {
                    attempt += 1;
                    if attempt > self.retries + 4 {
                        return Err(e);
                    }
                    if got_this_attempt > 0 {
                        attempt = attempt.saturating_sub(1); // progress resets the budget
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

    /// Read one request head; return it lower-cased.
    fn read_head(sock: &mut std::net::TcpStream) -> String {
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match sock.read(&mut b) {
                Ok(1) => head.push(b[0]),
                _ => break,
            }
        }
        String::from_utf8_lossy(&head).to_ascii_lowercase()
    }

    /// A server that sends 10 of 100 bytes and then goes silent with the socket open
    /// (the hang seen on a GitHub release download) must not hang `stream`: the idle
    /// timeout fires and the download resumes with `Range`.
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
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n")
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
        (err.to_string(), got)
    }

    #[test]
    fn resume_rejects_wrong_content_range_start() {
        let (err, got) = serve_truncated_then_partial(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 91\r\nContent-Range: bytes 9-99/100\r\nConnection: close\r\n\r\n", (9u8..100).collect());
        assert!(err.contains("started at byte 9, expected 10"), "{err}");
        assert_eq!(got, (0u8..10).collect::<Vec<_>>());
    }

    #[test]
    fn resume_rejects_changed_total_length() {
        let (err, got) = serve_truncated_then_partial(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 90\r\nContent-Range: bytes 10-99/101\r\nConnection: close\r\n\r\n", (10u8..100).collect());
        assert!(err.contains("total changed from 100 to 101 bytes"), "{err}");
        assert_eq!(got, (0u8..10).collect::<Vec<_>>());
    }

    #[test]
    fn resume_rejects_partial_length_mismatch() {
        let (err, got) = serve_truncated_then_partial(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 89\r\nContent-Range: bytes 10-99/100\r\nConnection: close\r\n\r\n", (10u8..99).collect());
        assert!(
            err.contains("disagrees with Content-Range length 90"),
            "{err}"
        );
        assert_eq!(got, (0u8..10).collect::<Vec<_>>());
    }

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
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                .expect("head 1");
            first.write_all(&served[..10]).expect("part 1");
            first.flush().expect("flush 1");
            // Stall: keep `first` open while the client times out and reconnects.
            let (mut second, _) = listener.accept().expect("accept 2");
            let h2 = read_head(&mut second);
            second
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 90\r\n\
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
}

//! Live probe: buffered HTTPS GET (GitHub API) + streamed download of the first N MiB of a
//! release asset through `StreamReader`. Usage: probe [MiB]
use std::io::Read;
use std::time::Instant;

fn main() {
    let mib: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(32);
    let net = fdop_http::Net::new().expect("net");
    let t = Instant::now();
    let r = net
        .get(
            "https://api.github.com/repos/adsblol/globe_history_2026/releases/tags/v2026.09.08-planes-readsb-mlatonly-0",
            &[("Accept", "application/vnd.github+json")],
        )
        .expect("github api");
    let j = r.json().expect("json");
    let assets = j["assets"].as_array().cloned().unwrap_or_default();
    println!(
        "GET api.github.com -> {} in {:.2}s, {} assets",
        r.status,
        t.elapsed().as_secs_f64(),
        assets.len()
    );
    let url = assets[0]["browser_download_url"]
        .as_str()
        .unwrap()
        .to_string();
    let size = assets[0]["size"].as_u64().unwrap_or(0);
    println!("asset {url} size {size}");
    let t = Instant::now();
    let mut rd = fdop_http::StreamReader::open(vec![url]);
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0usize;
    let mut first512 = Vec::new();
    while total < mib << 20 {
        let n = rd.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        if first512.len() < 512 {
            first512.extend_from_slice(&buf[..n.min(512 - first512.len())]);
        }
        total += n;
    }
    let dt = t.elapsed().as_secs_f64();
    let name: String = first512
        .iter()
        .take(100)
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    println!(
        "streamed {} MiB in {:.2}s = {:.1} MiB/s; first tar member: {name}",
        total >> 20,
        dt,
        (total as f64 / 1048576.0) / dt
    );
}

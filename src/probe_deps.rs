//! Compile-time probe that every franken dependency resolves (removed once modules use them).

pub fn probe() -> String {
    let opts = fsci_fft::FftOptions::default();
    let x = vec![1.0, 0.0, -1.0, 0.0];
    let y = fsci_fft::rfft(&x, &opts).map(|v| v.len()).unwrap_or(0);
    let _ = fsci_opt::LeastSquaresOptions::default();
    let _ = fsci_spatial::KDTree::new(&[vec![0.0, 0.0], vec![1.0, 1.0]]);
    let _ = fsci_signal::medfilt1(&[1.0, 2.0, 3.0], 3);
    let _ = asupersync::http::h1::HttpClient::builder();
    let _ = std::any::type_name::<frankenpandas::DataFrame>();
    let _ = std::any::type_name::<fsqlite::Connection>();
    format!("rfft bins {y}")
}

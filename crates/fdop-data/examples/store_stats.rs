//! Print engine-class / source statistics of a cached `.fdtrk` store.
//! usage: store_stats <file.fdtrk>
use fdop_types::store::TrackStore;
use std::collections::BTreeMap;

fn main() {
    let p = std::env::args().nth(1).expect("path");
    let s = TrackStore::load(std::path::Path::new(&p)).expect("load");
    let mut by: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    for l in &s.legs {
        let e = by.entry(l.engine.as_str().to_string()).or_default();
        e.0 += 1;
        e.1 += l.points.len();
        *types
            .entry(if l.typ.is_empty() {
                "?".into()
            } else {
                l.typ.clone()
            })
            .or_default() += 1;
    }
    println!(
        "{} legs, {} points, meta {:?}",
        s.legs.len(),
        s.n_points(),
        s.meta.stats
    );
    for (k, (n, pts)) in by {
        println!("  {k:<10} {n:6} legs {pts:9} points");
    }
    let mut tv: Vec<_> = types.into_iter().collect();
    tv.sort_by_key(|t| std::cmp::Reverse(t.1));
    println!("top types: {:?}", &tv[..tv.len().min(25)]);
}

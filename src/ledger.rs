//! Run ledger in FrankenSQLite (`<cache>/runs.sqlite`): one row per `fdop locate` run, so past
//! answers can be listed and compared (`fdop runs`).

use asupersync::runtime::RuntimeBuilder;
use fsqlite::{Connection, SqliteValue};
use std::path::Path;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RunRow {
    pub created_utc: String,
    pub input_file: String,
    pub dates: String,
    pub region: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub radius_m: Option<f64>,
    pub confidence: String,
    pub headline: String,
    pub run_dir: String,
    pub total_s: f64,
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY,
    created_utc TEXT NOT NULL,
    input_file TEXT NOT NULL,
    dates TEXT NOT NULL,
    region TEXT NOT NULL,
    lat REAL, lon REAL, radius_m REAL,
    confidence TEXT NOT NULL,
    headline TEXT NOT NULL,
    run_dir TEXT NOT NULL,
    total_s REAL NOT NULL
);";

fn opt(v: Option<f64>) -> SqliteValue {
    match v {
        Some(x) if x.is_finite() => SqliteValue::Float(x),
        _ => SqliteValue::Null,
    }
}

fn run_async<T>(f: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    let rt = RuntimeBuilder::current_thread()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(f)
}

pub fn record(db: &Path, r: &RunRow) -> Result<(), String> {
    if let Some(d) = db.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let path = db.display().to_string();
    let r = r.clone();
    run_async(async move {
        let conn = Connection::open(path).await.map_err(|e| e.to_string())?;
        conn.execute(SCHEMA).await.map_err(|e| e.to_string())?;
        {
            let st = conn
                .prepare(
                    "INSERT INTO runs (created_utc, input_file, dates, region, lat, lon, radius_m, confidence, headline, run_dir, total_s) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11);",
                )
                .await
                .map_err(|e| e.to_string())?;
            st.execute_with_params(&[
                SqliteValue::from(r.created_utc.as_str()),
                SqliteValue::from(r.input_file.as_str()),
                SqliteValue::from(r.dates.as_str()),
                SqliteValue::from(r.region.as_str()),
                opt(r.lat),
                opt(r.lon),
                opt(r.radius_m),
                SqliteValue::from(r.confidence.as_str()),
                SqliteValue::from(r.headline.as_str()),
                SqliteValue::from(r.run_dir.as_str()),
                SqliteValue::Float(r.total_s),
            ])
            .await
            .map_err(|e| e.to_string())?;
        }
        conn.close().await.map_err(|e| e.to_string())
    })
}

fn as_f(v: Option<&SqliteValue>) -> Option<f64> {
    match v {
        Some(SqliteValue::Float(x)) => Some(*x),
        Some(SqliteValue::Integer(i)) => Some(*i as f64),
        _ => None,
    }
}

fn as_s(v: Option<&SqliteValue>) -> String {
    match v {
        Some(SqliteValue::Text(s)) => s.to_string(),
        Some(SqliteValue::Null) | None => String::new(),
        Some(o) => format!("{o:?}"),
    }
}

pub fn list(db: &Path, limit: usize) -> Result<Vec<RunRow>, String> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    let path = db.display().to_string();
    run_async(async move {
        let conn = Connection::open(path).await.map_err(|e| e.to_string())?;
        conn.execute(SCHEMA).await.map_err(|e| e.to_string())?;
        let rows = conn
            .query(&format!(
                "SELECT created_utc, input_file, dates, region, lat, lon, radius_m, confidence, headline, run_dir, total_s FROM runs ORDER BY id DESC LIMIT {limit};"
            ))
            .await
            .map_err(|e| e.to_string())?;
        let out = rows
            .iter()
            .map(|r| RunRow {
                created_utc: as_s(r.get(0)),
                input_file: as_s(r.get(1)),
                dates: as_s(r.get(2)),
                region: as_s(r.get(3)),
                lat: as_f(r.get(4)),
                lon: as_f(r.get(5)),
                radius_m: as_f(r.get(6)),
                confidence: as_s(r.get(7)),
                headline: as_s(r.get(8)),
                run_dir: as_s(r.get(9)),
                total_s: as_f(r.get(10)).unwrap_or(0.0),
            })
            .collect();
        conn.close().await.map_err(|e| e.to_string())?;
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("fdop_ledger_{}", std::process::id()));
        let db = dir.join("runs.sqlite");
        let r = RunRow {
            created_utc: "2026-11-03T00:00:00Z".into(),
            input_file: "a.m4a".into(),
            dates: "2026-09-08".into(),
            region: "New York".into(),
            lat: Some(40.8765),
            lon: Some(-73.525),
            radius_m: Some(120.0),
            confidence: "high".into(),
            headline: "Oyster Bay, NY 11771".into(),
            run_dir: "runs/x".into(),
            total_s: 12.5,
        };
        record(&db, &r).unwrap();
        let got = list(&db, 5).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].headline, r.headline);
        assert_eq!(got[0].lat, r.lat);
    }
}

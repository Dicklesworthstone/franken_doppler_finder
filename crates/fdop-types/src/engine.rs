//! Engine classes from ICAO Doc 8643 aircraft type designators.
//!
//! The table (`data/icao_engine_class.tsv`, designator TAB class) is derived from the ICAO
//! Doc 8643 type list: J jet, T turboprop/turboshaft, P piston, E electric, M mixed, O other.
//! Policy (fail-open): drop known jets; KEEP unknown designators, because dropping unknowns
//! would silently remove props whose type is missing from the feed.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineClass {
    Piston,
    Turboprop,
    Electric,
    Jet,
    Mixed,
    Other,
    Unknown,
}

impl EngineClass {
    pub fn code(self) -> u8 {
        match self {
            Self::Piston => b'P',
            Self::Turboprop => b'T',
            Self::Electric => b'E',
            Self::Jet => b'J',
            Self::Mixed => b'M',
            Self::Other => b'O',
            Self::Unknown => b'U',
        }
    }
    pub fn from_code(c: u8) -> Self {
        match c {
            b'P' => Self::Piston,
            b'T' => Self::Turboprop,
            b'E' => Self::Electric,
            b'J' => Self::Jet,
            b'M' => Self::Mixed,
            b'O' => Self::Other,
            _ => Self::Unknown,
        }
    }
    /// Is this aircraft a candidate tonal (propeller/rotor) source? Jets are not.
    /// Mixed-type designators (some variants jet, some prop) are KEPT: fail-open.
    pub fn tonal(self) -> bool {
        !matches!(self, Self::Jet)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Piston => "piston",
            Self::Turboprop => "turboprop",
            Self::Electric => "electric",
            Self::Jet => "jet",
            Self::Mixed => "mixed",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }
}

const TABLE: &str = include_str!("../data/icao_engine_class.tsv");

fn table() -> &'static HashMap<&'static str, EngineClass> {
    static T: OnceLock<HashMap<&'static str, EngineClass>> = OnceLock::new();
    T.get_or_init(|| {
        TABLE
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                let d = it.next()?.trim();
                let c = it.next()?.trim().bytes().next()?;
                (!d.is_empty()).then_some((d, EngineClass::from_code(c)))
            })
            .collect()
    })
}

/// Engine class for an ICAO type designator (case-insensitive). Unknown or empty -> Unknown.
pub fn engine_class(designator: &str) -> EngineClass {
    let d = designator.trim().to_ascii_uppercase();
    if d.is_empty() {
        return EngineClass::Unknown;
    }
    table()
        .get(d.as_str())
        .copied()
        .unwrap_or(EngineClass::Unknown)
}

pub fn table_len() -> usize {
    table().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_designators() {
        assert_eq!(engine_class("PC12"), EngineClass::Turboprop);
        assert_eq!(engine_class("b350"), EngineClass::Turboprop);
        assert_eq!(engine_class("C172"), EngineClass::Piston);
        assert_eq!(engine_class("B738"), EngineClass::Jet);
        assert_eq!(engine_class(""), EngineClass::Unknown);
        assert_eq!(engine_class("ZZZZ9"), EngineClass::Unknown);
        assert!(table_len() > 2000);
        assert!(
            !EngineClass::Jet.tonal() && EngineClass::Unknown.tonal() && EngineClass::Mixed.tonal()
        );
    }
}

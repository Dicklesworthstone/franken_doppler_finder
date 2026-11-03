//! franken_doppler_finder (`fdop`): locate an audio recording from the Doppler curves of
//! propeller aircraft flying past, matched against archived ADS-B tracks.
//!
//! The pipeline (`fdop locate`) needs only a media file, a date range and a coarse US region:
//! media probe -> survey spectrogram -> automatic flyover detection -> per-event adaptive STFT,
//! model-guided ridge trace and retarded-time fit -> ADS-B download and ingest -> flight-centric
//! hypotheses and spatiotemporal pair join -> exact fine funnel (gates, joint clock-locked trace
//! scoring, confirmers, unheard close passes) -> 25 m refinement and chi-square radius ->
//! reverse geocode -> report and map.

pub mod cli;
pub mod doctor;
pub mod ledger;
pub mod pipeline;
pub mod selftest;

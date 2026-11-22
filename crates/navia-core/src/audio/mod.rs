//! The audio engine: decode, resample, crossfade, process, and buffer for the output.
//!
//! With no processing, resampling or crossfade and a source of at most 24 bits, samples pass
//! through unchanged as f32, which holds a 24-bit integer exactly.

pub mod airplay;
pub mod capture;
pub mod decode;
pub mod device_watch;
pub mod edges;
pub mod engine;
pub mod live;
pub mod mp4;
#[cfg(feature = "opus")]
mod opus;
pub mod output;
pub mod player;
pub mod processing;
#[cfg(target_os = "linux")]
pub mod pulse;
pub mod render;
pub mod resample;
pub mod stream;
pub mod transition;

pub use decode::{Decoder, GaplessSource, TrackSource};
pub use engine::{EngineConfig, EngineControl, EngineEvent, EngineOutput, EngineParts, OutputMode, TrackFeed, TrackOpener, TrackRequest, spawn, spawn_feed};
pub use processing::{EqBand, EqSettings, Equalizer, FilterShape, ProcessingChain, Processor};

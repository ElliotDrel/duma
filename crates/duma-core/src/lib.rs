//! Duma engine.
//!
//! - [`settings`]: user configuration (models, hotkeys, presets, capture options).
//! - [`store`]: threads and their versions in SQLite.
//! - [`log`]: the append-only JSONL event log, the source of truth for analysis and training data.
//! - [`prompts`]: model instructions and request builders.
//! - [`engine`]: the hotkey-driven pipeline (dictate, edit, preset, undo) and the post-insert edit watch.

pub mod engine;
pub mod log;
pub mod paths;
pub mod prompts;
pub mod secrets;
pub mod settings;
pub mod store;
mod text;
mod watch;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

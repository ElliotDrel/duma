//! Append-only JSONL event log: `logs/duma-YYYY-MM-DD.jsonl`, one JSON object per line.
//!
//! This is the source of truth for analysis and training data; SQLite is an index for the UI and every
//! store write is mirrored here, so the index can be rebuilt. Writes go through a background thread so
//! logging never delays the hot path. Every line carries `ts`, `event`, `schema`, `channel`, `app_version`, `pid`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};

use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::paths::Channel;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone)]
pub struct EventLog {
    tx: Sender<Value>,
    dir: PathBuf,
}

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_local()
        .unwrap_or_else(|_| OffsetDateTime::now_utc())
        .format(&Rfc3339)
        .unwrap_or_default()
}

impl EventLog {
    pub fn open(dir: PathBuf, channel: Channel) -> Self {
        let (tx, rx) = mpsc::channel::<Value>();
        let thread_dir = dir.clone();
        std::thread::Builder::new()
            .name("duma-log".into())
            .spawn(move || {
                for line in rx {
                    let date = line["ts"]
                        .as_str()
                        .and_then(|t| t.get(..10))
                        .unwrap_or("unknown")
                        .to_string();
                    let path = thread_dir.join(format!("duma-{date}.jsonl"));
                    let written = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .and_then(|mut file| writeln!(file, "{line}"));
                    if let Err(error) = written {
                        log::error!("writing {}: {error}", path.display());
                    }
                }
            })
            .expect("log thread");
        let log = Self { tx, dir };
        log.event("log_opened", json!({ "channel_name": channel.as_str() }));
        log
    }

    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    /// Writes one event. `data` must be a JSON object; its fields are merged into the line.
    pub fn event(&self, event: &str, data: Value) {
        let mut line = Map::new();
        line.insert("ts".into(), json!(now_rfc3339()));
        line.insert("event".into(), json!(event));
        line.insert("schema".into(), json!(SCHEMA_VERSION));
        line.insert("channel".into(), json!(Channel::current().as_str()));
        line.insert("app_version".into(), json!(crate::APP_VERSION));
        line.insert("pid".into(), json!(std::process::id()));
        if let Value::Object(fields) = data {
            line.extend(fields);
        } else if !data.is_null() {
            line.insert("data".into(), data);
        }
        let _ = self.tx.send(Value::Object(line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_one_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::open(dir.path().to_path_buf(), Channel::Dev);
        log.event("test", json!({ "a": 1 }));
        drop(log);
        std::thread::sleep(std::time::Duration::from_millis(200));
        let file = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["event"], "test");
        assert_eq!(lines[1]["a"], 1);
        assert_eq!(lines[1]["schema"], SCHEMA_VERSION);
    }
}

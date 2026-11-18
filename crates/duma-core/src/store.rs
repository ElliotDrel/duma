//! Threads and versions in SQLite: the index behind the threads UI and thread resolution.
//!
//! A thread is one piece of text Duma has worked on. Versions form a tree through `parent_id`; the
//! thread's `head_id` is the current version, so undo is a head move rather than a new version.
//! Every write here is mirrored to the JSONL log by the engine.

use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::log::now_rfc3339;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Version {
    pub id: String,
    pub thread_id: String,
    pub parent_id: Option<String>,
    pub created_at: String,
    /// `dictation`, `original` (text Duma did not write), `instruction`, `preset`, `manual_edit`.
    pub kind: String,
    pub text: String,
    /// What produced it: the raw transcript, the spoken instruction or the preset name.
    pub source: Option<String>,
    pub run_id: Option<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThreadSummary {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub head_id: Option<String>,
    pub head_text: Option<String>,
    pub version_count: i64,
}

pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Self> {
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS threads (
                 id TEXT PRIMARY KEY,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 app_name TEXT,
                 app_id TEXT,
                 window_title TEXT,
                 head_id TEXT
             );
             CREATE TABLE IF NOT EXISTS versions (
                 id TEXT PRIMARY KEY,
                 thread_id TEXT NOT NULL REFERENCES threads(id),
                 parent_id TEXT,
                 created_at TEXT NOT NULL,
                 kind TEXT NOT NULL,
                 text TEXT NOT NULL,
                 source TEXT,
                 run_id TEXT,
                 model TEXT
             );
             CREATE INDEX IF NOT EXISTS versions_thread ON versions(thread_id, created_at);
             CREATE INDEX IF NOT EXISTS versions_text ON versions(text);",
        )?;
        Ok(Self { db })
    }

    pub fn create_thread(
        &self,
        app_name: Option<&str>,
        app_id: Option<&str>,
        window_title: Option<&str>,
    ) -> Result<String> {
        let id = uuid::Uuid::now_v7().to_string();
        let now = now_rfc3339();
        self.db.execute(
            "INSERT INTO threads (id, created_at, updated_at, app_name, app_id, window_title) VALUES (?1, ?2, ?2, ?3, ?4, ?5)",
            params![id, now, app_name, app_id, window_title],
        )?;
        Ok(id)
    }

    /// Adds a version as the new head of its thread.
    pub fn add_version(&self, version: NewVersion<'_>) -> Result<Version> {
        let created = Version {
            id: uuid::Uuid::now_v7().to_string(),
            thread_id: version.thread_id.to_string(),
            parent_id: version.parent_id.map(str::to_string),
            created_at: now_rfc3339(),
            kind: version.kind.to_string(),
            text: version.text.to_string(),
            source: version.source.map(str::to_string),
            run_id: version.run_id.map(str::to_string),
            model: version.model.map(str::to_string),
        };
        self.db.execute(
            "INSERT INTO versions (id, thread_id, parent_id, created_at, kind, text, source, run_id, model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                created.id,
                created.thread_id,
                created.parent_id,
                created.created_at,
                created.kind,
                created.text,
                created.source,
                created.run_id,
                created.model
            ],
        )?;
        self.set_head(&created.thread_id, &created.id)?;
        Ok(created)
    }

    pub fn set_head(&self, thread_id: &str, version_id: &str) -> Result<()> {
        self.db.execute(
            "UPDATE threads SET head_id = ?2, updated_at = ?3 WHERE id = ?1",
            params![thread_id, version_id, now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn version(&self, id: &str) -> Result<Option<Version>> {
        Ok(self
            .db
            .query_row(
                "SELECT id, thread_id, parent_id, created_at, kind, text, source, run_id, model FROM versions WHERE id = ?1",
                params![id],
                row_to_version,
            )
            .optional()?)
    }

    pub fn head(&self, thread_id: &str) -> Result<Option<Version>> {
        let head: Option<String> = self
            .db
            .query_row(
                "SELECT head_id FROM threads WHERE id = ?1",
                params![thread_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        match head {
            Some(id) => self.version(&id),
            None => Ok(None),
        }
    }

    /// The head and its ancestors, oldest first, at most `limit`.
    pub fn lineage(&self, thread_id: &str, limit: usize) -> Result<Vec<Version>> {
        let mut chain = Vec::new();
        let mut next = self.head(thread_id)?;
        while let Some(version) = next {
            if chain.len() >= limit {
                break;
            }
            next = match &version.parent_id {
                Some(parent) => self.version(parent)?,
                None => None,
            };
            chain.push(version);
        }
        chain.reverse();
        Ok(chain)
    }

    pub fn versions(&self, thread_id: &str) -> Result<Vec<Version>> {
        let mut statement = self.db.prepare(
            "SELECT id, thread_id, parent_id, created_at, kind, text, source, run_id, model
             FROM versions WHERE thread_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(params![thread_id], row_to_version)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The most recent version whose text equals `text`, for reopening a thread from a selection.
    pub fn find_version_by_text(&self, text: &str) -> Result<Option<Version>> {
        Ok(self
            .db
            .query_row(
                "SELECT id, thread_id, parent_id, created_at, kind, text, source, run_id, model
                 FROM versions WHERE text = ?1 ORDER BY created_at DESC, id DESC LIMIT 1",
                params![text],
                row_to_version,
            )
            .optional()?)
    }

    pub fn threads(&self, limit: usize, offset: usize) -> Result<Vec<ThreadSummary>> {
        let mut statement = self.db.prepare(
            "SELECT t.id, t.created_at, t.updated_at, t.app_name, t.window_title, t.head_id, v.text,
                    (SELECT COUNT(*) FROM versions WHERE thread_id = t.id)
             FROM threads t LEFT JOIN versions v ON v.id = t.head_id
             ORDER BY t.updated_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![limit as i64, offset as i64], |r| {
            Ok(ThreadSummary {
                id: r.get(0)?,
                created_at: r.get(1)?,
                updated_at: r.get(2)?,
                app_name: r.get(3)?,
                window_title: r.get(4)?,
                head_id: r.get(5)?,
                head_text: r.get(6)?,
                version_count: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

pub struct NewVersion<'a> {
    pub thread_id: &'a str,
    pub parent_id: Option<&'a str>,
    pub kind: &'a str,
    pub text: &'a str,
    pub source: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub model: Option<&'a str>,
}

fn row_to_version(r: &rusqlite::Row<'_>) -> rusqlite::Result<Version> {
    Ok(Version {
        id: r.get(0)?,
        thread_id: r.get(1)?,
        parent_id: r.get(2)?,
        created_at: r.get(3)?,
        kind: r.get(4)?,
        text: r.get(5)?,
        source: r.get(6)?,
        run_id: r.get(7)?,
        model: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version<'a>(
        thread: &'a str,
        parent: Option<&'a str>,
        kind: &'a str,
        text: &'a str,
    ) -> NewVersion<'a> {
        NewVersion {
            thread_id: thread,
            parent_id: parent,
            kind,
            text,
            source: None,
            run_id: None,
            model: None,
        }
    }

    #[test]
    fn head_lineage_and_undo() {
        let store = Store::in_memory().unwrap();
        let thread = store.create_thread(Some("Slack"), None, None).unwrap();
        let first = store
            .add_version(version(&thread, None, "dictation", "hello there"))
            .unwrap();
        let second = store
            .add_version(version(
                &thread,
                Some(&first.id),
                "instruction",
                "Hello there!",
            ))
            .unwrap();
        assert_eq!(store.head(&thread).unwrap().unwrap().id, second.id);
        let lineage = store.lineage(&thread, 10).unwrap();
        assert_eq!(
            lineage.iter().map(|v| v.text.as_str()).collect::<Vec<_>>(),
            ["hello there", "Hello there!"]
        );

        store.set_head(&thread, &first.id).unwrap();
        assert_eq!(store.head(&thread).unwrap().unwrap().text, "hello there");
        assert_eq!(
            store
                .find_version_by_text("Hello there!")
                .unwrap()
                .unwrap()
                .id,
            second.id
        );
        let threads = store.threads(10, 0).unwrap();
        assert_eq!(threads[0].version_count, 2);
        assert_eq!(threads[0].head_text.as_deref(), Some("hello there"));
    }
}

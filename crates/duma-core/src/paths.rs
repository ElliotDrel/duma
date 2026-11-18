//! Where Duma keeps its data. Prod and Dev channels never share a folder, so a dev build can run
//! beside the installed app (the Universal Spell Check pattern).
//!
//! - Windows: `%LOCALAPPDATA%\Duma` / `Duma.Dev`
//! - macOS: `~/Library/Application Support/Duma` / `Duma.Dev`

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Prod,
    Dev,
}

impl Channel {
    pub fn current() -> Self {
        if cfg!(debug_assertions) || std::env::var_os("DUMA_DEV").is_some() {
            Self::Dev
        } else {
            Self::Prod
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prod => "prod",
            Self::Dev => "dev",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn for_channel(channel: Channel) -> Self {
        if let Some(root) = std::env::var_os("DUMA_DATA_DIR") {
            return Self { root: root.into() };
        }
        let base = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
        let name = match channel {
            Channel::Prod => "Duma",
            Channel::Dev => "Duma.Dev",
        };
        Self {
            root: base.join(name),
        }
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    pub fn database(&self) -> PathBuf {
        self.root.join("duma.sqlite")
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn audio(&self) -> PathBuf {
        self.root.join("audio")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.logs())?;
        std::fs::create_dir_all(self.audio())
    }
}

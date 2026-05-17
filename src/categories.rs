use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// a named category mapping (qBT-style). torrents tagged with a category
/// inherit the category's save_path when added without an explicit override.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub save_path: String,
    /// tags automatically applied to torrents in this category
    #[serde(default)]
    pub add_tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Categories {
    #[serde(default, flatten)]
    pub entries: BTreeMap<String, Category>,
}

impl Categories {
    pub fn load() -> Result<Self> {
        let path = Config::categories_path()?;
        if (!path.exists()) { return Ok(Categories::default()); }
        let content = std::fs::read_to_string(&path).context("read categories")?;
        let parsed: Categories = toml::from_str(&content).context("parse categories")?;
        Ok(parsed)
    }

    pub fn save(&self) -> Result<()> {
        let path = Config::categories_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("create config dir")?;
        }
        let content = toml::to_string_pretty(self).context("serialize categories")?;
        std::fs::write(path, content).context("write categories")
    }

    pub fn get(&self, name: &str) -> Option<&Category> {
        self.entries.get(name)
    }
}

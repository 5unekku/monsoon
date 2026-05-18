use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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

/// auto-tagging rule. matchers are AND-combined; an empty matcher set
/// means "always match". this is intentionally simple (no regex) so the
/// implementation has no dependencies and rule files stay grep-able.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagRule {
    pub name: String,
    /// torrents whose total_wanted is at least this many bytes match
    #[serde(default)]
    pub size_min: Option<i64>,
    /// torrents whose total_wanted is at most this many bytes match
    #[serde(default)]
    pub size_max: Option<i64>,
    /// case-insensitive substring matched against torrent name
    #[serde(default)]
    pub name_contains: Option<String>,
    /// case-insensitive substring matched against any tracker URL
    #[serde(default)]
    pub tracker_contains: Option<String>,
    /// tags applied when this rule matches
    pub add_tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagRules {
    #[serde(default)]
    pub rules: Vec<TagRule>,
}

impl TagRules {
    pub fn load() -> Result<Self> {
        let path = Config::tag_rules_path()?;
        if (!path.exists()) { return Ok(TagRules::default()); }
        let content = std::fs::read_to_string(&path).context("read tag rules")?;
        let parsed: TagRules = toml::from_str(&content).context("parse tag rules")?;
        Ok(parsed)
    }

    /// evaluate every rule against a torrent's properties and return the
    /// union of add_tags from matching rules.
    pub fn evaluate(
        &self,
        torrent_name: &str,
        total_size: i64,
        tracker_urls: &[String],
    ) -> BTreeSet<String> {
        let mut tags = BTreeSet::new();
        for rule in &self.rules {
            if let Some(threshold) = rule.size_min {
                if (total_size < threshold) { continue; }
            }
            if let Some(threshold) = rule.size_max {
                if (total_size > threshold) { continue; }
            }
            if let Some(needle) = &rule.name_contains {
                if (!torrent_name.to_lowercase().contains(&needle.to_lowercase())) { continue; }
            }
            if let Some(needle) = &rule.tracker_contains {
                let lower_needle = needle.to_lowercase();
                let any_match = tracker_urls.iter()
                    .any(|url| url.to_lowercase().contains(&lower_needle));
                if (!any_match) { continue; }
            }
            for tag in &rule.add_tags { tags.insert(tag.clone()); }
        }
        tags
    }
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

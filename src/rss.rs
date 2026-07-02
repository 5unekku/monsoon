use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RssFeed {
    pub url: String,
    /// regex matched against item titles. empty = accept all.
    #[serde(default)]
    pub filter: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub save_path: Option<String>,
    #[serde(default = "default_poll_minutes")]
    pub poll_interval_minutes: u64,
    #[serde(default)]
    pub start_paused: bool,
}

fn default_poll_minutes() -> u64 { 30 }

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RssFeeds {
    #[serde(default)]
    pub feeds: Vec<RssFeed>,
}

impl RssFeeds {
    pub fn load() -> Result<Self> {
        let path = crate::config::Config::feeds_path()?;
        if (!path.exists()) { return Ok(Self::default()); }
        let content = std::fs::read_to_string(&path).context("read feeds.toml")?;
        toml::from_str(&content).context("parse feeds.toml")
    }

    pub fn save(&self) -> Result<()> {
        let path = crate::config::Config::feeds_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let content = toml::to_string_pretty(self).context("serialize feeds")?;
        std::fs::write(path, content).context("write feeds.toml")
    }
}

/// persisted set of guids/links already submitted to the daemon.
/// prevents re-adding on restart or after a re-poll.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RssSeen {
    seen: HashSet<String>,
}

impl RssSeen {
    pub fn load() -> Result<Self> {
        let path = crate::config::Config::rss_seen_path()?;
        if (!path.exists()) { return Ok(Self::default()); }
        let content = std::fs::read_to_string(&path).context("read rss_seen.json")?;
        serde_json::from_str(&content).context("parse rss_seen.json")
    }

    pub fn save(&self) -> Result<()> {
        let path = crate::config::Config::rss_seen_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let content = serde_json::to_string(self).context("serialize rss_seen")?;
        std::fs::write(path, content).context("write rss_seen.json")
    }

    pub fn contains(&self, key: &str) -> bool { self.seen.contains(key) }
    pub fn insert(&mut self, key: String) { self.seen.insert(key); }
}

/// fetch and parse one feed. returns `(seen_key, uri)` pairs for items that
/// are new (not in `seen`) and whose title matches the filter regex.
/// uri is either a magnet URI or a .torrent download URL.
pub fn poll_feed(feed: &RssFeed, seen: &RssSeen, fetch_auth: &crate::sources::FetchAuth) -> Result<Vec<(String, String)>> {
    let xml = crate::sources::fetch_to_string(&feed.url, fetch_auth)
        .with_context(|| format!("fetch feed {}", feed.url))?;

    let filter = if (feed.filter.trim().is_empty()) {
        None
    } else {
        Some(regex::Regex::new(&feed.filter)
            .with_context(|| format!("compile filter regex: {}", feed.filter))?)
    };

    let items = parse_feed(&xml);
    let mut results = Vec::new();

    for item in items {
        let key = item.guid.as_deref()
            .or(item.uri.as_deref())
            .unwrap_or("")
            .to_string();
        if (key.is_empty() || seen.contains(&key)) { continue; }

        let Some(uri) = item.uri else { continue; };
        if (!looks_like_torrent(&uri)) { continue; }

        if let Some(re) = &filter {
            if (!re.is_match(&item.title)) { continue; }
        }

        results.push((key, uri));
    }

    Ok(results)
}

/// true for magnet URIs and URLs that are clearly pointing at a .torrent file.
/// page-level HTML links (no .torrent in the path) are excluded.
fn looks_like_torrent(uri: &str) -> bool {
    if (uri.starts_with("magnet:")) { return true; }
    // strip query string / fragment before checking for .torrent so
    // URLs like "https://host/dl/file.torrent?passkey=abc" still match
    let path_part = uri.find('?').or_else(|| uri.find('#'))
        .map(|i| &uri[..i])
        .unwrap_or(uri);
    path_part.ends_with(".torrent")
}

struct FeedItem {
    title: String,
    uri: Option<String>,
    guid: Option<String>,
}

/// SAX-style RSS 2.0 / Atom parser. handles:
/// - RSS 2.0: <enclosure url>, <link> text, <guid>, <torrent:magnetURI>
/// - Atom:    <link href>, <id>
///   namespaced local names are matched by local part only (strips prefix).
fn parse_feed(xml: &str) -> Vec<FeedItem> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut items: Vec<FeedItem> = Vec::new();
    let mut current: Option<FeedItem> = None;

    #[derive(PartialEq)]
    enum Cap { None, Title, Link, Guid, Magnet }
    let mut cap = Cap::None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref element)) => {
                let name = local_name(element.name().as_ref());
                match name.as_str() {
                    "item" | "entry" => {
                        current = Some(FeedItem { title: String::new(), uri: None, guid: None });
                    }
                    "title" if current.is_some() => cap = Cap::Title,
                    "link" if current.is_some() => {
                        // Atom uses href attribute; RSS 2.0 uses text content
                        let href = attr_value(element.attributes(), b"href");
                        if let Some(href) = href {
                            if let Some(item) = current.as_mut() {
                                if (item.uri.is_none()) { item.uri = Some(href); }
                            }
                        } else {
                            cap = Cap::Link;
                        }
                    }
                    "guid" | "id" if current.is_some() => cap = Cap::Guid,
                    "magneturi" if current.is_some() => cap = Cap::Magnet,
                    "enclosure" if current.is_some() => {
                        if let Some(url) = attr_value(element.attributes(), b"url") {
                            if let Some(item) = current.as_mut() {
                                // enclosure takes precedence over <link>
                                item.uri = Some(url);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref element)) => {
                let name = local_name(element.name().as_ref());
                if (name == "link") {
                    if let Some(item) = current.as_mut() {
                        if let Some(href) = attr_value(element.attributes(), b"href") {
                            if (item.uri.is_none()) { item.uri = Some(href); }
                        }
                    }
                } else if (name == "enclosure") {
                    if let Some(item) = current.as_mut() {
                        if let Some(url) = attr_value(element.attributes(), b"url") {
                            item.uri = Some(url);
                        }
                    }
                }
            }
            Ok(Event::End(ref element)) => {
                let name = local_name(element.name().as_ref());
                match name.as_str() {
                    "item" | "entry" => {
                        if let Some(item) = current.take() { items.push(item); }
                    }
                    _ => cap = Cap::None,
                }
            }
            Ok(Event::Text(ref text)) => {
                if (current.is_some()) {
                    let value = text.unescape().unwrap_or_default().into_owned();
                    if let Some(item) = current.as_mut() {
                        match cap {
                            Cap::Title => item.title.push_str(&value),
                            Cap::Link if item.uri.is_none() => item.uri = Some(value),
                            Cap::Guid => item.guid = Some(value),
                            Cap::Magnet => item.uri = Some(value),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::CData(ref cdata)) => {
                if (current.is_some()) {
                    let value = String::from_utf8_lossy(cdata.as_ref()).into_owned();
                    if let Some(item) = current.as_mut() {
                        match cap {
                            Cap::Title => item.title.push_str(&value),
                            Cap::Link if item.uri.is_none() => item.uri = Some(value),
                            Cap::Guid => item.guid = Some(value),
                            Cap::Magnet => item.uri = Some(value),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => { tracing::warn!("rss xml parse error: {}", error); break; }
            _ => {}
        }
        buf.clear();
    }

    items
}

fn local_name(raw: &[u8]) -> String {
    let s = std::str::from_utf8(raw).unwrap_or("");
    // strip namespace prefix if present (e.g. "torrent:magnetURI" → "magneturi")
    let local = s.rfind(':').map(|i| &s[i + 1..]).unwrap_or(s);
    local.to_ascii_lowercase()
}

fn attr_value(
    attrs: quick_xml::events::attributes::Attributes,
    key: &[u8],
) -> Option<String> {
    attrs.filter_map(|a| a.ok())
        .find(|a| a.key.local_name().as_ref() == key)
        .and_then(|a| String::from_utf8(a.value.to_vec()).ok())
}

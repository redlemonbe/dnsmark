use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Context;

use crate::dns::parse_record_type;
use super::{QueryEntry, QuerySource};

pub struct FileQuerySource {
    entries: Vec<QueryEntry>,
    index: AtomicUsize,
}

impl FileQuerySource {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("read query file {}", path.display()))?;
        let entries: Vec<QueryEntry> = content
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|line| {
                let mut parts = line.split_whitespace();
                let name = parts.next().unwrap_or("example.com").to_string();
                let qtype_str = parts.next().unwrap_or("A");
                QueryEntry { name, qtype: parse_record_type(qtype_str) }
            })
            .collect();
        if entries.is_empty() {
            anyhow::bail!("query file {} contains no valid entries", path.display());
        }
        Ok(Self { entries, index: AtomicUsize::new(0) })
    }
}

impl QuerySource for FileQuerySource {
    fn next(&self) -> QueryEntry {
        let len = self.entries.len();
        let idx = self.index.fetch_add(1, Ordering::Relaxed) % len;
        self.entries[idx].clone()
    }

    fn all_wire_pairs(&self) -> Vec<(String, u16)> {
        self.entries.iter().map(|e| (e.name.clone(), e.qtype)).collect()
    }
}

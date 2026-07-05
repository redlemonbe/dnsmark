use std::sync::atomic::{AtomicUsize, Ordering};
use crate::dns::parse_record_type;
use super::{QueryEntry, QuerySource};

static CORPUS: &str = include_str!("../../assets/builtin_corpus.txt");

pub struct BuiltinQuerySource {
    entries: Vec<QueryEntry>,
    index:   AtomicUsize,
}

impl BuiltinQuerySource {
    pub fn new() -> Self {
        let entries: Vec<QueryEntry> = CORPUS
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|line| {
                let mut parts = line.split_whitespace();
                let name      = parts.next().unwrap_or("example.com").to_string();
                let qtype_str = parts.next().unwrap_or("A");
                QueryEntry { name, qtype: parse_record_type(qtype_str) }
            })
            .collect();
        Self { entries, index: AtomicUsize::new(0) }
    }
}

impl QuerySource for BuiltinQuerySource {
    fn next(&self) -> QueryEntry {
        let idx = self.index.fetch_add(1, Ordering::Relaxed) % self.entries.len();
        self.entries[idx].clone()
    }
    fn all_wire_pairs(&self) -> Vec<(String, u16)> {
        self.entries.iter().map(|e| (e.name.clone(), e.qtype)).collect()
    }
}

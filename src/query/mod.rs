pub mod builtin;
pub mod file;
pub mod random;
pub mod wire;

pub use wire::{WireQueryPool, MAX_QUERY};

#[derive(Debug, Clone)]
pub struct QueryEntry {
    pub name: String,
    pub qtype: u16,
}

pub trait QuerySource: Send + Sync {
    fn next(&self) -> QueryEntry;

    /// Pre-build all wire-format templates for the zero-allocation hot path.
    /// FileQuerySource returns its full dataset; RandomQuerySource samples 4096 entries.
    fn all_wire_pairs(&self) -> Vec<(String, u16)>;
}

pub mod file;
pub mod random;

#[derive(Debug, Clone)]
pub struct QueryEntry {
    pub name: String,
    pub qtype: u16,
}

pub trait QuerySource: Send + Sync {
    fn next(&self) -> QueryEntry;
}

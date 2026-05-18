use super::{QueryEntry, QuerySource};

pub struct RandomQuerySource {
    base_domain: String,
}

impl RandomQuerySource {
    pub fn new(base_domain: &str) -> Self {
        let base = base_domain.trim_end_matches('.').to_string();
        Self { base_domain: base }
    }
}

impl QuerySource for RandomQuerySource {
    fn next(&self) -> QueryEntry {
        let a: u64 = rand::random();
        let b: u64 = rand::random();
        let name = format!("{:016x}{:016x}.{}", a, b, self.base_domain);
        QueryEntry { name, qtype: 1 } // A record
    }
}

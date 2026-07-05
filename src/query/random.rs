use super::{QueryEntry, QuerySource};

pub struct RandomQuerySource {
    base_domain: String,
    qtype: u16,
}

impl RandomQuerySource {
    pub fn new(base_domain: &str, qtype: u16) -> Self {
        let base = base_domain.trim_end_matches('.').to_string();
        Self { base_domain: base, qtype }
    }
}

impl QuerySource for RandomQuerySource {
    fn next(&self) -> QueryEntry {
        let id: u128 = rand::random();
        let name = format!("{:032x}.{}", id, self.base_domain);
        QueryEntry { name, qtype: self.qtype }
    }

    fn all_wire_pairs(&self) -> Vec<(String, u16)> {
        // Sample 4096 random names — sufficient pool for benchmark purposes.
        (0..4096usize)
            .map(|_| {
                let id: u128 = rand::random();
                let name = format!("{:032x}.{}", id, self.base_domain);
                (name, self.qtype)
            })
            .collect()
    }
}

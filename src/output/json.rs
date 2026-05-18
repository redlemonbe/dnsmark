use crate::stats::StatsSnapshot;

pub fn print_json(snap: &StatsSnapshot) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(snap)?;
    println!("{}", json);
    Ok(())
}

pub mod csv;
pub mod json;
pub mod text;
pub mod tui;

use crate::config::Config;
use crate::stats::StatsSnapshot;

pub fn print_output(snap: &StatsSnapshot, config: &Config) -> anyhow::Result<()> {
    if config.json_output {
        json::print_json(snap)?;
    } else {
        text::print_result(snap);
    }

    if let Some(ref csv_path) = config.csv_file {
        csv::write_csv(snap, csv_path)?;
    }

    Ok(())
}

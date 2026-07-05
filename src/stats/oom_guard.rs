use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::autodetect::read_proc_meminfo_total_and_avail_kb;

pub async fn run(shutdown: Arc<AtomicBool>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        interval.tick().await;
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        let (total, avail) = read_proc_meminfo_total_and_avail_kb();
        if total == 0 {
            continue;
        }
        let pct = avail * 100 / total;
        if pct < 10 {
            tracing::warn!("RAM < 10% available ({} kB free), stopping benchmark", avail);
            shutdown.store(true, Ordering::Relaxed);
            break;
        } else if pct < 20 {
            tracing::warn!("RAM < 20% available ({} kB free)", avail);
        }
    }
}

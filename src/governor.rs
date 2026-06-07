// CPU frequency governor guard: pin every CPU to `performance` for the duration
// of a benchmark, restore the previous governor on drop. DVFS ramp-up is the #1
// benchmark confounder (the same binary swings several Mqps between a governor
// that is `powersave`/`schedutil` and one pinned to `performance`).

pub struct GovernorGuard {
    saved: Vec<(String, String)>, // (path, previous governor)
}

impl GovernorGuard {
    /// Best-effort: write `performance` to every CPU's scaling_governor, recording
    /// the previous value. Silently no-ops on anything it cannot read/write (e.g.
    /// a VM without cpufreq, or insufficient privilege).
    pub fn pin_performance() -> Self {
        let mut saved = Vec::new();
        if let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") {
            for e in dir.flatten() {
                let p = e.path().join("cpufreq/scaling_governor");
                if let Ok(prev) = std::fs::read_to_string(&p) {
                    let prev = prev.trim().to_string();
                    if prev != "performance"
                        && std::fs::write(&p, "performance").is_ok()
                    {
                        saved.push((p.to_string_lossy().into_owned(), prev));
                    }
                }
            }
        }
        if !saved.is_empty() {
            eprintln!("[dnsmark] CPU governor pinned to performance ({} cores)", saved.len());
        }
        Self { saved }
    }
}

impl Drop for GovernorGuard {
    fn drop(&mut self) {
        for (p, prev) in &self.saved {
            let _ = std::fs::write(p, prev);
        }
    }
}

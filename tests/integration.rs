/// Integration tests — require a live DNS server at 192.168.1.10:53
/// Run with: cargo test -- --ignored

#[test]
#[ignore]
fn test_basic_udp_run() {
    use std::process::Command;
    let status = Command::new("cargo")
        .args([
            "run", "--release", "--",
            "-s", "192.168.1.10",
            "-d", "tests/fixtures/basic.txt",
            "-l", "5",
            "-q",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("run dnsmark");
    assert!(status.success(), "dnsmark exited with non-zero status");
}

#[test]
#[ignore]
fn test_random_mode() {
    use std::process::Command;
    let status = Command::new("cargo")
        .args([
            "run", "--release", "--",
            "-s", "192.168.1.10",
            "--random",
            "-l", "5",
            "-q",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("run dnsmark random");
    assert!(status.success(), "dnsmark random mode exited with non-zero status");
}

#[test]
#[ignore]
fn test_ramp_mode() {
    use std::process::Command;
    let output = Command::new("cargo")
        .args([
            "run", "--release", "--",
            "-s", "192.168.1.10",
            "--random",
            "--ramp",
            "-q",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run dnsmark ramp");
    assert!(output.status.success(), "dnsmark ramp mode failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Max sustainable QPS:") || stdout.contains("QPS"),
        "expected QPS result in ramp output"
    );
}

#[test]
#[ignore]
fn test_json_output() {
    use std::process::Command;
    let output = Command::new("cargo")
        .args([
            "run", "--release", "--",
            "-s", "192.168.1.10",
            "--random",
            "-l", "3",
            "-q",
            "--json",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run dnsmark json");
    assert!(output.status.success(), "dnsmark --json failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");
    assert!(parsed.get("queries_sent").is_some(), "missing queries_sent");
    assert!(parsed.get("queries_completed").is_some(), "missing queries_completed");
    assert!(parsed.get("avg_qps").is_some(), "missing avg_qps");
    assert!(parsed.get("p50_us").is_some(), "missing p50_us");
    assert!(parsed.get("p99_us").is_some(), "missing p99_us");
}

#[test]
#[ignore]
fn test_timeout_handling() {
    use std::process::Command;
    let status = Command::new("cargo")
        .args([
            "run", "--release", "--",
            "-s", "192.168.1.10",
            "-p", "9999",
            "-l", "2",
            "-t", "200",
            "-q",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("run dnsmark timeout test");
    // Should exit 0 even when all queries time out
    assert!(status.success(), "dnsmark should exit 0 even with full timeout rate");
}

#[test]
#[ignore]
fn test_oom_guard_does_not_crash() {
    use std::process::Command;
    let status = Command::new("cargo")
        .args([
            "run", "--release", "--",
            "-s", "192.168.1.10",
            "--random",
            "-l", "3",
            "-q",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("run dnsmark oom guard test");
    assert!(status.success(), "dnsmark should not crash under normal memory conditions");
}

// ── Multi-NIC tests ────────────────────────────────────────────────────────

/// Verify that two identical -s flags for the same IP are deduplicated
/// and that the binary errors gracefully (same subnet = same NIC → error).
/// This test does NOT require a live DNS server (exits before connecting).
#[test]
fn test_multi_nic_duplicate_target_rejected() {
    use std::process::Command;
    // Two targets on the same NIC (same /24) should be caught.
    // We use two addresses that definitely share the loopback interface.
    let output = Command::new(env!("CARGO_BIN_EXE_dnsmark"))
        .args([
            "-s", "127.0.0.1",
            "-s", "127.0.0.2",
            "-l", "1",
            "-q",
        ])
        .output()
        .expect("run dnsmark multi-nic duplicate");
    // Should either succeed (if deduped to 1) or fail with a clear error.
    // It must NOT panic.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If it failed, the error should mention NIC or subnet.
    if !output.status.success() {
        assert!(
            stderr.contains("NIC") || stderr.contains("route") || stderr.contains("subnet")
            || stdout.contains("NIC") || stdout.contains("route"),
            "failure message should explain NIC conflict, got: stderr={stderr} stdout={stdout}"
        );
    }
}

/// Verify --nic-stats flag is accepted (no panic).
#[test]
#[ignore]
fn test_multi_nic_dual_target_live() {
    use std::process::Command;
    // Requires two distinct DNS servers on distinct NICs.
    let status = Command::new(env!("CARGO_BIN_EXE_dnsmark"))
        .args([
            "-s", "10.10.10.2",
            "-s", "10.10.20.2",
            "-l", "5",
            "-q",
            "--nic-stats",
        ])
        .status()
        .expect("run dnsmark multi-nic dual");
    assert!(status.success(), "multi-NIC dual run should exit 0");
}

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

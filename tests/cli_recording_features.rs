//! Run also with --no-default-features and default (without persistence).
#[cfg(not(all(feature = "cdk", feature = "persistence", target_os = "linux")))]
#[test]
fn missing_safety_features_cannot_fall_back() {
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_tollgate-agent"))
        .args(["--recording-only", "attempt", "--lightning", "--auto"])
        .env("TOLLGATE_MODE", "lightning")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(78));
    assert_eq!(String::from_utf8(o.stdout).unwrap(),"{\"mode\":\"simulated\",\"outcome\":\"refused\",\"error\":\"recording requires Linux, cdk and persistence features\"}\n");
}

#[test]
fn installers_fail_closed_without_checksums() {
    let unix = include_str!("../install.sh");
    let windows = include_str!("../install.ps1");

    for script in [unix, windows] {
        assert!(script.contains("MARQI_ALLOW_UNVERIFIED"));
        assert!(script.contains("checksum file unavailable"));
        assert!(script.contains("checksum verification failed"));
    }
    assert!(unix.contains("MARQI_ALLOW_UNVERIFIED:-}"));
    assert!(windows.contains("MARQI_ALLOW_UNVERIFIED -eq \"1\""));
    assert!(windows.contains("if (-not $Expected)"));
    assert!(windows.contains(".Trim()"));
}

use std::process::Command;

#[test]
fn help_is_successful_and_invalid_input_is_usage() {
    // Catches help being treated as an error or invalid input returning false success.
    let help = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .arg("--help")
        .output()
        .expect("the command must run");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: mlx-guard <COMMAND>"));
    assert!(help.stderr.is_empty());

    let invalid = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .output()
        .expect("the command must run");
    assert_eq!(invalid.status.code(), Some(64));
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("Usage: mlx-guard <COMMAND>"));
}

#[test]
fn parsed_command_never_reports_false_success_before_runtime_is_wired() {
    // Catches the contract-only shell returning zero without launching or supervising the worker.
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .args(["observe", "--", "true"])
        .output()
        .expect("the command must run");
    assert_eq!(output.status.code(), Some(70));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "supervisor runtime is not implemented\n"
    );
}

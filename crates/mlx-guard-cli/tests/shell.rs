use std::process::Command;

#[test]
fn shell_has_no_public_contract_before_ticket_0018() {
    // Catches accidentally freezing provisional output or argument semantics in the scaffold ticket.
    let output = Command::new(env!("CARGO_BIN_EXE_mlx-guard"))
        .output()
        .expect("the workspace must build the command shell");

    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

use std::env;
use std::process::ExitCode;

use mlx_guard_core::SupervisorOutcome;
use mlx_guard_core::explain::{EarlyCause, explain_early};

fn main() -> ExitCode {
    match mlx_guard_cli::parse_cli(env::args_os()) {
        Ok(parsed) => ExitCode::from(mlx_guard_cli::execute_and_report(parsed).exit_code()),
        Err(error) if error.is_display_only() => {
            mlx_guard_cli::write_stdout(&error.to_string());
            ExitCode::SUCCESS
        }
        Err(error) => {
            let mut text = error.to_string();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(
                &explain_early(EarlyCause::CommandLine { flag: error.flag() }).stderr_lines(),
            );
            mlx_guard_cli::write_stderr(&text);
            ExitCode::from(SupervisorOutcome::InvalidConfiguration.exit_code())
        }
    }
}

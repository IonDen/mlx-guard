use std::env;
use std::process::ExitCode;

use mlx_guard_core::SupervisorOutcome;
use mlx_guard_core::explain::{EarlyCause, explain_early};

fn main() -> ExitCode {
    match mlx_guard_cli::parse_cli(env::args_os()) {
        Ok(parsed) => {
            let result = mlx_guard_cli::execute(parsed);
            mlx_guard_cli::write_summary(&result);
            ExitCode::from(result.outcome.exit_code())
        }
        Err(error) if error.is_display_only() => {
            print!("{error}");
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
            eprint!("{text}");
            ExitCode::from(SupervisorOutcome::InvalidConfiguration.exit_code())
        }
    }
}

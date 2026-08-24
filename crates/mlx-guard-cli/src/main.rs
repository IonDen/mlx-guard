use std::env;
use std::process::ExitCode;

use mlx_guard_core::SupervisorOutcome;

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
            eprint!("{error}");
            ExitCode::from(SupervisorOutcome::InvalidConfiguration.exit_code())
        }
    }
}

//! Frozen v0.1 command-line value objects and parsing.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand};

#[cfg(unix)]
mod runtime;

#[cfg(unix)]
pub use runtime::{RuntimeResult, execute};

const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
const MAX_SAMPLE_INTERVAL: Duration = Duration::from_secs(10);
const MAX_WALL_TIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const MIN_CHECKPOINT_TIMEOUT: Duration = Duration::from_millis(10);
const MAX_CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(60);

const fn policy_band_step(limit: u64) -> u64 {
    let tenth = limit / 10;
    if tenth == 0 { 1 } else { tenth }
}

/// Fully normalized CLI configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedCli {
    /// Selected observation or enforcement mode.
    pub mode: CommandMode,
}

/// Public v0.1 command mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandMode {
    /// Observe footprint without a memory or wall intervention policy.
    Observe(ObserveOptions),
    /// Enforce an explicitly supplied footprint limit and optional wall time.
    Run(RunOptions),
}

/// Normalized observe-only options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObserveOptions {
    /// Options shared with enforcement mode.
    pub common: CommonOptions,
}

/// Normalized enforcement options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOptions {
    /// Required sampled OS-accounted footprint limit.
    pub max_footprint_bytes: u64,
    /// Optional wall-time intervention limit.
    pub wall_time: Option<Duration>,
    /// Optional cooperative checkpoint acknowledgement timeout override.
    pub checkpoint_timeout: Option<Duration>,
    /// Options shared with observe-only mode.
    pub common: CommonOptions,
}

/// Launch options that do not define intervention policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommonOptions {
    /// Requested interval between sampling-loop starts.
    pub sample_interval: Duration,
    /// Explicit destination for the owner-only final JSON report.
    pub report_path: PathBuf,
    /// Optional child working directory.
    pub cwd: Option<PathBuf>,
    /// Whether the child starts without the inherited environment.
    pub clear_env: bool,
    /// Explicit UTF-8 child environment overrides. Duplicate keys are rejected.
    pub env: BTreeMap<String, String>,
    /// Optional inherited descriptor closed when the native runtime is ready for client signals.
    #[doc(hidden)]
    pub client_ready_fd: Option<i32>,
    /// Literal executable and argument vector following the mandatory `--` separator.
    pub command: Vec<OsString>,
}

/// Parse and normalize the v0.1 command line.
///
/// No configuration file or policy environment variables are read. CLI values are the complete
/// policy source; `--env` and `--clear-env` affect only the child process.
///
/// # Errors
///
/// Returns [`CliParseError`] for syntax, range, duplicate-environment, or missing-policy errors.
pub fn parse_cli<I, T>(args: I) -> Result<ParsedCli, CliParseError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let raw = RawCli::try_parse_from(args).map_err(|error| CliParseError::from_clap(&error))?;
    let mode = match raw.command {
        RawCommand::Observe(common) => CommandMode::Observe(ObserveOptions {
            common: normalize(common)?,
        }),
        RawCommand::Run {
            max_footprint_bytes,
            wall_time,
            checkpoint_timeout,
            common,
        } => {
            if max_footprint_bytes < 2 {
                return Err(CliParseError::contract(
                    "--max-footprint must be at least 2B",
                ));
            }
            if max_footprint_bytes
                .checked_add(policy_band_step(max_footprint_bytes))
                .is_none()
            {
                return Err(CliParseError::contract(
                    "--max-footprint is too large for the emergency policy band",
                ));
            }
            CommandMode::Run(RunOptions {
                max_footprint_bytes,
                wall_time,
                checkpoint_timeout,
                common: normalize(common)?,
            })
        }
    };
    Ok(ParsedCli { mode })
}

/// Parse a positive binary byte quantity using `B`, `KiB`, `MiB`, `GiB`, or `TiB`.
///
/// # Errors
///
/// Returns a message when the value is zero, malformed, uses another suffix, or overflows `u64`.
pub fn parse_bytes(value: &str) -> Result<u64, String> {
    let (digits, multiplier) = [
        ("TiB", 1024_u64.pow(4)),
        ("GiB", 1024_u64.pow(3)),
        ("MiB", 1024_u64.pow(2)),
        ("KiB", 1024_u64),
        ("B", 1_u64),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier)| {
        value
            .strip_suffix(suffix)
            .map(|digits| (digits, multiplier))
    })
    .ok_or_else(|| "expected a binary byte suffix: B, KiB, MiB, GiB, or TiB".to_owned())?;
    parse_positive_integer(digits, "byte quantity")?
        .checked_mul(multiplier)
        .ok_or_else(|| "byte quantity overflows u64".to_owned())
}

/// Parse a positive integer duration using `ms`, `s`, `m`, or `h`.
///
/// # Errors
///
/// Returns a message when the value is zero, malformed, uses another suffix, or overflows `u64`
/// milliseconds.
pub fn parse_duration(value: &str) -> Result<Duration, String> {
    let (digits, multiplier_ms) = [("ms", 1_u64), ("s", 1_000), ("m", 60_000), ("h", 3_600_000)]
        .into_iter()
        .find_map(|(suffix, multiplier)| {
            value
                .strip_suffix(suffix)
                .map(|digits| (digits, multiplier))
        })
        .ok_or_else(|| "expected a duration suffix: ms, s, m, or h".to_owned())?;
    let milliseconds = parse_positive_integer(digits, "duration")?
        .checked_mul(multiplier_ms)
        .ok_or_else(|| "duration overflows u64 milliseconds".to_owned())?;
    Ok(Duration::from_millis(milliseconds))
}

fn parse_positive_integer(value: &str, name: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{name} must be a positive integer"));
    }
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{name} overflows u64"))?;
    if parsed == 0 {
        return Err(format!("{name} must be greater than zero"));
    }
    Ok(parsed)
}

fn normalize(raw: RawCommon) -> Result<CommonOptions, CliParseError> {
    if !(MIN_SAMPLE_INTERVAL..=MAX_SAMPLE_INTERVAL).contains(&raw.sample_interval) {
        return Err(CliParseError::contract(
            "--sample-interval must be within 10ms..=10s",
        ));
    }
    let mut env = BTreeMap::new();
    for assignment in raw.env {
        let (key, value) = assignment
            .split_once('=')
            .ok_or_else(|| CliParseError::contract("--env requires KEY=VALUE"))?;
        if key.is_empty() {
            return Err(CliParseError::contract("--env key must not be empty"));
        }
        if env.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(CliParseError::contract(format!(
                "duplicate --env key {key:?}"
            )));
        }
    }
    Ok(CommonOptions {
        sample_interval: raw.sample_interval,
        report_path: raw.report_path,
        cwd: raw.cwd,
        clear_env: raw.clear_env,
        env,
        client_ready_fd: raw.client_ready_fd,
        command: raw.command,
    })
}

/// Stable parsing failure suitable for mapping to exit 64 before worker launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliParseError {
    message: String,
    display_only: bool,
}

impl CliParseError {
    fn from_clap(error: &clap::Error) -> Self {
        Self {
            message: error.to_string(),
            display_only: matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ),
        }
    }

    fn contract(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            display_only: false,
        }
    }

    /// Return whether this value is requested help/version text rather than invalid input.
    #[must_use]
    pub const fn is_display_only(&self) -> bool {
        self.display_only
    }
}

impl fmt::Display for CliParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CliParseError {}

#[derive(Debug, Parser)]
#[command(
    name = "mlx-guard",
    version,
    about = "External runtime safety supervision for MLX commands"
)]
struct RawCli {
    #[command(subcommand)]
    command: RawCommand,
}

#[derive(Debug, Subcommand)]
enum RawCommand {
    /// Observe OS-accounted footprint without memory intervention.
    Observe(RawCommon),
    /// Enforce an explicit OS-accounted footprint limit.
    Run {
        /// Required sampled intervention threshold.
        #[arg(long = "max-footprint", value_parser = parse_bytes)]
        max_footprint_bytes: u64,
        /// Optional wall-time intervention threshold, capped at 30 days.
        #[arg(long, value_parser = parse_wall_time)]
        wall_time: Option<Duration>,
        /// Cooperative checkpoint acknowledgement timeout (10ms..=60s, default 1s).
        #[arg(long, value_parser = parse_checkpoint_timeout)]
        checkpoint_timeout: Option<Duration>,
        /// Launch and sampling options.
        #[command(flatten)]
        common: RawCommon,
    },
}

#[derive(Debug, Args)]
struct RawCommon {
    /// Interval between sampling-loop starts (10ms..=10s).
    #[arg(long, default_value = "50ms", value_parser = parse_duration)]
    sample_interval: Duration,
    /// Final JSON report path in an existing owner-only directory.
    #[arg(long = "report", value_name = "PATH")]
    report_path: PathBuf,
    /// Child working directory, validated before launch.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Start the child with no inherited environment.
    #[arg(long)]
    clear_env: bool,
    /// Set one UTF-8 child environment value; duplicate keys are invalid.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,
    /// Internal Python-client readiness descriptor. The invoking client transfers ownership.
    #[arg(long, hide = true, value_parser = parse_client_ready_fd)]
    client_ready_fd: Option<i32>,
    /// Literal executable and arguments. The `--` separator is mandatory.
    #[arg(last = true, required = true, num_args = 1.., value_name = "COMMAND")]
    command: Vec<OsString>,
}

fn parse_client_ready_fd(value: &str) -> Result<i32, String> {
    let descriptor = value
        .parse::<i32>()
        .map_err(|_| "client readiness descriptor must be an integer".to_owned())?;
    if descriptor < 3 {
        return Err("client readiness descriptor must not replace standard I/O".to_owned());
    }
    Ok(descriptor)
}

fn parse_wall_time(value: &str) -> Result<Duration, String> {
    let duration = parse_duration(value)?;
    if duration > MAX_WALL_TIME {
        return Err("--wall-time must not exceed 30 days".to_owned());
    }
    Ok(duration)
}

fn parse_checkpoint_timeout(value: &str) -> Result<Duration, String> {
    let duration = parse_duration(value)?;
    if !(MIN_CHECKPOINT_TIMEOUT..=MAX_CHECKPOINT_TIMEOUT).contains(&duration) {
        return Err("--checkpoint-timeout must be within 10ms..=60s".to_owned());
    }
    Ok(duration)
}

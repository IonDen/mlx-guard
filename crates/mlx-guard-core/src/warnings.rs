//! Warning lines printed on stderr: the launch fit line and the warning-band lines of a run.
//!
//! Each renderer is pure and returns one newline-ended line holding only the numbers it is given
//! and fixed text. Byte counts and durations use the same formatters as the plain outcome lines,
//! so a footprint reads the same during a run and at its end. Values one sentence compares go
//! through `compared_bytes`, which prints them all as exact bytes when two different values would
//! otherwise render alike.

use crate::explain::{compared_bytes, doc_url, format_duration_ms};

/// Troubleshooting-page anchor the band lines link to. Kept apart from `explain::ANCHORS`.
pub const WARNING_ANCHOR: &str = "warnings-during-a-run";

/// The line printed after the launch banner when the limit is more than the launch estimate.
#[must_use]
pub fn fit_line(limit_bytes: u64, estimate_bytes: u64, version: &str) -> String {
    let [limit, estimate] = compared_bytes([limit_bytes, estimate_bytes]);
    format!(
        "mlx-guard: warning: your limit of {limit} is more than the {estimate} macOS reported as \
         not wired just before launch, so a run that reaches it may not fit in memory on this \
         Mac. That number is one estimate at one moment and counts every other program's memory \
         as room. mlx-guard runs with your limit as given. More: {}\n",
        doc_url(version, WARNING_ANCHOR),
    )
}

/// The line printed when a sample first moves the footprint into the warning band.
#[must_use]
pub fn band_entered_line(
    footprint: u64,
    at_ms: u64,
    warning: u64,
    limit: u64,
    emergency: u64,
    required_samples: u32,
    version: &str,
) -> String {
    let [footprint, warning, limit, emergency] =
        compared_bytes([footprint, warning, limit, emergency]);
    format!(
        "mlx-guard: warning: the OS-accounted footprint reached {footprint} at {}, at or above \
         the {warning} warning threshold for your {limit} limit; {required_samples} samples in a \
         row at or above the limit stop the command, and 1 sample at or above the {emergency} \
         emergency threshold kills it at once, with no checkpoint. More: {}\n",
        format_duration_ms(at_ms),
        doc_url(version, WARNING_ANCHOR),
    )
}

/// The line repeated while the footprint has not fallen back to the recovery threshold.
#[must_use]
pub fn band_still_line(
    footprint: u64,
    at_ms: u64,
    since_ms: u64,
    recovery: u64,
    limit: u64,
) -> String {
    let [footprint, recovery, limit] = compared_bytes([footprint, recovery, limit]);
    format!(
        "mlx-guard: warning: the OS-accounted footprint is {footprint} at {} and has not fallen \
         to the {recovery} recovery threshold since {}; your limit is {limit}.\n",
        format_duration_ms(at_ms),
        format_duration_ms(since_ms),
    )
}

/// The line printed when an announced warning ends at the recovery threshold.
#[must_use]
pub fn band_cleared_line(footprint: u64, at_ms: u64, recovery: u64, limit: u64) -> String {
    let [footprint, recovery, limit] = compared_bytes([footprint, recovery, limit]);
    format!(
        "mlx-guard: warning cleared: the OS-accounted footprint fell to {footprint} at {}, at or \
         below the {recovery} recovery threshold for your {limit} limit.\n",
        format_duration_ms(at_ms),
    )
}

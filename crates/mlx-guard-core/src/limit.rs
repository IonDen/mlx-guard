//! The footprint limit a run enforces: a size the user typed, or one picked automatically from
//! the memory macOS has not wired just before launch.

use serde::{Deserialize, Serialize};

use crate::report::{MemoryCategories, Observed, PrelaunchRecord};

/// The smallest automatic limit; an estimate whose share falls under it gives no usable number.
pub const AUTO_LIMIT_FLOOR_BYTES: u64 = 1 << 30;

const MIB: u64 = 1 << 20;

/// What the user asked `--max-footprint` for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FootprintRequest {
    /// A typed size, enforced exactly as given.
    Bytes(u64),
    /// A limit picked from the memory estimate just before launch.
    Auto,
}

/// Where a run's limit came from, as recorded in its report.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitSource {
    Explicit,
    Auto,
    /// A source written by a later release; read, never written by this one.
    #[serde(other)]
    Unknown,
}

/// The rule that turned the memory estimate into an automatic limit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoLimitRule {
    /// 75 % of the memory macOS did not report as wired, rounded down to a whole MiB.
    #[serde(rename = "not_wired_75_percent")]
    NotWired75Percent,
    /// A rule written by a later release; read, never written or applied by this one.
    #[serde(other)]
    Unknown,
}

impl AutoLimitRule {
    /// The rule this release applies.
    pub const CURRENT: Self = Self::NotWired75Percent;

    /// The share of the estimate the rule takes, in percent; 0 for a rule this release does not
    /// know.
    #[must_use]
    pub const fn percent(self) -> u64 {
        match self {
            Self::NotWired75Percent => 75,
            Self::Unknown => 0,
        }
    }

    /// The limit this rule picks from `estimate_bytes`, or `None` when it is under
    /// [`AUTO_LIMIT_FLOOR_BYTES`] or the rule is not one this release can apply.
    #[must_use]
    pub fn limit_for(self, estimate_bytes: u64) -> Option<u64> {
        match self {
            Self::NotWired75Percent => {
                let share = u128::from(estimate_bytes) * 3 / 4;
                let limit = u64::try_from(share).ok()? / MIB * MIB;
                (limit >= AUTO_LIMIT_FLOOR_BYTES).then_some(limit)
            }
            Self::Unknown => None,
        }
    }
}

/// How an automatic limit was chosen, as recorded in the report's prelaunch block.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutoLimitRecord {
    pub rule: AutoLimitRule,
    pub limit_bytes: u64,
}

/// The limit a run enforces and where it came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedLimit {
    pub limit_bytes: u64,
    pub source: LimitSource,
}

/// Why `--max-footprint auto` gave no usable number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoLimitUnavailable {
    /// macOS gave no memory reading just before launch.
    NoEstimate,
    /// The rule's share of the reading is under [`AUTO_LIMIT_FLOOR_BYTES`].
    BelowFloor,
}

/// Resolve the requested limit from the one memory reading taken before launch, and build the
/// prelaunch record that reading goes into.
///
/// A typed size always resolves to itself, with the record [`PrelaunchRecord::for_limit`] builds.
///
/// # Errors
///
/// Returns [`AutoLimitUnavailable`] when `auto` was asked for and the reading is missing or too
/// small for a limit of at least [`AUTO_LIMIT_FLOOR_BYTES`].
pub fn resolve_limit(
    request: FootprintRequest,
    categories: Observed<MemoryCategories>,
) -> Result<(ResolvedLimit, PrelaunchRecord), AutoLimitUnavailable> {
    match request {
        FootprintRequest::Bytes(limit_bytes) => Ok((
            ResolvedLimit {
                limit_bytes,
                source: LimitSource::Explicit,
            },
            PrelaunchRecord::for_limit(limit_bytes, categories),
        )),
        FootprintRequest::Auto => {
            let Observed::Available { value } = &categories else {
                return Err(AutoLimitUnavailable::NoEstimate);
            };
            let rule = AutoLimitRule::CURRENT;
            let limit_bytes = rule
                .limit_for(value.estimate())
                .ok_or(AutoLimitUnavailable::BelowFloor)?;
            let mut record = PrelaunchRecord::for_limit(limit_bytes, categories);
            record.auto_limit = Some(AutoLimitRecord { rule, limit_bytes });
            Ok((
                ResolvedLimit {
                    limit_bytes,
                    source: LimitSource::Auto,
                },
                record,
            ))
        }
    }
}

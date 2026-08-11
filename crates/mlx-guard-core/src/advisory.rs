#![allow(unsafe_code)]

use std::time::Duration;
use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    AdvisoryFreshness, AdvisoryMetadata, AdvisoryMetricMetadata, AdvisoryMetrics, AdvisoryScope,
    AdvisorySource, FootprintSample, MemoryPressureLevel, ObservationError, ObservationFailureKind,
    Observed, SampleOutcome, SampleWindow, UnavailableReason,
};

/// The only calibration artifact schema major understood by this package.
pub const CALIBRATION_SCHEMA_VERSION: u32 = 1;

/// One system-wide advisory snapshot captured independently from the worker footprint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdvisorySnapshot {
    captured_at: Duration,
    pressure_events: Observed<u64>,
    pressure_level: Observed<MemoryPressureLevel>,
    swap_bytes: Observed<u64>,
    compressor_bytes: Observed<u64>,
    wired_bytes: Observed<u64>,
}

impl AdvisorySnapshot {
    #[must_use]
    pub const fn new(
        captured_at: Duration,
        pressure_events: Observed<u64>,
        pressure_level: Observed<MemoryPressureLevel>,
        swap_bytes: Observed<u64>,
        compressor_bytes: Observed<u64>,
        wired_bytes: Observed<u64>,
    ) -> Self {
        Self {
            captured_at,
            pressure_events,
            pressure_level,
            swap_bytes,
            compressor_bytes,
            wired_bytes,
        }
    }

    /// Project system observations plus derived growth into additive schema-v1 advisory fields.
    #[must_use]
    pub fn with_growth(
        &self,
        growth_bytes_per_second: &Observed<i64>,
        growth_observed_at: Option<Duration>,
    ) -> AdvisoryMetrics {
        let captured_at_ms = duration_ms(self.captured_at);
        let growth_at_ms = growth_observed_at
            .map(duration_ms)
            .or_else(|| stale_timestamp(growth_bytes_per_second))
            .unwrap_or(captured_at_ms);
        AdvisoryMetrics {
            pressure_events: self.pressure_events.clone(),
            swap_bytes: self.swap_bytes.clone(),
            compressor_bytes: self.compressor_bytes.clone(),
            wired_bytes: self.wired_bytes.clone(),
            growth_bytes_per_second: growth_bytes_per_second.clone(),
            pressure_level: Some(self.pressure_level.clone()),
            metadata: Some(AdvisoryMetadata {
                pressure_events: metadata(
                    AdvisoryScope::System,
                    AdvisorySource::DispatchMemoryPressure,
                    observation_timestamp(&self.pressure_events, captured_at_ms),
                    &self.pressure_events,
                ),
                pressure_level: metadata(
                    AdvisoryScope::System,
                    AdvisorySource::DispatchMemoryPressure,
                    observation_timestamp(&self.pressure_level, captured_at_ms),
                    &self.pressure_level,
                ),
                swap_bytes: metadata(
                    AdvisoryScope::System,
                    AdvisorySource::SysctlVmSwapusage,
                    observation_timestamp(&self.swap_bytes, captured_at_ms),
                    &self.swap_bytes,
                ),
                compressor_bytes: metadata(
                    AdvisoryScope::System,
                    AdvisorySource::HostStatistics64,
                    observation_timestamp(&self.compressor_bytes, captured_at_ms),
                    &self.compressor_bytes,
                ),
                wired_bytes: metadata(
                    AdvisoryScope::System,
                    AdvisorySource::HostStatistics64,
                    observation_timestamp(&self.wired_bytes, captured_at_ms),
                    &self.wired_bytes,
                ),
                growth_bytes_per_second: metadata(
                    AdvisoryScope::OwnedProcessGroup,
                    AdvisorySource::DerivedFootprintSamples,
                    growth_at_ms,
                    growth_bytes_per_second,
                ),
            }),
        }
    }

    #[must_use]
    pub const fn pressure_level(&self) -> &Observed<MemoryPressureLevel> {
        &self.pressure_level
    }

    fn system_metrics_available(&self) -> bool {
        matches!(self.swap_bytes, Observed::Available { .. })
            && matches!(self.compressor_bytes, Observed::Available { .. })
            && matches!(self.wired_bytes, Observed::Available { .. })
    }
}

fn metadata<T>(
    scope: AdvisoryScope,
    source: AdvisorySource,
    captured_at_ms: u64,
    observation: &Observed<T>,
) -> AdvisoryMetricMetadata {
    AdvisoryMetricMetadata {
        scope,
        source,
        captured_at_ms,
        freshness: freshness(observation),
    }
}

fn freshness<T>(observation: &Observed<T>) -> AdvisoryFreshness {
    match observation {
        Observed::Available { .. } => AdvisoryFreshness::Fresh,
        Observed::Unknown => AdvisoryFreshness::InitialUnknown,
        Observed::Stale { .. } => AdvisoryFreshness::Stale,
        Observed::Unavailable { .. } | Observed::Error { .. } => AdvisoryFreshness::Unavailable,
    }
}

fn observation_timestamp<T>(observation: &Observed<T>, fallback: u64) -> u64 {
    stale_timestamp(observation).unwrap_or(fallback)
}

fn stale_timestamp<T>(observation: &Observed<T>) -> Option<u64> {
    match observation {
        Observed::Stale { last_seen_at_ms } => Some(*last_seen_at_ms),
        _ => None,
    }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Guidance deliberately avoids selecting a destructive threshold from one run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationGuidance {
    ChooseExplicitLimitFromRepeatedRepresentativeRuns,
}

/// Bounded evidence produced by an observe-only run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CalibrationArtifact {
    pub schema_version: u32,
    pub observation_only: bool,
    pub safety_certified: bool,
    pub total_samples: u64,
    pub complete_samples: u64,
    pub incomplete_samples: u64,
    pub observed_duration_ms: u64,
    pub peak_aggregate_footprint_bytes: Observed<u64>,
    pub peak_growth_bytes_per_second: Observed<i64>,
    pub automatic_limit_bytes: Option<u64>,
    pub guidance: CalibrationGuidance,
}

impl CalibrationArtifact {
    /// Validate observe-only invariants before persistence or use as limit evidence.
    ///
    /// # Errors
    ///
    /// Returns [`CalibrationError::InvalidArtifact`] for impossible counters, a selected automatic
    /// limit, a safety claim, or an incoherent peak.
    pub fn validate(&self) -> Result<(), CalibrationError> {
        let counters_match =
            self.complete_samples.checked_add(self.incomplete_samples) == Some(self.total_samples);
        let footprint_matches = matches!(
            (&self.peak_aggregate_footprint_bytes, self.complete_samples),
            (Observed::Unknown, 0) | (Observed::Available { .. }, 1..)
        );
        let growth_matches = !matches!(
            self.peak_growth_bytes_per_second,
            Observed::Available { value } if value <= 0
        );
        if self.schema_version != CALIBRATION_SCHEMA_VERSION
            || !self.observation_only
            || self.safety_certified
            || self.automatic_limit_bytes.is_some()
            || !counters_match
            || !footprint_matches
            || !growth_matches
        {
            return Err(CalibrationError::InvalidArtifact);
        }
        Ok(())
    }

    /// Serialize a validated calibration artifact with a final newline.
    ///
    /// # Errors
    ///
    /// Returns a typed validation or JSON error.
    pub fn to_json_pretty(&self) -> Result<String, CalibrationError> {
        self.validate()?;
        let mut encoded = serde_json::to_string_pretty(self).map_err(CalibrationError::Json)?;
        encoded.push('\n');
        Ok(encoded)
    }

    /// Parse and validate a calibration artifact.
    ///
    /// # Errors
    ///
    /// Returns a typed validation or JSON error.
    pub fn from_json(value: &str) -> Result<Self, CalibrationError> {
        let artifact: Self = serde_json::from_str(value).map_err(CalibrationError::Json)?;
        artifact.validate()?;
        Ok(artifact)
    }
}

/// Calibration artifact validation or serialization failure.
#[derive(Debug)]
pub enum CalibrationError {
    InvalidArtifact,
    Json(serde_json::Error),
}

impl fmt::Display for CalibrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidArtifact => "calibration artifact is internally inconsistent",
            Self::Json(_) => "calibration artifact JSON is malformed",
        })
    }
}

impl Error for CalibrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::InvalidArtifact => None,
        }
    }
}

/// Observe-only projection over the same bounded footprint samples used by enforcement.
#[derive(Debug, Default)]
pub struct ObserveCalibration {
    growth: GrowthTracker,
    total_samples: u64,
    complete_samples: u64,
    incomplete_samples: u64,
    first_captured_at: Option<Duration>,
    last_captured_at: Option<Duration>,
    peak_aggregate_bytes: Option<u64>,
    peak_growth_bytes_per_second: Option<i64>,
}

impl ObserveCalibration {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Project one sampler window into report fields without constructing or advancing policy.
    #[must_use]
    pub fn record_sample(
        &mut self,
        sample: &FootprintSample,
        processed_at: Duration,
        advisory: &AdvisorySnapshot,
    ) -> SampleWindow {
        self.total_samples = self.total_samples.saturating_add(1);
        self.first_captured_at.get_or_insert(sample.finished_at);
        self.last_captured_at = Some(sample.finished_at);
        match sample.outcome {
            SampleOutcome::Complete { total_bytes } => {
                self.complete_samples = self.complete_samples.saturating_add(1);
                self.peak_aggregate_bytes = Some(
                    self.peak_aggregate_bytes
                        .map_or(total_bytes, |peak| peak.max(total_bytes)),
                );
            }
            _ => {
                self.incomplete_samples = self.incomplete_samples.saturating_add(1);
            }
        }
        let (growth, growth_at) = self.growth.observe(sample);
        if let Observed::Available { value } = growth
            && value > 0
        {
            self.peak_growth_bytes_per_second = Some(
                self.peak_growth_bytes_per_second
                    .map_or(value, |peak| peak.max(value)),
            );
        }
        SampleWindow {
            captured_at_ms: duration_ms(sample.finished_at),
            processed_at_ms: duration_ms(processed_at),
            window_ms: positive_duration_ms(sample.finished_at.saturating_sub(sample.started_at)),
            aggregate_footprint_bytes: sample_observation(&sample.outcome),
            advisory: advisory.with_growth(&growth, growth_at),
        }
    }

    /// This path has no intervention surface; the value is frozen for regression tests.
    #[must_use]
    pub const fn intervention_count(&self) -> u64 {
        0
    }

    #[must_use]
    pub fn artifact(&self) -> CalibrationArtifact {
        let observed_duration = match (self.first_captured_at, self.last_captured_at) {
            (Some(first), Some(last)) => last.saturating_sub(first),
            _ => Duration::ZERO,
        };
        CalibrationArtifact {
            schema_version: CALIBRATION_SCHEMA_VERSION,
            observation_only: true,
            safety_certified: false,
            total_samples: self.total_samples,
            complete_samples: self.complete_samples,
            incomplete_samples: self.incomplete_samples,
            observed_duration_ms: duration_ms(observed_duration),
            peak_aggregate_footprint_bytes: self
                .peak_aggregate_bytes
                .map_or(Observed::Unknown, |value| Observed::Available { value }),
            peak_growth_bytes_per_second: self
                .peak_growth_bytes_per_second
                .map_or(Observed::Unknown, |value| Observed::Available { value }),
            automatic_limit_bytes: None,
            guidance: CalibrationGuidance::ChooseExplicitLimitFromRepeatedRepresentativeRuns,
        }
    }
}

fn positive_duration_ms(value: Duration) -> u64 {
    let milliseconds = duration_ms(value);
    if milliseconds == 0 && !value.is_zero() {
        1
    } else {
        milliseconds
    }
}

#[derive(Debug, Default)]
struct GrowthTracker {
    baseline: Option<(Duration, u64)>,
    last_rate_at: Option<Duration>,
}

impl GrowthTracker {
    fn observe(&mut self, sample: &FootprintSample) -> (Observed<i64>, Option<Duration>) {
        let SampleOutcome::Complete { total_bytes } = sample.outcome else {
            self.baseline = None;
            return self.last_rate_at.map_or((Observed::Unknown, None), |at| {
                (
                    Observed::Stale {
                        last_seen_at_ms: duration_ms(at),
                    },
                    Some(at),
                )
            });
        };
        let current = (sample.finished_at, total_bytes);
        let Some((previous_at, previous_bytes)) = self.baseline.replace(current) else {
            return (Observed::Unknown, None);
        };
        let Some(elapsed) = sample.finished_at.checked_sub(previous_at) else {
            self.baseline = None;
            return (
                Observed::Error {
                    code: ObservationError::ClockAnomaly,
                },
                None,
            );
        };
        if elapsed.is_zero() {
            self.baseline = None;
            return (
                Observed::Error {
                    code: ObservationError::ClockAnomaly,
                },
                None,
            );
        }
        let byte_delta = i128::from(total_bytes) - i128::from(previous_bytes);
        let rate = byte_delta
            .checked_mul(1_000_000_000)
            .and_then(|scaled| scaled.checked_div(i128::try_from(elapsed.as_nanos()).ok()?))
            .and_then(|value| i64::try_from(value).ok());
        let Some(value) = rate else {
            return (
                Observed::Error {
                    code: ObservationError::Internal,
                },
                None,
            );
        };
        self.last_rate_at = Some(sample.finished_at);
        (Observed::Available { value }, self.last_rate_at)
    }
}

fn sample_observation(outcome: &SampleOutcome) -> Observed<u64> {
    match outcome {
        SampleOutcome::Complete { total_bytes } => Observed::Available {
            value: *total_bytes,
        },
        SampleOutcome::Partial { .. } => Observed::Unknown,
        SampleOutcome::Overflow => Observed::Error {
            code: ObservationError::Internal,
        },
        SampleOutcome::SnapshotFailed { kind } => snapshot_failure(*kind),
        SampleOutcome::ClockDiscontinuity => Observed::Error {
            code: ObservationError::ClockAnomaly,
        },
    }
}

fn snapshot_failure(kind: ObservationFailureKind) -> Observed<u64> {
    match kind {
        ObservationFailureKind::Unsupported | ObservationFailureKind::Unavailable => {
            Observed::Unavailable {
                reason: UnavailableReason::NotSupported,
            }
        }
        ObservationFailureKind::PermissionDenied => Observed::Error {
            code: ObservationError::PermissionDenied,
        },
        ObservationFailureKind::Disappeared | ObservationFailureKind::StaleIdentity => {
            Observed::Error {
                code: ObservationError::ProcessMissing,
            }
        }
        ObservationFailureKind::MalformedData => Observed::Error {
            code: ObservationError::MalformedKernelData,
        },
        ObservationFailureKind::EnumerationFailed => Observed::Error {
            code: ObservationError::Internal,
        },
    }
}

/// Non-fatal warning emitted before launch; no variant is an automatic rejection threshold.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrelaunchWarning {
    FootprintObservationUnavailable,
    PressureStateUnknown,
    MemoryPressureWarning,
    MemoryPressureCritical,
    AdvisorySystemMetricsPartial,
}

/// Capability and current-state evidence captured before the worker starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrelaunchSummary {
    pub footprint_observation: Observed<bool>,
    pub advisory: AdvisoryMetrics,
    pub warnings: Vec<PrelaunchWarning>,
    pub reject_launch: bool,
}

impl PrelaunchSummary {
    #[must_use]
    pub fn new(footprint_observation: Observed<bool>, snapshot: &AdvisorySnapshot) -> Self {
        let mut warnings = Vec::new();
        if footprint_observation != (Observed::Available { value: true }) {
            warnings.push(PrelaunchWarning::FootprintObservationUnavailable);
        }
        match snapshot.pressure_level() {
            Observed::Available {
                value: MemoryPressureLevel::Normal,
            } => {}
            Observed::Available {
                value: MemoryPressureLevel::Warning,
            } => warnings.push(PrelaunchWarning::MemoryPressureWarning),
            Observed::Available {
                value: MemoryPressureLevel::Critical,
            } => warnings.push(PrelaunchWarning::MemoryPressureCritical),
            Observed::Unknown
            | Observed::Unavailable { .. }
            | Observed::Stale { .. }
            | Observed::Error { .. } => warnings.push(PrelaunchWarning::PressureStateUnknown),
        }
        if !snapshot.system_metrics_available() {
            warnings.push(PrelaunchWarning::AdvisorySystemMetricsPartial);
        }
        Self {
            footprint_observation,
            advisory: snapshot.with_growth(&Observed::Unknown, None),
            warnings,
            reject_launch: false,
        }
    }
}

/// Compile-selected system advisory observer. Unsupported platforms return typed unavailability.
#[derive(Debug)]
pub struct NativeAdvisoryObserver {
    inner: native::Observer,
}

impl Default for NativeAdvisoryObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeAdvisoryObserver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: native::Observer::new(),
        }
    }

    #[must_use]
    pub fn snapshot(&self, captured_at: Duration, max_pressure_age: Duration) -> AdvisorySnapshot {
        self.inner.snapshot(captured_at, max_pressure_age)
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    use super::{AdvisorySnapshot, Duration, Observed, UnavailableReason};

    #[derive(Debug)]
    pub(super) struct Observer {
        reason: UnavailableReason,
    }

    impl Observer {
        pub(super) fn new() -> Self {
            Self {
                reason: UnavailableReason::NotSupported,
            }
        }

        pub(super) fn snapshot(
            &self,
            captured_at: Duration,
            _max_pressure_age: Duration,
        ) -> AdvisorySnapshot {
            AdvisorySnapshot::new(
                captured_at,
                unavailable(self.reason),
                unavailable(self.reason),
                unavailable(self.reason),
                unavailable(self.reason),
                unavailable(self.reason),
            )
        }
    }

    fn unavailable<T>(reason: UnavailableReason) -> Observed<T> {
        Observed::Unavailable { reason }
    }
}

#[cfg(target_os = "macos")]
mod native {
    use std::ffi::{c_char, c_int, c_uint, c_void};
    use std::fmt;
    use std::mem::{MaybeUninit, offset_of, size_of};
    use std::ptr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::{
        AdvisorySnapshot, Duration, MemoryPressureLevel, ObservationError, Observed,
        UnavailableReason,
    };

    const HOST_VM_INFO64: c_int = 4;
    const KERN_SUCCESS: c_int = 0;
    const PRESSURE_NORMAL: usize = 0x01;
    const PRESSURE_WARNING: usize = 0x02;
    const PRESSURE_CRITICAL: usize = 0x04;
    const PRESSURE_MASK: usize = PRESSURE_NORMAL | PRESSURE_WARNING | PRESSURE_CRITICAL;

    #[repr(C)]
    struct VmStatistics64 {
        free_count: c_uint,
        active_count: c_uint,
        inactive_count: c_uint,
        wire_count: c_uint,
        zero_fill_count: u64,
        reactivations: u64,
        pageins: u64,
        pageouts: u64,
        faults: u64,
        cow_faults: u64,
        lookups: u64,
        hits: u64,
        purges: u64,
        purgeable_count: c_uint,
        speculative_count: c_uint,
        decompressions: u64,
        compressions: u64,
        swapins: u64,
        swapouts: u64,
        compressor_page_count: c_uint,
        throttled_count: c_uint,
        external_page_count: c_uint,
        internal_page_count: c_uint,
        total_uncompressed_pages_in_compressor: u64,
        swapped_count: u64,
        total_tag_storage_pages: u64,
        nontag_pageable_tag_storage_pages: u64,
        nontag_wired_tag_storage_pages: u64,
        free_tag_storage_pages: u64,
    }

    #[repr(C)]
    struct SwapUsage {
        total: u64,
        available: u64,
        used: u64,
        page_size: c_uint,
        encrypted: c_int,
    }

    unsafe extern "C" {
        static _dispatch_source_type_memorypressure: c_void;
        static mach_task_self_: c_uint;

        fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
        fn dispatch_source_create(
            source_type: *const c_void,
            handle: usize,
            mask: usize,
            queue: *mut c_void,
        ) -> *mut c_void;
        fn dispatch_set_context(object: *mut c_void, context: *mut c_void);
        fn dispatch_set_finalizer_f(
            object: *mut c_void,
            finalizer: Option<unsafe extern "C" fn(*mut c_void)>,
        );
        fn dispatch_source_set_event_handler_f(
            source: *mut c_void,
            handler: Option<unsafe extern "C" fn(*mut c_void)>,
        );
        fn dispatch_source_get_data(source: *mut c_void) -> usize;
        fn dispatch_activate(object: *mut c_void);
        fn dispatch_source_cancel(source: *mut c_void);
        fn dispatch_release(object: *mut c_void);

        fn mach_host_self() -> c_uint;
        fn mach_port_deallocate(task: c_uint, name: c_uint) -> c_int;
        fn host_page_size(host: c_uint, page_size: *mut usize) -> c_int;
        fn host_statistics64(
            host: c_uint,
            flavor: c_int,
            info: *mut c_int,
            count: *mut c_uint,
        ) -> c_int;
        fn sysctlbyname(
            name: *const c_char,
            old_value: *mut c_void,
            old_len: *mut usize,
            new_value: *mut c_void,
            new_len: usize,
        ) -> c_int;
    }

    #[derive(Debug)]
    pub(super) struct Observer {
        pressure: Option<PressureMonitor>,
    }

    impl Observer {
        pub(super) fn new() -> Self {
            Self {
                pressure: PressureMonitor::new(),
            }
        }

        pub(super) fn snapshot(
            &self,
            captured_at: Duration,
            max_pressure_age: Duration,
        ) -> AdvisorySnapshot {
            let (pressure_events, pressure_level) = self.pressure.as_ref().map_or_else(
                || {
                    (
                        Observed::Unavailable {
                            reason: UnavailableReason::NotSupported,
                        },
                        Observed::Unavailable {
                            reason: UnavailableReason::NotSupported,
                        },
                    )
                },
                |monitor| monitor.observation(captured_at, max_pressure_age),
            );
            let swap_bytes = read_swap_bytes();
            let (compressor_bytes, wired_bytes) = read_vm_bytes();
            AdvisorySnapshot::new(
                captured_at,
                pressure_events,
                pressure_level,
                swap_bytes,
                compressor_bytes,
                wired_bytes,
            )
        }
    }

    struct PressureCounters {
        deliveries: AtomicU64,
        latest_mask: AtomicUsize,
        last_event_ns: AtomicU64,
    }

    struct PressureContext {
        source: *mut c_void,
        counters: Arc<PressureCounters>,
    }

    struct PressureMonitor {
        source: *mut c_void,
        counters: Arc<PressureCounters>,
    }

    impl fmt::Debug for PressureMonitor {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("PressureMonitor(<dispatch-source>)")
        }
    }

    impl PressureMonitor {
        fn new() -> Option<Self> {
            // SAFETY: the global queue accessor has no ownership transfer and accepts zero flags.
            let queue = unsafe { dispatch_get_global_queue(0, 0) };
            if queue.is_null() {
                return None;
            }
            // SAFETY: the SDK defines the memory-pressure source with an unused zero handle and the
            // three public pressure event flags. The returned source is checked before use.
            let source = unsafe {
                dispatch_source_create(
                    ptr::addr_of!(_dispatch_source_type_memorypressure),
                    0,
                    PRESSURE_MASK,
                    queue,
                )
            };
            if source.is_null() {
                return None;
            }
            let counters = Arc::new(PressureCounters {
                deliveries: AtomicU64::new(0),
                latest_mask: AtomicUsize::new(0),
                last_event_ns: AtomicU64::new(0),
            });
            let context = Box::new(PressureContext {
                source,
                counters: Arc::clone(&counters),
            });
            // SAFETY: the boxed context stays owned by the dispatch object until its finalizer. The
            // source is inactive while context and handlers are installed, then activated once.
            unsafe {
                dispatch_set_context(source, Box::into_raw(context).cast::<c_void>());
                dispatch_source_set_event_handler_f(source, Some(pressure_event));
                dispatch_set_finalizer_f(source, Some(drop_pressure_context));
                dispatch_activate(source);
            }
            Some(Self { source, counters })
        }

        fn observation(
            &self,
            captured_at: Duration,
            max_age: Duration,
        ) -> (Observed<u64>, Observed<MemoryPressureLevel>) {
            let deliveries = self.counters.deliveries.load(Ordering::Acquire);
            let mask = self.counters.latest_mask.load(Ordering::Acquire);
            let event_ns = self.counters.last_event_ns.load(Ordering::Acquire);
            let now_ns = monotonic_ns().unwrap_or(event_ns);
            pressure_observation(deliveries, mask, event_ns, now_ns, captured_at, max_age)
        }
    }

    fn pressure_observation(
        deliveries: u64,
        mask: usize,
        event_ns: u64,
        now_ns: u64,
        captured_at: Duration,
        max_age: Duration,
    ) -> (Observed<u64>, Observed<MemoryPressureLevel>) {
        if deliveries == 0 {
            return (Observed::Unknown, Observed::Unknown);
        }
        let age_ns = now_ns.saturating_sub(event_ns);
        if age_ns > duration_ns(max_age) {
            let last_seen_at_ms =
                duration_ms(captured_at.saturating_sub(Duration::from_nanos(age_ns)));
            return (
                Observed::Stale { last_seen_at_ms },
                Observed::Stale { last_seen_at_ms },
            );
        }
        let level = if mask & PRESSURE_CRITICAL != 0 {
            MemoryPressureLevel::Critical
        } else if mask & PRESSURE_WARNING != 0 {
            MemoryPressureLevel::Warning
        } else if mask & PRESSURE_NORMAL != 0 {
            MemoryPressureLevel::Normal
        } else {
            return (
                Observed::Available { value: deliveries },
                Observed::Error {
                    code: ObservationError::MalformedKernelData,
                },
            );
        };
        (
            Observed::Available { value: deliveries },
            Observed::Available { value: level },
        )
    }

    impl Drop for PressureMonitor {
        fn drop(&mut self) {
            // SAFETY: this monitor owns one activated source reference. Cancellation stops future
            // event acquisition; release schedules finalization after any in-flight handler.
            unsafe {
                dispatch_source_cancel(self.source);
                dispatch_release(self.source);
            }
        }
    }

    unsafe extern "C" fn pressure_event(context: *mut c_void) {
        if context.is_null() {
            return;
        }
        // SAFETY: dispatch passes the boxed PressureContext installed before source activation.
        let context = unsafe { &*context.cast::<PressureContext>() };
        // SAFETY: the callback runs for the live source stored in its own context.
        let mask = unsafe { dispatch_source_get_data(context.source) } & PRESSURE_MASK;
        context.counters.latest_mask.store(mask, Ordering::Release);
        context
            .counters
            .last_event_ns
            .store(monotonic_ns().unwrap_or(0), Ordering::Release);
        let _ = context.counters.deliveries.fetch_update(
            Ordering::Release,
            Ordering::Relaxed,
            |value| Some(value.saturating_add(1)),
        );
    }

    unsafe extern "C" fn drop_pressure_context(context: *mut c_void) {
        if !context.is_null() {
            // SAFETY: the finalizer runs exactly once for the pointer transferred with Box::into_raw.
            drop(unsafe { Box::from_raw(context.cast::<PressureContext>()) });
        }
    }

    fn read_swap_bytes() -> Observed<u64> {
        let mut usage = MaybeUninit::<SwapUsage>::zeroed();
        let mut length = size_of::<SwapUsage>();
        // SAFETY: the nul-terminated key is static and the output buffer/length describe SwapUsage.
        let result = unsafe {
            sysctlbyname(
                c"vm.swapusage".as_ptr(),
                usage.as_mut_ptr().cast::<c_void>(),
                &raw mut length,
                ptr::null_mut(),
                0,
            )
        };
        if result != 0 || length != size_of::<SwapUsage>() {
            return classify_system_error();
        }
        // SAFETY: sysctlbyname succeeded and returned the complete public xsw_usage structure.
        let usage = unsafe { usage.assume_init() };
        Observed::Available { value: usage.used }
    }

    fn read_vm_bytes() -> (Observed<u64>, Observed<u64>) {
        // SAFETY: mach_host_self returns a send right for the current host.
        let host = HostPort(unsafe { mach_host_self() });
        if host.0 == 0 {
            return system_error_pair();
        }
        let mut page_size = 0_usize;
        // SAFETY: page_size is writable and host is a live current-host send right.
        if unsafe { host_page_size(host.0, &raw mut page_size) } != KERN_SUCCESS {
            return system_error_pair();
        }
        let mut statistics = MaybeUninit::<VmStatistics64>::zeroed();
        let Ok(mut count) = c_uint::try_from(size_of::<VmStatistics64>() / size_of::<c_int>())
        else {
            return system_error_pair();
        };
        // SAFETY: count describes the integer-sized writable VM statistics buffer.
        if unsafe {
            host_statistics64(
                host.0,
                HOST_VM_INFO64,
                statistics.as_mut_ptr().cast::<c_int>(),
                &raw mut count,
            )
        } != KERN_SUCCESS
        {
            return system_error_pair();
        }
        let required_count = (offset_of!(VmStatistics64, compressor_page_count)
            + size_of::<c_uint>())
            / size_of::<c_int>();
        if usize::try_from(count).map_or(true, |value| value < required_count) {
            return system_error_pair();
        }
        // SAFETY: host_statistics64 returned at least through compressor_page_count.
        let statistics = unsafe { statistics.assume_init() };
        let Ok(page_size) = u64::try_from(page_size) else {
            return system_error_pair();
        };
        let compressor = u64::from(statistics.compressor_page_count).checked_mul(page_size);
        let wired = u64::from(statistics.wire_count).checked_mul(page_size);
        match (compressor, wired) {
            (Some(compressor), Some(wired)) => (
                Observed::Available { value: compressor },
                Observed::Available { value: wired },
            ),
            _ => system_error_pair(),
        }
    }

    struct HostPort(c_uint);

    impl Drop for HostPort {
        fn drop(&mut self) {
            if self.0 != 0 {
                // SAFETY: the send right came from mach_host_self and is released exactly once.
                unsafe {
                    let _ = mach_port_deallocate(mach_task_self_, self.0);
                }
            }
        }
    }

    fn system_error_pair() -> (Observed<u64>, Observed<u64>) {
        (
            Observed::Error {
                code: ObservationError::Internal,
            },
            Observed::Error {
                code: ObservationError::Internal,
            },
        )
    }

    fn classify_system_error<T>() -> Observed<T> {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM | libc::EACCES) => Observed::Error {
                code: ObservationError::PermissionDenied,
            },
            Some(libc::ENOENT | libc::ENOTSUP | libc::ENOSYS) => Observed::Unavailable {
                reason: UnavailableReason::NotSupported,
            },
            _ => Observed::Error {
                code: ObservationError::Internal,
            },
        }
    }

    fn monotonic_ns() -> Option<u64> {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: time is a valid output pointer and CLOCK_MONOTONIC requires no other state.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut time) } != 0 {
            return None;
        }
        let seconds = u64::try_from(time.tv_sec).ok()?;
        let nanos = u64::try_from(time.tv_nsec).ok()?;
        seconds.checked_mul(1_000_000_000)?.checked_add(nanos)
    }

    fn duration_ns(duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
    }

    fn duration_ms(duration: Duration) -> u64 {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    }

    #[cfg(test)]
    mod tests {
        use std::mem::{offset_of, size_of};
        use std::time::Duration;

        use super::{
            MemoryPressureLevel, Observed, PRESSURE_CRITICAL, PRESSURE_NORMAL, PRESSURE_WARNING,
            SwapUsage, VmStatistics64, pressure_observation,
        };

        #[test]
        fn public_sdk_struct_layouts_match_darwin_arm64() {
            assert_eq!(size_of::<SwapUsage>(), 32);
            assert_eq!(offset_of!(SwapUsage, used), 16);
            assert_eq!(size_of::<VmStatistics64>(), 192);
            assert_eq!(offset_of!(VmStatistics64, wire_count), 12);
            assert_eq!(offset_of!(VmStatistics64, compressor_page_count), 128);
        }

        #[test]
        fn pressure_events_stay_unknown_until_delivered_and_then_age_explicitly() {
            assert_eq!(
                pressure_observation(
                    0,
                    PRESSURE_NORMAL,
                    1,
                    1,
                    Duration::ZERO,
                    Duration::from_secs(1),
                ),
                (Observed::Unknown, Observed::Unknown)
            );
            assert_eq!(
                pressure_observation(
                    3,
                    PRESSURE_WARNING,
                    1_000_000,
                    2_000_000,
                    Duration::from_millis(50),
                    Duration::from_secs(1),
                ),
                (
                    Observed::Available { value: 3 },
                    Observed::Available {
                        value: MemoryPressureLevel::Warning,
                    },
                )
            );
            assert_eq!(
                pressure_observation(
                    4,
                    PRESSURE_NORMAL | PRESSURE_CRITICAL,
                    1_000_000,
                    5_000_000,
                    Duration::from_millis(10),
                    Duration::from_millis(1),
                ),
                (
                    Observed::Stale { last_seen_at_ms: 6 },
                    Observed::Stale { last_seen_at_ms: 6 },
                )
            );
        }
    }
}

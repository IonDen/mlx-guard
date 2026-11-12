//! When a warning-band line is due during a run.
//!
//! [`BandWatch`] reads the policy state after each sample and decides, on the sample's own time,
//! whether one of the three band lines prints. It reads no clock and does no I/O; rendering the
//! line is the caller's job.

use std::time::Duration;

use crate::PolicyState;

/// The shortest time between two `Entered` or `Still` lines, measured from the last line of any kind.
pub const BAND_LINE_INTERVAL: Duration = Duration::from_secs(300);

/// A band line that is due on the sample just observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BandEvent {
    /// This sample moved the state from `Normal` to `Warning`.
    Entered { footprint: u64, at: Duration },
    /// The state is still `Warning`; `since` is the crossing that began this episode.
    Still {
        footprint: u64,
        at: Duration,
        since: Duration,
    },
    /// The state is back to `Normal` and this episode had printed a line.
    Cleared { footprint: u64, at: Duration },
}

/// Pure rate-limited watcher of the `Normal`/`Warning` band.
///
/// A state other than `Normal` or `Warning` closes the watch for good, so no band line follows
/// the start of an intervention. A sample with no value changes nothing.
#[derive(Clone, Debug, Default)]
pub struct BandWatch {
    closed: bool,
    episode: Option<Duration>,
    announced: bool,
    last_line: Option<Duration>,
}

impl BandWatch {
    /// Create a watch that has printed nothing and is outside the band.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe the policy state after one sample and return the line due on it, if any.
    ///
    /// `at` is the sample's processing time, the same value its transition record carries.
    #[must_use]
    pub fn observe(
        &mut self,
        state: PolicyState,
        footprint: Option<u64>,
        at: Duration,
    ) -> Option<BandEvent> {
        if self.closed {
            return None;
        }
        if !matches!(state, PolicyState::Normal | PolicyState::Warning) {
            self.closed = true;
            return None;
        }
        let footprint = footprint?;
        let quiet = self
            .last_line
            .is_some_and(|last| at.saturating_sub(last) < BAND_LINE_INTERVAL);
        match (state, self.episode) {
            (PolicyState::Warning, None) => {
                self.episode = Some(at);
                self.announced = !quiet;
                if quiet {
                    return None;
                }
                self.last_line = Some(at);
                Some(BandEvent::Entered { footprint, at })
            }
            (PolicyState::Warning, Some(since)) if !quiet => {
                self.announced = true;
                self.last_line = Some(at);
                Some(BandEvent::Still {
                    footprint,
                    at,
                    since,
                })
            }
            (PolicyState::Normal, Some(_)) => {
                self.episode = None;
                if !self.announced {
                    return None;
                }
                self.last_line = Some(at);
                Some(BandEvent::Cleared { footprint, at })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{BandEvent, BandWatch};
    use crate::{Event, PolicyConfig, PolicyMachine, PolicyState, SampleEvent};

    // Thresholds used throughout, as literals: recovery 80, warning 90, limit 100, emergency 150.
    const IN_BAND: u64 = 95;
    const BETWEEN_RECOVERY_AND_WARNING: u64 = 85;
    const RECOVERED: u64 = 75;

    fn s(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn warning(watch: &mut BandWatch, footprint: u64, at: Duration) -> Option<BandEvent> {
        watch.observe(PolicyState::Warning, Some(footprint), at)
    }

    fn normal(watch: &mut BandWatch, footprint: u64, at: Duration) -> Option<BandEvent> {
        watch.observe(PolicyState::Normal, Some(footprint), at)
    }

    fn event_at(event: BandEvent) -> Duration {
        match event {
            BandEvent::Entered { at, .. }
            | BandEvent::Still { at, .. }
            | BandEvent::Cleared { at, .. } => at,
        }
    }

    fn policy_config(required_breach_samples: u32) -> PolicyConfig {
        PolicyConfig {
            limit_bytes: 100,
            warning_bytes: 90,
            recovery_bytes: 80,
            emergency_bytes: 150,
            required_breach_samples,
            max_missing_samples: 3,
            max_sample_age: ms(100),
            max_sample_window: ms(10),
            checkpoint_timeout: None,
            term_grace: ms(100),
            wall_time: None,
        }
    }

    fn sample(at: Duration, bytes: u64) -> Event {
        Event::Sample(SampleEvent {
            captured_at: at,
            processed_at: at,
            window: ms(1),
            aggregate_bytes: Some(bytes),
        })
    }

    #[test]
    fn the_first_warning_state_fires_at_once() {
        // Bug: the first line waits for the interval (last line starts at zero instead of none).
        let mut watch = BandWatch::new();
        assert_eq!(normal(&mut watch, RECOVERED, s(0)), None);
        assert_eq!(
            warning(&mut watch, IN_BAND, s(10)),
            Some(BandEvent::Entered {
                footprint: IN_BAND,
                at: s(10),
            })
        );
    }

    #[test]
    fn no_still_line_before_five_minutes_and_one_at_five() {
        // Bug: off-by-one on the interval (`<=` for `<`), or a line on every sample.
        let mut watch = BandWatch::new();
        assert!(matches!(
            warning(&mut watch, IN_BAND, s(10)),
            Some(BandEvent::Entered { .. })
        ));
        for second in 11..310 {
            assert_eq!(
                warning(&mut watch, IN_BAND, s(second)),
                None,
                "at {second} s"
            );
        }
        assert_eq!(warning(&mut watch, IN_BAND, ms(309_999)), None);
        assert_eq!(
            warning(&mut watch, IN_BAND, s(310)),
            Some(BandEvent::Still {
                footprint: IN_BAND,
                at: s(310),
                since: s(10),
            })
        );
        assert_eq!(warning(&mut watch, IN_BAND, s(311)), None);
    }

    #[test]
    fn recovery_fires_at_once_and_once() {
        // Bug: the cleared line is rate-limited away, or repeats on every recovered sample.
        let mut watch = BandWatch::new();
        assert!(warning(&mut watch, IN_BAND, s(10)).is_some());
        assert_eq!(
            normal(&mut watch, RECOVERED, s(20)),
            Some(BandEvent::Cleared {
                footprint: RECOVERED,
                at: s(20),
            })
        );
        for second in 21..1000 {
            assert_eq!(
                normal(&mut watch, RECOVERED, s(second)),
                None,
                "at {second} s"
            );
        }
    }

    #[test]
    fn a_bouncing_footprint_prints_two_lines_per_interval() {
        // Bug: re-entry bypasses the rate limit (Entered gated on the episode, not the last line).
        let mut watch = BandWatch::new();
        let mut lines = Vec::new();
        for second in 0..3600 {
            let event = if second % 2 == 0 {
                warning(&mut watch, IN_BAND, s(second))
            } else {
                normal(&mut watch, RECOVERED, s(second))
            };
            lines.extend(event.map(event_at));
        }
        assert_eq!(
            lines[..6],
            [s(0), s(1), s(302), s(303), s(604), s(605)],
            "pairs 302 s apart"
        );
        assert_eq!(lines.len(), 24);
        for &start in &lines {
            let end = start + s(300);
            let in_window = lines.iter().filter(|&&t| t >= start && t <= end).count();
            assert!(in_window <= 2, "{in_window} lines in [{start:?}, {end:?}]");
        }
    }

    #[test]
    fn a_line_due_between_recovery_and_warning_says_still_not_entered() {
        // Bug: a held-back Entered prints later, claiming "at or above the warning threshold".
        let mut watch = BandWatch::new();
        assert!(matches!(
            warning(&mut watch, IN_BAND, s(0)),
            Some(BandEvent::Entered { .. })
        ));
        assert!(matches!(
            normal(&mut watch, RECOVERED, s(1)),
            Some(BandEvent::Cleared { .. })
        ));
        assert_eq!(
            warning(&mut watch, IN_BAND, s(10)),
            None,
            "a quiet crossing"
        );
        assert_eq!(
            warning(&mut watch, BETWEEN_RECOVERY_AND_WARNING, s(301)),
            Some(BandEvent::Still {
                footprint: BETWEEN_RECOVERY_AND_WARNING,
                at: s(301),
                since: s(10),
            })
        );
    }

    #[test]
    fn an_entered_line_is_true_of_its_own_sample() {
        // Bug: Entered printed on any sample but the Normal-to-Warning crossing.
        let mut machine = PolicyMachine::enforce(policy_config(2)).unwrap();
        let mut watch = BandWatch::new();
        // A fixed, seeded sequence on a 7 s cadence over 70 min: Normal stretches (85 there holds
        // Normal) alternate with band stretches that cross at 95 or 100, hold between recovery and
        // warning, and touch the limit but never twice in a row. Stretch lengths vary, so lines
        // come due on crossings and on held samples alike.
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut pick = |bound: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from((seed >> 33) % bound).unwrap()
        };
        let mut sequence: Vec<u64> = Vec::new();
        let mut in_band = false;
        while sequence.len() < 600 {
            let length = 1 + pick(40);
            for index in 0..length {
                let bytes = match (in_band, index) {
                    (false, 0) => 76,
                    (false, _) => [70, 76, 85][pick(3)],
                    (true, 0) => [95, 100][pick(2)],
                    (true, _) => [82, 85, 88, 91, 95, 100][pick(6)],
                };
                let bytes = if bytes == 100 && sequence.last() == Some(&100) {
                    85
                } else {
                    bytes
                };
                sequence.push(bytes);
            }
            in_band = !in_band;
        }
        let mut entered = 0;
        let mut low_stills = 0;
        for (step, &bytes) in (0_u64..).zip(&sequence) {
            let at = s(7 * step);
            let before = machine.state();
            machine.apply(sample(at, bytes));
            let after = machine.state();
            assert!(
                matches!(after, PolicyState::Normal | PolicyState::Warning),
                "the sequence must stay short of intervention, got {after:?} at {at:?}"
            );
            match watch.observe(after, Some(bytes), at) {
                Some(BandEvent::Entered {
                    footprint,
                    at: line_at,
                }) => {
                    entered += 1;
                    assert!(footprint >= 90, "Entered at {footprint} bytes, at {at:?}");
                    assert_eq!(line_at, at);
                    assert_eq!(
                        (before, after),
                        (PolicyState::Normal, PolicyState::Warning),
                        "Entered at {at:?} was not a crossing"
                    );
                }
                Some(BandEvent::Still { footprint, .. }) if footprint < 90 => low_stills += 1,
                _ => {}
            }
        }
        assert!(entered >= 2, "only {entered} Entered lines");
        assert!(
            low_stills >= 1,
            "no line came due below the warning threshold"
        );
    }

    #[test]
    fn a_quiet_episode_that_ends_unannounced_prints_nothing() {
        // Bug: a "warning cleared" line for a warning nobody saw.
        let mut watch = BandWatch::new();
        assert!(matches!(
            warning(&mut watch, IN_BAND, s(0)),
            Some(BandEvent::Entered { .. })
        ));
        assert!(matches!(
            normal(&mut watch, RECOVERED, s(1)),
            Some(BandEvent::Cleared { .. })
        ));
        assert_eq!(warning(&mut watch, IN_BAND, s(10)), None);
        assert_eq!(normal(&mut watch, RECOVERED, s(20)), None);
        assert_eq!(normal(&mut watch, RECOVERED, s(400)), None);
    }

    #[test]
    fn an_intervention_state_ends_the_watch_for_good() {
        // Bug: a band line after TERM (or any state past Warning).
        for state in [
            PolicyState::CheckpointRequested,
            PolicyState::Terminating,
            PolicyState::Emergency,
            PolicyState::Exited,
            PolicyState::SupervisorError,
            PolicyState::Observe,
        ] {
            let mut watch = BandWatch::new();
            assert!(matches!(
                warning(&mut watch, IN_BAND, s(0)),
                Some(BandEvent::Entered { .. })
            ));
            assert_eq!(
                watch.observe(state, Some(IN_BAND), s(10)),
                None,
                "{state:?}"
            );
            assert_eq!(normal(&mut watch, RECOVERED, s(20)), None, "{state:?}");
            assert_eq!(warning(&mut watch, IN_BAND, s(1000)), None, "{state:?}");
            assert_eq!(normal(&mut watch, RECOVERED, s(2000)), None, "{state:?}");
        }
    }

    #[test]
    fn a_sample_with_no_value_changes_nothing() {
        // Bug: a missing reading read as a recovery (or printed as a line).
        let mut watch = BandWatch::new();
        assert!(matches!(
            warning(&mut watch, IN_BAND, s(0)),
            Some(BandEvent::Entered { .. })
        ));
        assert_eq!(watch.observe(PolicyState::Normal, None, s(10)), None);
        assert_eq!(watch.observe(PolicyState::Warning, None, s(300)), None);
        assert_eq!(
            warning(&mut watch, IN_BAND, s(301)),
            Some(BandEvent::Still {
                footprint: IN_BAND,
                at: s(301),
                since: s(0),
            })
        );
    }

    #[test]
    fn a_sample_that_starts_the_intervention_prints_no_band_line() {
        // Bug: a warning printed after the stop began (the footprint read instead of the state).
        let mut control = PolicyMachine::enforce(policy_config(2)).unwrap();
        let mut control_watch = BandWatch::new();
        control.apply(sample(s(0), 100));
        assert_eq!(control.state(), PolicyState::Warning);
        assert!(matches!(
            control_watch.observe(control.state(), Some(100), s(0)),
            Some(BandEvent::Entered { .. })
        ));

        let mut breach = PolicyMachine::enforce(policy_config(1)).unwrap();
        let mut breach_watch = BandWatch::new();
        breach.apply(sample(s(0), 70));
        assert_eq!(breach_watch.observe(breach.state(), Some(70), s(0)), None);
        breach.apply(sample(s(1), 100));
        assert_eq!(breach.state(), PolicyState::Terminating);
        assert_eq!(breach_watch.observe(breach.state(), Some(100), s(1)), None);
        assert_eq!(
            breach_watch.observe(PolicyState::Warning, Some(100), s(400)),
            None
        );

        let mut kill = PolicyMachine::enforce(policy_config(2)).unwrap();
        let mut kill_watch = BandWatch::new();
        kill.apply(sample(s(0), 150));
        assert_eq!(kill.state(), PolicyState::Emergency);
        assert_eq!(kill_watch.observe(kill.state(), Some(150), s(0)), None);
    }

    #[test]
    fn a_day_in_the_band_prints_288_lines() {
        // Bug: interval drift (each line measured from the wrong time), or unbounded volume.
        let mut watch = BandWatch::new();
        let mut lines = Vec::new();
        for second in 0..86_400 {
            lines.extend(warning(&mut watch, IN_BAND, s(second)).map(event_at));
        }
        assert_eq!(lines.len(), 288);
        assert_eq!(lines[0], s(0));
        for pair in lines.windows(2) {
            assert_eq!(pair[1], pair[0] + s(300), "gap after {:?}", pair[0]);
        }
    }

    #[test]
    fn still_says_since_the_episode_began() {
        // Bug: "since" reset by each repeat, or set when a line first prints, not at the crossing.
        let mut watch = BandWatch::new();
        assert!(warning(&mut watch, IN_BAND, s(10)).is_some());
        for at in [s(310), s(610)] {
            assert_eq!(
                warning(&mut watch, IN_BAND, at),
                Some(BandEvent::Still {
                    footprint: IN_BAND,
                    at,
                    since: s(10),
                })
            );
        }

        let mut quiet = BandWatch::new();
        assert!(warning(&mut quiet, IN_BAND, s(0)).is_some());
        assert!(normal(&mut quiet, RECOVERED, s(1)).is_some());
        assert_eq!(
            warning(&mut quiet, IN_BAND, s(20)),
            None,
            "a quiet crossing"
        );
        assert_eq!(warning(&mut quiet, IN_BAND, s(200)), None);
        assert_eq!(
            warning(&mut quiet, IN_BAND, s(301)),
            Some(BandEvent::Still {
                footprint: IN_BAND,
                at: s(301),
                since: s(20),
            })
        );
    }
}

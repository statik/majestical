//! Autopilot scheduling policy: a pure function from power state, manual
//! throttle override, and pending work count to a scheduler decision. No
//! I/O, no timers, no threads — the desktop app hosts the actual loop
//! (polling power state, reading the queue depth, calling this function on
//! each tick, and acting on the verdict). Keeping the policy pure here
//! means every branch is exhaustively testable without mocking a power
//! source or standing up a real queue.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    Ac,
    Battery,
    /// Probe unavailable (non-macOS) or unparseable output.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PowerState {
    pub source: PowerSource,
    pub low_power_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThrottleOverride {
    Auto,
    Paused,
    Low,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    Paused,
    LowPowerMode,
    NoPendingWork,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "mode", content = "hold_reason", rename_all = "snake_case")]
pub enum SchedulerDecision {
    RunFull,
    RunLow,
    Hold(HoldReason),
}

/// Decide what the indexer loop should do on this tick.
///
/// Precedence: an explicit `Paused` override always holds; with anything
/// else, an empty queue holds for `NoPendingWork`; then `Low`/`Full`
/// overrides force a mode regardless of power; and only `Auto` consults
/// the power state (low power mode holds, AC runs full, battery or an
/// unknown source runs low).
#[must_use]
pub fn autopilot_decision(
    power: PowerState,
    throttle: ThrottleOverride,
    pending_items: u64,
) -> SchedulerDecision {
    if pending_items == 0 && throttle != ThrottleOverride::Paused {
        return SchedulerDecision::Hold(HoldReason::NoPendingWork);
    }
    match throttle {
        ThrottleOverride::Paused => SchedulerDecision::Hold(HoldReason::Paused),
        ThrottleOverride::Low => SchedulerDecision::RunLow,
        ThrottleOverride::Full => SchedulerDecision::RunFull,
        ThrottleOverride::Auto => {
            if power.low_power_mode {
                return SchedulerDecision::Hold(HoldReason::LowPowerMode);
            }
            match power.source {
                PowerSource::Ac => SchedulerDecision::RunFull,
                PowerSource::Battery | PowerSource::Unknown => SchedulerDecision::RunLow,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn state(source: PowerSource, low_power_mode: bool) -> PowerState {
        PowerState {
            source,
            low_power_mode,
        }
    }

    /// `any::<u64>()` practically never draws 0, so the zero-pending
    /// branch would go unexercised by the properties without this bias.
    fn pending_items_strategy() -> impl Strategy<Value = u64> {
        prop_oneof![Just(0u64), any::<u64>()]
    }

    fn power_source_strategy() -> impl Strategy<Value = PowerSource> {
        prop_oneof![
            Just(PowerSource::Ac),
            Just(PowerSource::Battery),
            Just(PowerSource::Unknown),
        ]
    }

    #[test]
    fn paused_wins_over_ac_with_pending_work() {
        let decision =
            autopilot_decision(state(PowerSource::Ac, false), ThrottleOverride::Paused, 5);
        assert_eq!(decision, SchedulerDecision::Hold(HoldReason::Paused));
    }

    #[test]
    fn low_override_ignores_power_including_low_power_mode() {
        let decision =
            autopilot_decision(state(PowerSource::Battery, true), ThrottleOverride::Low, 5);
        assert_eq!(decision, SchedulerDecision::RunLow);
    }

    #[test]
    fn full_override_ignores_power_including_low_power_mode() {
        let decision =
            autopilot_decision(state(PowerSource::Battery, true), ThrottleOverride::Full, 5);
        assert_eq!(decision, SchedulerDecision::RunFull);
    }

    #[test]
    fn auto_on_ac_runs_full() {
        let decision = autopilot_decision(state(PowerSource::Ac, false), ThrottleOverride::Auto, 5);
        assert_eq!(decision, SchedulerDecision::RunFull);
    }

    #[test]
    fn auto_on_battery_runs_low() {
        let decision = autopilot_decision(
            state(PowerSource::Battery, false),
            ThrottleOverride::Auto,
            5,
        );
        assert_eq!(decision, SchedulerDecision::RunLow);
    }

    #[test]
    fn auto_on_unknown_source_runs_low() {
        let decision = autopilot_decision(
            state(PowerSource::Unknown, false),
            ThrottleOverride::Auto,
            5,
        );
        assert_eq!(decision, SchedulerDecision::RunLow);
    }

    #[test]
    fn auto_in_low_power_mode_holds_regardless_of_source() {
        for source in [PowerSource::Ac, PowerSource::Battery, PowerSource::Unknown] {
            let decision = autopilot_decision(state(source, true), ThrottleOverride::Auto, 5);
            assert_eq!(decision, SchedulerDecision::Hold(HoldReason::LowPowerMode));
        }
    }

    #[test]
    fn zero_pending_holds_for_every_non_paused_throttle() {
        for throttle in [
            ThrottleOverride::Auto,
            ThrottleOverride::Low,
            ThrottleOverride::Full,
        ] {
            let decision = autopilot_decision(state(PowerSource::Ac, false), throttle, 0);
            assert_eq!(decision, SchedulerDecision::Hold(HoldReason::NoPendingWork));
        }
    }

    #[test]
    fn zero_pending_and_paused_reports_paused_not_no_work() {
        let decision =
            autopilot_decision(state(PowerSource::Ac, false), ThrottleOverride::Paused, 0);
        assert_eq!(decision, SchedulerDecision::Hold(HoldReason::Paused));
    }

    proptest! {
        #[test]
        fn paused_always_holds_paused(
            source in power_source_strategy(),
            low_power_mode: bool,
            pending_items in pending_items_strategy(),
        ) {
            let decision = autopilot_decision(
                state(source, low_power_mode),
                ThrottleOverride::Paused,
                pending_items,
            );
            prop_assert_eq!(decision, SchedulerDecision::Hold(HoldReason::Paused));
        }

        #[test]
        fn non_auto_throttle_never_holds_for_low_power_mode(
            source in power_source_strategy(),
            low_power_mode: bool,
            throttle in prop_oneof![
                Just(ThrottleOverride::Paused),
                Just(ThrottleOverride::Low),
                Just(ThrottleOverride::Full),
            ],
            pending_items in pending_items_strategy(),
        ) {
            let decision = autopilot_decision(state(source, low_power_mode), throttle, pending_items);
            prop_assert_ne!(decision, SchedulerDecision::Hold(HoldReason::LowPowerMode));
        }
    }

    #[test]
    fn wire_shape_hold_paused() {
        let value = serde_json::to_value(SchedulerDecision::Hold(HoldReason::Paused)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"mode": "hold", "hold_reason": "paused"})
        );
    }

    #[test]
    fn wire_shape_run_full() {
        let value = serde_json::to_value(SchedulerDecision::RunFull).unwrap();
        assert_eq!(value, serde_json::json!({"mode": "run_full"}));
    }

    #[test]
    fn wire_shape_throttle_override_auto() {
        let value = serde_json::to_value(ThrottleOverride::Auto).unwrap();
        assert_eq!(value, serde_json::json!("auto"));
    }

    #[test]
    fn throttle_override_round_trips() {
        for throttle in [
            ThrottleOverride::Auto,
            ThrottleOverride::Paused,
            ThrottleOverride::Low,
            ThrottleOverride::Full,
        ] {
            let value = serde_json::to_value(throttle).unwrap();
            let back: ThrottleOverride = serde_json::from_value(value).unwrap();
            assert_eq!(back, throttle);
        }
    }

    #[test]
    fn power_source_round_trips() {
        for source in [PowerSource::Ac, PowerSource::Battery, PowerSource::Unknown] {
            let value = serde_json::to_value(source).unwrap();
            let back: PowerSource = serde_json::from_value(value).unwrap();
            assert_eq!(back, source);
        }
    }
}

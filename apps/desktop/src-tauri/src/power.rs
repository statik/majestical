//! The `pmset` power probe: pure parsers over captured `pmset` output, plus
//! the two-line macOS shell-out that feeds them. `majestical_services::autopilot`
//! consumes the resulting [`PowerState`] as one of its scheduler decision
//! inputs; this module owns nothing about scheduling policy, only "what does
//! this Mac's power state look like right now."
use majestical_services::autopilot::{PowerSource, PowerState};

/// True where the platform has a power probe at all. `pmset` is macOS-only;
/// every other target gets the conservative `Unknown`/`false` state from
/// [`read_power_state`] without ever spawning a process.
pub const POWER_PROBE_AVAILABLE: bool = cfg!(target_os = "macos");

/// Parses `pmset -g batt` output. The first line names the drawing source —
/// `"Now drawing from 'AC Power'"` or `"...'Battery Power'"` — anything else
/// (a future macOS rewording, or no output at all) degrades to `Unknown`
/// rather than guessing wrong.
#[must_use]
pub fn parse_power_source(batt_output: &str) -> PowerSource {
    if batt_output.contains("'AC Power'") {
        PowerSource::Ac
    } else if batt_output.contains("'Battery Power'") {
        PowerSource::Battery
    } else {
        PowerSource::Unknown
    }
}

/// Parses `pmset -g` output for Low Power Mode. Apple Silicon reports a
/// `powermode` line (0 automatic, 1 low power, 2 high power); Intel Macs
/// that support Low Power Mode report `lowpowermode` (0 or 1) instead. A
/// trimmed line whose first token is either key and whose last token is `1`
/// is low power; both keys' `0` (and `powermode 2`) and the case where
/// neither line appears are `false`.
#[must_use]
pub fn parse_low_power_mode(pmset_output: &str) -> bool {
    for line in pmset_output.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let (Some(&first), Some(&last)) = (tokens.first(), tokens.last()) else {
            continue;
        };
        if (first == "lowpowermode" || first == "powermode") && last == "1" {
            return true;
        }
    }
    false
}

/// Reads the live power state by shelling out to `pmset -g batt` and
/// `pmset -g`. Any spawn or decode failure on either call falls back to
/// `PowerSource::Unknown` / `false` — the same conservative state a
/// non-macOS build reports — logged via `tracing::warn!` rather than
/// surfaced to the caller, since a scheduler tick has no user-facing error
/// path for "couldn't read power state."
#[cfg(target_os = "macos")]
#[must_use]
pub fn read_power_state() -> PowerState {
    let source = run_pmset(&["-g", "batt"])
        .map_or(PowerSource::Unknown, |output| parse_power_source(&output));
    let low_power_mode = run_pmset(&["-g"]).is_some_and(|output| parse_low_power_mode(&output));
    PowerState {
        source,
        low_power_mode,
    }
}

#[cfg(target_os = "macos")]
fn run_pmset(args: &[&str]) -> Option<String> {
    match std::process::Command::new("pmset").args(args).output() {
        Ok(output) => match String::from_utf8(output.stdout) {
            Ok(text) => Some(text),
            Err(error) => {
                tracing::warn!(?args, %error, "pmset output was not valid UTF-8");
                None
            }
        },
        Err(error) => {
            tracing::warn!(?args, %error, "failed to spawn pmset");
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn read_power_state() -> PowerState {
    PowerState {
        source: PowerSource::Unknown,
        low_power_mode: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured on macOS 26.6.2 (build 25G83), Apple M1 Max, 2026-09-05.
    const AC_BATT_OUTPUT: &str = "Now drawing from 'AC Power'\n \
        -InternalBattery-0 (id=9240675)\t100%; charged; 0:00 remaining present: true\n";

    const BATTERY_BATT_OUTPUT: &str = "Now drawing from 'Battery Power'\n \
        -InternalBattery-0 (id=9240675)\t87%; discharging; 3:14 remaining present: true\n";

    // Captured on macOS 26.6.2, Apple Silicon (Apple M1 Max), 2026-09-05 —
    // note the `powermode` key; there is no `lowpowermode` line on this
    // machine at all (see the AMENDED note in the phase 7E plan, Task 11).
    const PMSET_G_POWERMODE_0: &str = "System-wide power settings:\n\
        Currently in use:\n \
        standby              1\n \
        powernap             0\n \
        powermode            0\n \
        womp                 1\n";

    const PMSET_G_POWERMODE_1: &str = "System-wide power settings:\n\
        Currently in use:\n \
        standby              1\n \
        powernap             0\n \
        powermode            1\n \
        womp                 1\n";

    const PMSET_G_POWERMODE_2: &str = "System-wide power settings:\n\
        Currently in use:\n \
        standby              1\n \
        powernap             0\n \
        powermode            2\n \
        womp                 1\n";

    // `lowpowermode` literal per Apple's documented Intel `pmset -g` output
    // (not captured on this Apple Silicon dev machine, which has no such
    // line at all).
    const PMSET_G_LOWPOWERMODE_0: &str = "System-wide power settings:\n\
        Currently in use:\n \
        lowpowermode          0\n \
        womp                  1\n";

    const PMSET_G_LOWPOWERMODE_1: &str = "System-wide power settings:\n\
        Currently in use:\n \
        lowpowermode          1\n \
        womp                  1\n";

    const PMSET_G_NEITHER_KEY: &str = "System-wide power settings:\n\
        Currently in use:\n \
        standby               1\n \
        womp                  1\n";

    #[test]
    fn parses_ac_power_source() {
        assert_eq!(parse_power_source(AC_BATT_OUTPUT), PowerSource::Ac);
    }

    #[test]
    fn parses_battery_power_source() {
        assert_eq!(
            parse_power_source(BATTERY_BATT_OUTPUT),
            PowerSource::Battery
        );
    }

    #[test]
    fn garbled_batt_output_is_unknown_source() {
        assert_eq!(
            parse_power_source("not pmset output at all"),
            PowerSource::Unknown
        );
    }

    #[test]
    fn empty_batt_output_is_unknown_source() {
        assert_eq!(parse_power_source(""), PowerSource::Unknown);
    }

    #[test]
    fn powermode_1_is_low_power_mode() {
        assert!(parse_low_power_mode(PMSET_G_POWERMODE_1));
    }

    #[test]
    fn powermode_0_is_not_low_power_mode() {
        assert!(!parse_low_power_mode(PMSET_G_POWERMODE_0));
    }

    #[test]
    fn powermode_2_is_not_low_power_mode() {
        assert!(!parse_low_power_mode(PMSET_G_POWERMODE_2));
    }

    #[test]
    fn lowpowermode_1_is_low_power_mode() {
        assert!(parse_low_power_mode(PMSET_G_LOWPOWERMODE_1));
    }

    #[test]
    fn lowpowermode_0_is_not_low_power_mode() {
        assert!(!parse_low_power_mode(PMSET_G_LOWPOWERMODE_0));
    }

    #[test]
    fn neither_key_present_is_not_low_power_mode() {
        assert!(!parse_low_power_mode(PMSET_G_NEITHER_KEY));
    }

    #[test]
    fn empty_pmset_g_output_is_not_low_power_mode() {
        assert!(!parse_low_power_mode(""));
    }

    /// macOS-only smoke test: asserts the real probe returns a known power
    /// source on this machine. Gated to macOS because `pmset` does not
    /// exist elsewhere — the coverage gap this leaves is real: non-macOS
    /// platforms exercise only the `Unknown`/`false` constant-return branch
    /// of `read_power_state`, never a live process spawn, anywhere in this
    /// suite (the 7C rule: gate and gap recorded together).
    #[cfg(target_os = "macos")]
    #[test]
    fn read_power_state_returns_a_known_source_on_macos() {
        assert_ne!(read_power_state().source, PowerSource::Unknown);
    }
}

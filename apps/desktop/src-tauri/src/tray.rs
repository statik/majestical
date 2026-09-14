//! The menu-bar tray: icon, status menu, and the hide-to-tray window
//! behavior in `lib.rs`'s `on_window_event`.
//!
//! `menu_model` is a pure function from the scheduler's shared state, plus
//! whether a catalog is selected (which `SchedulerStateOutcome` cannot
//! distinguish from "not ticked yet" — see the Task 14 amendment in
//! `docs/superpowers/plans/2026-08-26-phase7e-alwayson-e2e-doctor.md`), to a
//! plain [`MenuModel`]. Tauri's menu objects are built FROM that model in
//! `build_menu`, a thin untested shim — the same philosophy `commands.rs`
//! follows for its `*_impl` functions: logic stays testable, the Tauri glue
//! does not.
use crate::commands::{AppState, selected_catalog};
use crate::indexer::{SchedulerShared, SchedulerState, set_throttle_impl};
use majestical_services::autopilot::{
    HoldReason, PowerSource, SchedulerDecision, ThrottleOverride,
};
use std::sync::PoisonError;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};

/// The tray icon's id — `refresh` looks the tray back up by this id to
/// rebuild its menu.
const TRAY_ID: &str = "main";

/// The Tauri event `App.svelte` listens for to select the Settings surface,
/// fired by "Health…" and the attention line.
const NAVIGATE_SETTINGS_EVENT: &str = "navigate-settings";

/// The three looks the tray icon can have. `MenuModel::attention_tint`
/// layers an attention indicator over any of the three when the last batch
/// failed — see the mockup's icon table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIcon {
    Idle,
    Indexing,
    Paused,
}

/// What the tray menu shows, computed from the scheduler's shared state and
/// whether a catalog is selected. Pure and no-I/O — see the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuModel {
    /// The status line(s) at the top of the menu: one line normally, two
    /// when a run-low names its reason ("On battery"/"Power source
    /// unknown") or a low-power-mode hold has pending work to report.
    pub status_lines: Vec<String>,
    /// "Last batch failed — open Health…", present iff `last_error` is.
    pub attention: Option<String>,
    /// The checked radio item.
    pub throttle: ThrottleOverride,
    /// Whether the radio group responds to clicks — false with no catalog
    /// selected, since there is nothing to throttle.
    pub throttle_enabled: bool,
    pub icon: TrayIcon,
    /// Whether the icon carries the attention tint (`last_error` present).
    pub attention_tint: bool,
}

/// "1 item pending" / "{n} items pending" — the tray's copy is new, so it
/// pluralizes correctly from day one rather than copying the app's older
/// "1 items" pattern (see the mockup's pluralization note).
fn pending_line(n: u64) -> String {
    if n == 1 {
        "1 item pending".to_string()
    } else {
        format!("{n} items pending")
    }
}

/// Computes the tray menu's model. `catalog_selected` is the wire gap the
/// Task 14 amendment resolves by taking it as a second input read from
/// `AppState` at rebuild time, rather than adding a field to
/// `SchedulerStateOutcome`. See `tray-menu.html`'s "Status line ← wire
/// mapping" table for the exact strings this pins.
#[must_use]
pub fn menu_model(state: &SchedulerShared, catalog_selected: bool) -> MenuModel {
    let attention = state
        .last_error
        .as_ref()
        .map(|_| "Last batch failed — open Health…".to_string());
    let attention_tint = state.last_error.is_some();

    if !catalog_selected {
        return MenuModel {
            status_lines: vec!["No catalog selected".to_string()],
            attention,
            throttle: state.throttle,
            throttle_enabled: false,
            icon: TrayIcon::Idle,
            attention_tint,
        };
    }

    let (status_lines, icon) = match state.last_decision {
        None => (vec!["Starting…".to_string()], TrayIcon::Idle),
        Some(SchedulerDecision::Hold(HoldReason::NoPendingWork)) => {
            (vec!["Idle".to_string()], TrayIcon::Idle)
        }
        Some(SchedulerDecision::RunFull) => (
            vec![format!("Indexing — {}", pending_line(state.pending_items))],
            TrayIcon::Indexing,
        ),
        Some(SchedulerDecision::RunLow) => {
            let mut lines = vec![format!(
                "Indexing slowly — {}",
                pending_line(state.pending_items)
            )];
            // Only Auto names why it chose Low; under the Low override the
            // second line would be redundant with the operator's own choice.
            if state.throttle == ThrottleOverride::Auto {
                match state.power.source {
                    PowerSource::Battery => lines.push("On battery".to_string()),
                    PowerSource::Unknown => lines.push("Power source unknown".to_string()),
                    PowerSource::Ac => {}
                }
            }
            (lines, TrayIcon::Indexing)
        }
        Some(SchedulerDecision::Hold(HoldReason::Paused)) => {
            // Pending count deliberately omitted: the user asked for quiet.
            (vec!["Paused".to_string()], TrayIcon::Paused)
        }
        Some(SchedulerDecision::Hold(HoldReason::LowPowerMode)) => {
            let mut lines = vec!["Paused (Low Power Mode)".to_string()];
            if state.pending_items > 0 {
                lines.push(pending_line(state.pending_items));
            }
            (lines, TrayIcon::Paused)
        }
    };

    MenuModel {
        status_lines,
        attention,
        throttle: state.throttle,
        throttle_enabled: true,
        icon,
        attention_tint,
    }
}

/// Builds the Tauri menu the tray shows from a [`MenuModel`]. Thin and
/// untested — [`menu_model`] carries the logic. Status lines are
/// non-interactive; the attention line, when present, is clickable and does
/// the same thing "Health…" does.
fn build_menu(app: &AppHandle, model: &MenuModel) -> tauri::Result<Menu<tauri::Wry>> {
    let mut builder = MenuBuilder::new(app);
    for (i, line) in model.status_lines.iter().enumerate() {
        builder = builder.item(
            &MenuItemBuilder::new(line)
                .id(format!("status-{i}"))
                .enabled(false)
                .build(app)?,
        );
    }
    if let Some(attention) = &model.attention {
        builder = builder.item(&MenuItemBuilder::new(attention).id("attention").build(app)?);
    }
    builder = builder.separator();
    for (id, label, throttle) in [
        ("throttle-auto", "Auto", ThrottleOverride::Auto),
        ("throttle-paused", "Paused", ThrottleOverride::Paused),
        ("throttle-low", "Low", ThrottleOverride::Low),
        ("throttle-full", "Full", ThrottleOverride::Full),
    ] {
        builder = builder.item(
            &CheckMenuItemBuilder::new(label)
                .id(id)
                .checked(model.throttle == throttle)
                .enabled(model.throttle_enabled)
                .build(app)?,
        );
    }
    builder
        .separator()
        .item(
            &MenuItemBuilder::new("Open Majestical")
                .id("open")
                .build(app)?,
        )
        .item(&MenuItemBuilder::new("Health…").id("health").build(app)?)
        .separator()
        .item(
            &MenuItemBuilder::new("Quit Majestical")
                .id("quit")
                .build(app)?,
        )
        .build()
}

/// Shows and focuses the main window, restoring the Dock icon on macOS —
/// what "Open Majestical" and the tray's left click both do, and the first
/// step "Health…" and the attention line take before navigating.
fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
}

/// Applies a throttle override the same way `set_throttle` does, then
/// rebuilds the menu so the checked radio moves immediately rather than
/// waiting for the next tick.
fn apply_throttle(app: &AppHandle, throttle: ThrottleOverride) {
    let state = app.state::<SchedulerState>();
    let _ = set_throttle_impl(&state, throttle);
    refresh(app);
}

fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        "throttle-auto" => apply_throttle(app, ThrottleOverride::Auto),
        "throttle-paused" => apply_throttle(app, ThrottleOverride::Paused),
        "throttle-low" => apply_throttle(app, ThrottleOverride::Low),
        "throttle-full" => apply_throttle(app, ThrottleOverride::Full),
        "open" => show_window(app),
        "health" | "attention" => {
            show_window(app);
            let _ = app.emit(NAVIGATE_SETTINGS_EVENT, ());
        }
        // The scheduler never holds a transaction open across a tick: a
        // batch is one per-kind-capped run (`indexer::BATCH_LIMIT` items per
        // kind, seconds to a minute), and the loop sleeps between batches —
        // there is nothing an immediate exit here could tear.
        "quit" => app.exit(0),
        _ => {}
    }
}

/// Builds the tray icon and its initial menu. Called once from `setup`.
///
/// # Errors
/// Returns an error if the app ships no default window icon, or if the
/// Tauri runtime refuses to build the menu or the tray icon itself.
pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let model = current_model(app);
    let menu = build_menu(app, &model)?;
    let Some(icon) = app.default_window_icon().cloned() else {
        return Err(tauri::Error::AssetNotFound(
            "no default window icon configured for the tray".to_string(),
        ));
    };
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .menu(&menu)
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()))
        // A left click already drops the menu down (Tauri's default); this
        // also shows and focuses the main window underneath it, the same
        // as clicking "Open Majestical" — so bringing the app forward does
        // not require finding that item in the menu first.
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Reads the scheduler's shared state and whether a catalog is selected,
/// and computes this instant's [`MenuModel`].
fn current_model(app: &AppHandle) -> MenuModel {
    let scheduler = app.state::<SchedulerState>();
    let app_state = app.state::<AppState>();
    let catalog_selected = selected_catalog(&app_state).is_some();
    let shared = scheduler.0.read().unwrap_or_else(PoisonError::into_inner);
    menu_model(&shared, catalog_selected)
}

/// Rebuilds the tray's menu from the scheduler's current state. Called at
/// the end of every scheduler tick and right after a throttle change, so
/// the status line is at most one tick stale.
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let model = current_model(app);
    if let Ok(menu) = build_menu(app, &model) {
        let _ = tray.set_menu(Some(menu));
    }
}

#[cfg(test)]
mod tests {
    use super::{MenuModel, SchedulerShared, TrayIcon, menu_model};
    use majestical_services::autopilot::{
        HoldReason, PowerSource, PowerState, SchedulerDecision, ThrottleOverride,
    };

    fn power(source: PowerSource) -> PowerState {
        PowerState {
            source,
            low_power_mode: false,
        }
    }

    fn shared(
        throttle: ThrottleOverride,
        decision: Option<SchedulerDecision>,
        source: PowerSource,
        pending_items: u64,
        last_error: Option<&str>,
    ) -> SchedulerShared {
        SchedulerShared {
            throttle,
            last_decision: decision,
            power: power(source),
            pending_items,
            running: false,
            last_error: last_error.map(str::to_string),
        }
    }

    #[test]
    fn status_is_starting_before_the_first_tick() {
        let state = shared(ThrottleOverride::Auto, None, PowerSource::Ac, 0, None);
        let model = menu_model(&state, true);
        assert_eq!(model.status_lines, vec!["Starting…".to_string()]);
        assert_eq!(model.icon, TrayIcon::Idle);
        assert!(model.throttle_enabled);
    }

    #[test]
    fn status_is_idle_on_no_pending_work() {
        let state = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::Hold(HoldReason::NoPendingWork)),
            PowerSource::Ac,
            0,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(model.status_lines, vec!["Idle".to_string()]);
        assert_eq!(model.icon, TrayIcon::Idle);
    }

    #[test]
    fn run_full_shows_indexing_with_the_pending_count() {
        let state = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::RunFull),
            PowerSource::Ac,
            214,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(
            model.status_lines,
            vec!["Indexing — 214 items pending".to_string()]
        );
        assert_eq!(model.icon, TrayIcon::Indexing);
    }

    #[test]
    fn run_low_under_auto_on_battery_names_the_reason() {
        let state = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::RunLow),
            PowerSource::Battery,
            214,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(
            model.status_lines,
            vec![
                "Indexing slowly — 214 items pending".to_string(),
                "On battery".to_string(),
            ]
        );
        assert_eq!(model.icon, TrayIcon::Indexing);
    }

    #[test]
    fn run_low_under_auto_on_unknown_power_names_the_reason() {
        let state = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::RunLow),
            PowerSource::Unknown,
            214,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(
            model.status_lines,
            vec![
                "Indexing slowly — 214 items pending".to_string(),
                "Power source unknown".to_string(),
            ]
        );
    }

    #[test]
    fn run_low_under_the_low_override_omits_the_second_line() {
        let state = shared(
            ThrottleOverride::Low,
            Some(SchedulerDecision::RunLow),
            PowerSource::Battery,
            214,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(
            model.status_lines,
            vec!["Indexing slowly — 214 items pending".to_string()]
        );
    }

    #[test]
    fn paused_by_you_hides_the_pending_count() {
        let state = shared(
            ThrottleOverride::Paused,
            Some(SchedulerDecision::Hold(HoldReason::Paused)),
            PowerSource::Ac,
            214,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(model.status_lines, vec!["Paused".to_string()]);
        assert_eq!(model.icon, TrayIcon::Paused);
    }

    #[test]
    fn low_power_mode_hold_shows_pending_count_when_nonzero() {
        let state = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::Hold(HoldReason::LowPowerMode)),
            PowerSource::Battery,
            214,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(
            model.status_lines,
            vec![
                "Paused (Low Power Mode)".to_string(),
                "214 items pending".to_string(),
            ]
        );
        assert_eq!(model.icon, TrayIcon::Paused);
    }

    #[test]
    fn low_power_mode_hold_omits_the_second_line_when_nothing_is_pending() {
        let state = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::Hold(HoldReason::LowPowerMode)),
            PowerSource::Battery,
            0,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(
            model.status_lines,
            vec!["Paused (Low Power Mode)".to_string()]
        );
    }

    #[test]
    fn no_catalog_selected_disables_the_throttle_group() {
        let state = shared(ThrottleOverride::Auto, None, PowerSource::Ac, 0, None);
        let model = menu_model(&state, false);
        assert_eq!(model.status_lines, vec!["No catalog selected".to_string()]);
        assert!(!model.throttle_enabled);
        assert_eq!(model.icon, TrayIcon::Idle);
    }

    #[test]
    fn pending_count_pluralizes_one_item_singular() {
        let state = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::RunFull),
            PowerSource::Ac,
            1,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(
            model.status_lines,
            vec!["Indexing — 1 item pending".to_string()]
        );
    }

    #[test]
    fn pending_count_pluralizes_two_items_plural() {
        let state = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::RunFull),
            PowerSource::Ac,
            2,
            None,
        );
        let model = menu_model(&state, true);
        assert_eq!(
            model.status_lines,
            vec!["Indexing — 2 items pending".to_string()]
        );
    }

    #[test]
    fn attention_line_is_present_iff_last_error_is() {
        let failed = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::RunFull),
            PowerSource::Ac,
            214,
            Some("disk full"),
        );
        let model = menu_model(&failed, true);
        assert_eq!(
            model.attention,
            Some("Last batch failed — open Health…".to_string())
        );
        assert!(model.attention_tint);

        let clean = shared(
            ThrottleOverride::Auto,
            Some(SchedulerDecision::RunFull),
            PowerSource::Ac,
            214,
            None,
        );
        let model = menu_model(&clean, true);
        assert_eq!(model.attention, None);
        assert!(!model.attention_tint);
    }

    #[test]
    fn checked_radio_reflects_the_current_throttle() {
        for throttle in [
            ThrottleOverride::Auto,
            ThrottleOverride::Paused,
            ThrottleOverride::Low,
            ThrottleOverride::Full,
        ] {
            let state = shared(throttle, None, PowerSource::Ac, 0, None);
            let model: MenuModel = menu_model(&state, true);
            assert_eq!(model.throttle, throttle);
        }
    }
}

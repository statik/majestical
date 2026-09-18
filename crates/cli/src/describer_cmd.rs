//! `maj describer set|show|test` — per-machine backend configuration.
//! Compute for all three lives in `majestical_services::describer_config`;
//! this module only reads the API-key env var and renders.

use std::path::Path;

use majestical_services::describer_config::{
    self, DescriberConfigView, DescriberProbe, KeyCheck, SetArgs,
};
use majestical_services::notices::Notices;

pub(crate) fn env_api_key() -> Option<String> {
    std::env::var(majestical_describe::config::OPENROUTER_KEY_ENV)
        .ok()
        .filter(|k| !k.is_empty())
}

pub(crate) fn cmd_set(catalog_root: &Path, args: &SetArgs) -> anyhow::Result<()> {
    let notices = Notices::new();
    let view = describer_config::set(catalog_root, args, &notices);
    crate::drain_notices(&notices);
    let view = view?;
    print_view(&view);
    Ok(())
}

pub(crate) fn cmd_show(catalog_root: &Path) -> anyhow::Result<()> {
    let notices = Notices::new();
    let shown = describer_config::show(catalog_root, &notices);
    crate::drain_notices(&notices);
    match shown? {
        Some(view) => print_view(&view),
        None => println!(
            "no describer configured — run `maj describer set --backend <ollama|lm-studio|open-router> --model <model>`"
        ),
    }
    Ok(())
}

pub(crate) fn cmd_test(catalog_root: &Path) -> anyhow::Result<()> {
    let notices = Notices::new();
    let probe = describer_config::test(catalog_root, env_api_key(), &notices);
    crate::drain_notices(&notices);
    let probe = probe?;
    println!("backend reachable: yes");
    println!(
        "model {} listed: {}",
        probe.model,
        if probe.model_listed {
            "yes"
        } else {
            "NO — check the model name"
        }
    );
    match probe.vision {
        Some(true) => println!("vision capability: yes"),
        Some(false) => {
            println!("vision capability: NO — caption work will not run with this model");
        }
        None => println!("vision capability: unknown (reported by LM Studio only)"),
    }
    if let Some(line) = key_line(probe.key) {
        println!("{line}");
    }
    if will_run(&probe) {
        println!("caption and tag-suggestion work will run on the next `maj index run`");
    }
    Ok(())
}

/// The key line of `describer test`, or `None` when the key was not checked
/// (a notice already said so if the check was attempted and failed).
fn key_line(key: KeyCheck) -> Option<&'static str> {
    match key {
        KeyCheck::Accepted => Some("key: accepted"),
        KeyCheck::Rejected => Some("key: REJECTED — OpenRouter answered 401; set a new key"),
        KeyCheck::NotChecked => None,
    }
}

/// Whether `describer test` may promise caption work: every line above the
/// promise has to have been good news, the key's included.
fn will_run(probe: &DescriberProbe) -> bool {
    let key_usable = match probe.key {
        KeyCheck::Accepted | KeyCheck::NotChecked => true,
        KeyCheck::Rejected => false,
    };
    probe.model_listed && probe.vision != Some(false) && key_usable
}

fn print_view(view: &DescriberConfigView) {
    println!("backend:  {}", view.backend);
    println!("base-url: {}", view.base_url);
    println!("model:    {}", view.model);
    match &view.api_key {
        Some(_) => println!("api-key:  (redacted)"),
        None => println!("api-key:  (none)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key that was not checked says nothing, rather than something that
    /// reads as a verdict.
    #[test]
    fn key_line_speaks_only_for_a_checked_key() {
        assert_eq!(key_line(KeyCheck::Accepted), Some("key: accepted"));
        assert_eq!(
            key_line(KeyCheck::Rejected),
            Some("key: REJECTED — OpenRouter answered 401; set a new key")
        );
        assert_eq!(key_line(KeyCheck::NotChecked), None);
    }

    fn probe(model_listed: bool, vision: Option<bool>, key: KeyCheck) -> DescriberProbe {
        DescriberProbe {
            model: "m".to_string(),
            model_listed,
            vision,
            key,
        }
    }

    /// The closing line is a promise, so anything the lines above it
    /// reported as broken — a rejected key included — must withhold it.
    #[test]
    fn will_run_only_when_nothing_above_it_said_no() {
        for (case, probe, expected) in [
            (
                "all good",
                probe(true, Some(true), KeyCheck::Accepted),
                true,
            ),
            (
                "model not listed",
                probe(false, Some(true), KeyCheck::Accepted),
                false,
            ),
            (
                "no vision",
                probe(true, Some(false), KeyCheck::Accepted),
                false,
            ),
            ("key rejected", probe(true, None, KeyCheck::Rejected), false),
            (
                "key not checked",
                probe(true, None, KeyCheck::NotChecked),
                true,
            ),
        ] {
            assert_eq!(will_run(&probe), expected, "{case}");
        }
    }
}

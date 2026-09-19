//! `maj describer set|show|test` — per-machine backend configuration.
//! Compute for all three lives in `majestical_services::describer_config`;
//! this module only reads the API-key env var and renders.

use std::path::Path;

use majestical_services::describer_config::{
    self, DescriberConfigView, DescriberProbe, KeyCheck, KeyPresence, KeySource, SetArgs,
};
use majestical_services::notices::Notices;

pub(crate) fn env_api_key() -> Option<String> {
    std::env::var(majestical_describe::config::OPENROUTER_KEY_ENV)
        .ok()
        .filter(|k| !k.is_empty())
}

/// What this head found outside `describer.toml`, for the views and the
/// doctor row that name the key's source.
pub(crate) fn key_presence() -> KeyPresence {
    KeyPresence {
        env: env_api_key().is_some(),
        keychain: false,
    }
}

pub(crate) fn cmd_set(catalog_root: &Path, args: &SetArgs) -> anyhow::Result<()> {
    let notices = Notices::new();
    let stored = describer_config::set(catalog_root, args, &notices);
    crate::drain_notices(&notices);
    stored?;
    cmd_show(catalog_root)
}

pub(crate) fn cmd_show(catalog_root: &Path) -> anyhow::Result<()> {
    let notices = Notices::new();
    let shown = describer_config::show(catalog_root, key_presence(), &notices);
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
        KeyCheck::Missing => Some(
            "key: MISSING — save one in Settings → Captions, or set it with \
             `maj describer set --api-key` or MAJ_OPENROUTER_KEY",
        ),
        KeyCheck::NotChecked => None,
    }
}

/// Whether `describer test` may promise caption work. Three things withhold
/// the promise: the backend does not list the model, the model reports no
/// vision support, or `OpenRouter`'s key is rejected or missing — each one
/// fails every caption item on the next pass. A key that was not checked
/// withholds nothing: local backends need none, and an `OpenRouter` key
/// endpoint that judged nothing says nothing against the key.
fn will_run(probe: &DescriberProbe) -> bool {
    let key_usable = match probe.key {
        KeyCheck::Accepted | KeyCheck::NotChecked => true,
        KeyCheck::Rejected | KeyCheck::Missing => false,
    };
    probe.model_listed && probe.vision != Some(false) && key_usable
}

fn print_view(view: &DescriberConfigView) {
    println!("backend:  {}", view.backend);
    println!("base-url: {}", view.base_url);
    println!("model:    {}", view.model);
    println!("api-key:  {}", key_source_label(view.key_source));
}

fn key_source_label(source: KeySource) -> &'static str {
    match source {
        KeySource::Env => "(from env)",
        KeySource::Keychain => "(from keychain)",
        KeySource::File => "(from file)",
        KeySource::Absent => "(none)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_key_line_names_the_source_and_never_a_key() {
        assert_eq!(key_source_label(KeySource::Env), "(from env)");
        assert_eq!(key_source_label(KeySource::Keychain), "(from keychain)");
        assert_eq!(key_source_label(KeySource::File), "(from file)");
        assert_eq!(key_source_label(KeySource::Absent), "(none)");
    }

    /// A key that was not checked says nothing, rather than something that
    /// reads as a verdict; every other state gets its line.
    #[test]
    fn key_line_is_silent_only_for_a_key_that_was_not_checked() {
        assert_eq!(key_line(KeyCheck::Accepted), Some("key: accepted"));
        assert_eq!(
            key_line(KeyCheck::Rejected),
            Some("key: REJECTED — OpenRouter answered 401; set a new key")
        );
        assert_eq!(
            key_line(KeyCheck::Missing),
            Some(
                "key: MISSING — save one in Settings → Captions, or set it with \
                 `maj describer set --api-key` or MAJ_OPENROUTER_KEY"
            )
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
            ("key missing", probe(true, None, KeyCheck::Missing), false),
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

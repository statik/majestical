//! `maj describer set|show|test|clear-key` — per-machine backend
//! configuration. Compute lives in `majestical_services::describer_config`;
//! the key's reading and writing in `crate::describer_key`; this module
//! renders.

use std::path::Path;

use majestical_services::describer_config::{
    self, ClearKeyOutcome, DescriberConfigView, DescriberProbe, KeyCheck, KeySource, SetArgs,
};
use majestical_services::notices::Notices;

use crate::describer_key::{self, KeySources};

/// `maj describer set`'s arguments as the command line gives them: the key
/// still unplaced, where [`SetArgs`] already says what the file does with it.
pub(crate) struct SetRequest {
    pub(crate) backend: majestical_describe::BackendKind,
    pub(crate) model: String,
    pub(crate) base_url: Option<String>,
    pub(crate) api_key: Option<String>,
}

/// Key first, then the config, then the echo: the Keychain write must
/// precede the file's (see `describer_key::store`), and the echo names the
/// key's source for the backend `set` just stored.
pub(crate) fn cmd_set(catalog_root: &Path, request: SetRequest) -> anyhow::Result<()> {
    let store = describer_key::system_store();
    let file_key = describer_key::store(request.api_key, &store)?;
    let notices = Notices::new();
    let stored = describer_config::set(
        catalog_root,
        &SetArgs {
            backend: request.backend,
            model: request.model,
            base_url: request.base_url,
            file_key,
        },
        &notices,
    );
    crate::drain_notices(&notices);
    stored?;
    cmd_show(catalog_root)
}

pub(crate) fn cmd_show(catalog_root: &Path) -> anyhow::Result<()> {
    let notices = Notices::new();
    let store = describer_key::system_store();
    let resolved = describer_key::resolve(catalog_root, &KeySources::ambient(&store), &notices);
    let shown = describer_config::show(catalog_root, describer_key::presence(&resolved), &notices);
    crate::drain_notices(&notices);
    match shown? {
        Some(view) => print_view(&view),
        None => println!(
            "no describer configured — run `maj describer set --backend <ollama|lm-studio|open-router> --model <model>`"
        ),
    }
    Ok(())
}

pub(crate) fn cmd_clear_key(catalog_root: &Path) -> anyhow::Result<()> {
    let notices = Notices::new();
    let store = describer_key::system_store();
    let cleared = describer_key::clear(catalog_root, &KeySources::ambient(&store), &notices);
    crate::drain_notices(&notices);
    for line in clear_key_lines(&cleared?) {
        println!("{line}");
    }
    Ok(())
}

/// What `clear-key` prints: what was removed, then — when it is — that the
/// environment goes on supplying a key all the same.
fn clear_key_lines(outcome: &ClearKeyOutcome) -> Vec<&'static str> {
    let removed = match (outcome.keychain_cleared, outcome.file_cleared) {
        (true, true) => "removed the key from the Keychain and describer.toml",
        (true, false) => "removed the key from the Keychain",
        (false, true) => "removed the key from describer.toml",
        (false, false) => "no stored key to remove",
    };
    let mut lines = vec![removed];
    if outcome.env_still_supplies {
        lines.push("MAJ_OPENROUTER_KEY is set and still supplies a key");
    }
    lines
}

pub(crate) fn cmd_test(catalog_root: &Path) -> anyhow::Result<()> {
    let notices = Notices::new();
    let store = describer_key::system_store();
    let resolved = describer_key::resolve(catalog_root, &KeySources::ambient(&store), &notices);
    let probe = describer_config::test(catalog_root, resolved.key, &notices);
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

    #[test]
    fn clear_key_says_what_was_removed_and_whether_the_env_still_supplies() {
        let lines = |keychain_cleared, file_cleared, env_still_supplies| {
            clear_key_lines(&ClearKeyOutcome {
                keychain_cleared,
                file_cleared,
                env_still_supplies,
            })
        };
        assert_eq!(
            lines(true, true, false),
            ["removed the key from the Keychain and describer.toml"]
        );
        assert_eq!(
            lines(true, false, false),
            ["removed the key from the Keychain"]
        );
        assert_eq!(
            lines(false, true, false),
            ["removed the key from describer.toml"]
        );
        assert_eq!(lines(false, false, false), ["no stored key to remove"]);
        assert_eq!(
            lines(false, false, true),
            [
                "no stored key to remove",
                "MAJ_OPENROUTER_KEY is set and still supplies a key"
            ]
        );
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

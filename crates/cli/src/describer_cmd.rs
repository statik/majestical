//! `maj describer set|show|test` — per-machine backend configuration.
//! Compute for all three lives in `majestical_services::describer_config`;
//! this module only reads the API-key env var and renders.

use std::path::Path;

use majestical_services::describer_config::{self, DescriberConfigView, SetArgs};

pub(crate) fn env_api_key() -> Option<String> {
    std::env::var("MAJ_OPENROUTER_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

pub(crate) fn cmd_set(catalog_root: &Path, args: &SetArgs) -> anyhow::Result<()> {
    let view = describer_config::set(catalog_root, args)?;
    print_view(&view);
    Ok(())
}

pub(crate) fn cmd_show(catalog_root: &Path) -> anyhow::Result<()> {
    match describer_config::show(catalog_root)? {
        Some(view) => print_view(&view),
        None => println!(
            "no describer configured — run `maj describer set --backend <ollama|lm-studio|open-router> --model <model>`"
        ),
    }
    Ok(())
}

pub(crate) fn cmd_test(catalog_root: &Path) -> anyhow::Result<()> {
    let probe = describer_config::test(catalog_root, env_api_key())?;
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
    if probe.model_listed && probe.vision != Some(false) {
        println!("caption and tag-suggestion work will run on the next `maj index run`");
    }
    Ok(())
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

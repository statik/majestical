//! `maj describer set|show|test` — per-machine backend configuration.

use std::path::Path;

use anyhow::{Context as _, bail};
use majestical_describe::{BackendKind, DescriberConfig, HttpDescriber};

use majestical_services::describer_config::{config_path, load_config};

pub(crate) fn env_api_key() -> Option<String> {
    std::env::var("MAJ_OPENROUTER_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

pub(crate) struct SetArgs {
    pub backend: BackendKind,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

pub(crate) fn cmd_set(catalog_root: &Path, args: &SetArgs) -> anyhow::Result<()> {
    let config = DescriberConfig {
        backend: args.backend,
        base_url: args
            .base_url
            .clone()
            .unwrap_or_else(|| args.backend.default_base_url().to_string()),
        model: args.model.clone(),
        api_key: args.api_key.clone(),
    };
    let path = config_path(catalog_root)?;
    config
        .store(&path)
        .with_context(|| format!("write {}", path.display()))?;
    print_config(&config);
    Ok(())
}

pub(crate) fn cmd_show(catalog_root: &Path) -> anyhow::Result<()> {
    match load_config(catalog_root)? {
        Some(config) => print_config(&config),
        None => println!(
            "no describer configured — run `maj describer set --backend <ollama|lm-studio|open-router> --model <model>`"
        ),
    }
    Ok(())
}

pub(crate) fn cmd_test(catalog_root: &Path) -> anyhow::Result<()> {
    let Some(config) = load_config(catalog_root)? else {
        bail!("no describer configured — run `maj describer set`");
    };
    let base_url = config.base_url.clone();
    let model = config.model.clone();
    let describer = HttpDescriber::new(config, env_api_key());
    let report = describer
        .probe()
        .with_context(|| format!("describer test against {base_url}"))?;
    println!("backend reachable: yes");
    println!(
        "model {model} listed: {}",
        if report.model_listed {
            "yes"
        } else {
            "NO — check the model name"
        }
    );
    match report.vision {
        Some(true) => println!("vision capability: yes"),
        Some(false) => {
            println!("vision capability: NO — caption work will not run with this model");
        }
        None => println!("vision capability: unknown (reported by LM Studio only)"),
    }
    if report.model_listed && report.vision != Some(false) {
        println!("caption and tag-suggestion work will run on the next `maj index run`");
    }
    Ok(())
}

fn print_config(config: &DescriberConfig) {
    println!("backend:  {}", config.backend.as_str());
    println!("base-url: {}", config.base_url);
    println!("model:    {}", config.model);
    match &config.api_key {
        Some(_) => println!("api-key:  (redacted)"),
        None => println!("api-key:  (none)"),
    }
}

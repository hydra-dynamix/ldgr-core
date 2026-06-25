use std::fs;

use anyhow::Context;

use crate::store::{
    create_bundle, create_prompt, seal_bundle, set_prompt_status, update_prompt, Prompt,
    PromptBundle,
};

use super::super::args::{BundleArgs, BundleCommand, PromptArgs, PromptCommand};

pub fn handle_prompt(connection: &rusqlite::Connection, args: PromptArgs) -> anyhow::Result<()> {
    match args.command {
        PromptCommand::Create(args) => {
            let prompt = create_prompt(
                connection,
                &args.slug,
                &args.role,
                &args.body,
                None,
                args.description.as_deref(),
            )?;
            print_prompt_action("created", &prompt);
        }
        PromptCommand::Import(args) => {
            let body = fs::read_to_string(&args.path)
                .with_context(|| format!("failed to read prompt file {}", args.path.display()))?;
            let path = args.path.to_string_lossy();
            let prompt = create_prompt(
                connection,
                &args.slug,
                &args.role,
                &body,
                Some(path.as_ref()),
                args.description.as_deref(),
            )?;
            print_prompt_action("imported", &prompt);
        }
        PromptCommand::Update(args) => {
            let body = fs::read_to_string(&args.path)
                .with_context(|| format!("failed to read prompt file {}", args.path.display()))?;
            let path = args.path.to_string_lossy();
            let prompt = update_prompt(
                connection,
                &args.slug,
                &body,
                Some(path.as_ref()),
                args.description.as_deref(),
            )?;
            print_prompt_action("updated", &prompt);
        }
        PromptCommand::Activate(args) => {
            let prompt = set_prompt_status(connection, &args.slug, "active")?;
            print_prompt_action("activated", &prompt);
        }
    }
    Ok(())
}

pub fn handle_bundle(connection: &rusqlite::Connection, args: BundleArgs) -> anyhow::Result<()> {
    match args.command {
        BundleCommand::Create(args) => {
            let bundle = create_bundle(connection, &args.slug, &args.prompts)?;
            print_bundle_action("created", &bundle);
        }
        BundleCommand::Seal(args) => {
            let bundle = seal_bundle(connection, &args.slug)?;
            print_bundle_action("sealed", &bundle);
        }
    }
    Ok(())
}

fn print_prompt_action(action: &str, prompt: &Prompt) {
    println!(
        "{action} prompt {} version={} status={} hash={}",
        prompt.slug, prompt.current_version, prompt.status, prompt.content_hash
    );
}

fn print_bundle_action(action: &str, bundle: &PromptBundle) {
    println!(
        "{action} bundle {} status={} hash={}",
        bundle.slug, bundle.status, bundle.bundle_hash
    );
}

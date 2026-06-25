use std::path::PathBuf;

use clap::{Args, Subcommand};

const PROMPT_HELP: &str = "Examples:\n  ldgr prompt create surface --role surface-loop --body '... {{ldgr_context}} ...'\n  ldgr prompt import implementation --role implementation-loop --path prompts/impl.md\n  ldgr prompt update surface --path prompts/surface-v2.md\n  ldgr prompt activate surface\n\nPrompt records are ledger-owned prompt templates. Loop runs consume active prompts with `ldgr loop run --prompt-slug <slug>`.";

const BUNDLE_HELP: &str = "Examples:\n  ldgr bundle create cleanroom --prompt surface --prompt implementation\n  ldgr bundle seal cleanroom\n\nBundles seal active prompt versions so loop runs can consume an immutable prompt set with `ldgr loop run --bundle <slug>`.";

#[derive(Debug, Args)]
#[command(after_help = PROMPT_HELP)]
pub struct PromptArgs {
    #[command(subcommand)]
    pub command: PromptCommand,
}

#[derive(Debug, Subcommand)]
pub enum PromptCommand {
    /// Create a draft prompt from inline body text.
    Create(CreatePromptArgs),
    /// Import a draft prompt from a file.
    Import(ImportPromptArgs),
    /// Update a prompt body from a file, creating a new version.
    Update(UpdatePromptArgs),
    /// Mark a prompt active for loop use.
    Activate(ActivatePromptArgs),
}

#[derive(Debug, Args)]
pub struct CreatePromptArgs {
    pub slug: String,

    #[arg(long)]
    pub role: String,

    #[arg(long)]
    pub body: String,

    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Debug, Args)]
pub struct ImportPromptArgs {
    pub slug: String,

    #[arg(long)]
    pub role: String,

    #[arg(long)]
    pub path: PathBuf,

    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Debug, Args)]
pub struct UpdatePromptArgs {
    pub slug: String,

    #[arg(long)]
    pub path: PathBuf,

    #[arg(long)]
    pub description: Option<String>,
}

#[derive(Debug, Args)]
pub struct ActivatePromptArgs {
    pub slug: String,
}

#[derive(Debug, Args)]
#[command(after_help = BUNDLE_HELP)]
pub struct BundleArgs {
    #[command(subcommand)]
    pub command: BundleCommand,
}

#[derive(Debug, Subcommand)]
pub enum BundleCommand {
    /// Create a draft bundle from active prompt slugs.
    Create(CreateBundleArgs),
    /// Seal a draft bundle for loop use.
    Seal(SealBundleArgs),
}

#[derive(Debug, Args)]
pub struct CreateBundleArgs {
    pub slug: String,

    #[arg(long = "prompt", required = true)]
    pub prompts: Vec<String>,
}

#[derive(Debug, Args)]
pub struct SealBundleArgs {
    pub slug: String,
}

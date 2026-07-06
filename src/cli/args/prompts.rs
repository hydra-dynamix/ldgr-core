use std::path::PathBuf;

use clap::{Args, Subcommand};

const PROMPT_HELP: &str = "Examples:\n  ldgr prompt list\n  ldgr prompt show surface\n  ldgr prompt show surface --body\n  ldgr prompt create surface --role surface-loop --body '... {{ldgr_context}} ...'\n  ldgr prompt import implementation --role implementation-loop --path prompts/impl.md\n  ldgr prompt update surface --path prompts/surface-v2.md\n  ldgr prompt compose project-loop --source surface --source ./prompts/project-rules.md\n  ldgr loop run --prompt-slug project-loop --agent agentctl\n\nPrompts are global files under $LDGR_HOME/prompts or ~/.ldgr/prompts. The prompt slug maps to <slug>.md in that directory. Use list/show to discover global prompts. Use compose to concatenate prompt slugs and/or file paths once and store the result as a reusable global prompt.";

#[derive(Debug, Args)]
#[command(after_help = PROMPT_HELP)]
pub struct PromptArgs {
    #[command(subcommand)]
    pub command: PromptCommand,
}

#[derive(Debug, Subcommand)]
pub enum PromptCommand {
    /// List global prompt files.
    List(ListPromptArgs),
    /// Show one global prompt file.
    Show(ShowPromptArgs),
    /// Create a draft prompt from inline body text.
    Create(CreatePromptArgs),
    /// Import a draft prompt from a file.
    Import(ImportPromptArgs),
    /// Update a prompt body from a file.
    Update(UpdatePromptArgs),
    /// Compose prompt slugs and/or paths into a reusable global prompt.
    Compose(ComposePromptArgs),
    /// Compatibility check: verify a global prompt slug exists.
    Activate(ActivatePromptArgs),
}

#[derive(Debug, Args)]
pub struct ListPromptArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowPromptArgs {
    pub slug: String,

    /// Print only the current prompt body.
    #[arg(long)]
    pub body: bool,

    #[arg(long)]
    pub json: bool,
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
pub struct ComposePromptArgs {
    /// New global prompt slug to write under ~/.ldgr/prompts/<slug>.md.
    pub slug: String,

    /// Prompt slug or file path. Repeat in the desired concatenation order.
    #[arg(long = "source", required = true)]
    pub sources: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ActivatePromptArgs {
    pub slug: String,
}

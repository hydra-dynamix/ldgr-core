use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Serialize;

use crate::store::stable_content_hash;

use super::super::args::{PromptArgs, PromptCommand};

#[derive(Debug, Serialize)]
struct GlobalPromptSummary {
    slug: String,
    path: String,
    content_hash: String,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct GlobalPromptDetail {
    slug: String,
    path: String,
    content_hash: String,
    bytes: u64,
    body: String,
}

pub fn handle_prompt(connection: &rusqlite::Connection, args: PromptArgs) -> anyhow::Result<()> {
    let _ = connection;
    match args.command {
        PromptCommand::List(args) => {
            let prompts = list_global_prompts()?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&prompts)?);
            } else {
                print_prompt_list(&prompts);
            }
        }
        PromptCommand::Show(args) => {
            let prompt = read_global_prompt(&args.slug)?;
            if args.body {
                if args.json {
                    bail!("--body and --json are mutually exclusive");
                }
                print!("{}", prompt.body);
                if !prompt.body.ends_with('\n') {
                    println!();
                }
            } else if args.json {
                println!("{}", serde_json::to_string_pretty(&prompt)?);
            } else {
                print_prompt_detail(&prompt);
            }
        }
        PromptCommand::Create(args) => {
            write_global_prompt(&args.slug, &args.body)?;
            let prompt = read_global_prompt(&args.slug)?;
            print_prompt_action("created", &prompt);
        }
        PromptCommand::Import(args) => {
            let body = fs::read_to_string(&args.path)
                .with_context(|| format!("failed to read prompt file {}", args.path.display()))?;
            write_global_prompt(&args.slug, &body)?;
            let prompt = read_global_prompt(&args.slug)?;
            print_prompt_action("imported", &prompt);
        }
        PromptCommand::Update(args) => {
            let body = fs::read_to_string(&args.path)
                .with_context(|| format!("failed to read prompt file {}", args.path.display()))?;
            write_global_prompt(&args.slug, &body)?;
            let prompt = read_global_prompt(&args.slug)?;
            print_prompt_action("updated", &prompt);
        }
        PromptCommand::Compose(args) => {
            let body = compose_prompt_body(&args.sources)?;
            write_global_prompt(&args.slug, &body)?;
            let prompt = read_global_prompt(&args.slug)?;
            println!(
                "composed global prompt {} path={} hash={} sources={}",
                prompt.slug,
                prompt.path,
                prompt.content_hash,
                args.sources.len()
            );
        }
        PromptCommand::Activate(args) => {
            let prompt = read_global_prompt(&args.slug)?;
            println!(
                "global prompt {} is available path={} hash={}",
                prompt.slug, prompt.path, prompt.content_hash
            );
        }
    }
    Ok(())
}

fn global_prompt_root() -> anyhow::Result<PathBuf> {
    let root = std::env::var_os("LDGR_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".ldgr")))
        .context("cannot resolve LDGR_HOME or HOME for global prompt root")?
        .join("prompts");
    Ok(root)
}

fn prompt_path_for_write(slug: &str) -> anyhow::Result<PathBuf> {
    validate_prompt_slug(slug)?;
    Ok(global_prompt_root()?.join(format!("{slug}.md")))
}

fn prompt_path_for_read(slug: &str) -> anyhow::Result<PathBuf> {
    validate_prompt_slug(slug)?;
    let root = global_prompt_root()?;
    let direct = root.join(slug);
    if direct.is_file() {
        return Ok(direct);
    }
    let markdown = root.join(format!("{slug}.md"));
    if markdown.is_file() {
        return Ok(markdown);
    }
    bail!(
        "unknown global prompt {slug}; expected {} or {}",
        direct.display(),
        markdown.display()
    )
}

fn validate_prompt_slug(slug: &str) -> anyhow::Result<()> {
    if slug.is_empty() || slug.contains('/') || slug.contains('\\') || slug == "." || slug == ".." {
        bail!("prompt slug must be a simple file stem under ~/.ldgr/prompts");
    }
    Ok(())
}

fn write_global_prompt(slug: &str, body: &str) -> anyhow::Result<()> {
    let path = prompt_path_for_write(slug)?;
    fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))?;
    fs::write(&path, body).with_context(|| format!("failed to write prompt {}", path.display()))
}

fn list_global_prompts() -> anyhow::Result<Vec<GlobalPromptSummary>> {
    let root = global_prompt_root()?;
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut prompts = Vec::new();
    for entry in
        fs::read_dir(&root).with_context(|| format!("failed to read {}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(slug) = path
            .file_stem()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        let body = fs::read_to_string(&path)
            .with_context(|| format!("failed to read prompt {}", path.display()))?;
        let bytes = entry.metadata()?.len();
        prompts.push(GlobalPromptSummary {
            slug,
            path: path.display().to_string(),
            content_hash: stable_content_hash(&body),
            bytes,
        });
    }
    prompts.sort_by(|left, right| left.slug.cmp(&right.slug));
    Ok(prompts)
}

fn read_prompt_source(source: &str) -> anyhow::Result<(String, PathBuf, String)> {
    let path = PathBuf::from(source);
    if path.is_file() {
        let body = fs::read_to_string(&path)
            .with_context(|| format!("failed to read prompt source {}", path.display()))?;
        return Ok((source.to_owned(), path, body));
    }
    let prompt = read_global_prompt(source)?;
    Ok((source.to_owned(), PathBuf::from(prompt.path), prompt.body))
}

fn compose_prompt_body(sources: &[String]) -> anyhow::Result<String> {
    let mut fragments = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let (label, path, body) = read_prompt_source(source)?;
        fragments.push(format!(
            "<!-- ldgr-prompt-fragment {}: {} ({}) -->\n{}\n",
            index + 1,
            label,
            path.display(),
            body.trim_end()
        ));
    }
    Ok(fragments.join("\n"))
}

fn read_global_prompt(slug: &str) -> anyhow::Result<GlobalPromptDetail> {
    let path = prompt_path_for_read(slug)?;
    let body = fs::read_to_string(&path)
        .with_context(|| format!("failed to read prompt {}", path.display()))?;
    let bytes = fs::metadata(&path)?.len();
    Ok(GlobalPromptDetail {
        slug: path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or(slug)
            .to_owned(),
        path: path.display().to_string(),
        content_hash: stable_content_hash(&body),
        bytes,
        body,
    })
}

fn print_prompt_action(action: &str, prompt: &GlobalPromptDetail) {
    println!(
        "{action} global prompt {} path={} hash={}",
        prompt.slug, prompt.path, prompt.content_hash
    );
}

fn print_prompt_list(prompts: &[GlobalPromptSummary]) {
    if prompts.is_empty() {
        println!(
            "no global prompts in {}",
            global_prompt_root()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "~/.ldgr/prompts".to_owned())
        );
        return;
    }
    for prompt in prompts {
        println!(
            "{} path={} hash={} bytes={}",
            prompt.slug, prompt.path, prompt.content_hash, prompt.bytes
        );
    }
}

fn print_prompt_detail(prompt: &GlobalPromptDetail) {
    println!("prompt: {}", prompt.slug);
    println!("path: {}", prompt.path);
    println!("hash: {}", prompt.content_hash);
    println!("bytes: {}", prompt.bytes);
    println!("body:\n{}", prompt.body);
}

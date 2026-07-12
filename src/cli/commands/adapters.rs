use anyhow::bail;

use crate::adapter_registry::{AdapterCommand as RegistryCommand, AdapterRegistry};

use super::super::args::{
    AdapterArgs, AdapterCommand, InstallAdapterArgs as CoreInstallAdapterArgs,
};

pub fn handle_adapter(args: AdapterArgs) -> anyhow::Result<()> {
    let registry = AdapterRegistry::discover();
    print_warnings(&registry);
    match args.command {
        AdapterCommand::Install(args) => match args.name {
            Some(name) if name == "list" => {
                super::ops::print_available_adapter_catalog();
                Ok(())
            }
            Some(name) => super::ops::handle_install_adapter(&CoreInstallAdapterArgs {
                name,
                source_root: args.source_root,
                install_root: args.install_root,
                version: args.version,
                prerelease: args.prerelease,
                yes: args.yes,
            }),
            None => super::ops::handle_interactive_adapter_install(
                args.source_root,
                args.install_root,
                args.yes,
            ),
        },
        AdapterCommand::List(args) => {
            if args.json {
                println!("{}", serde_json::to_string_pretty(&registry)?);
            } else if registry.adapters.is_empty() {
                println!("No adapters discovered.");
            } else {
                for adapter in &registry.adapters {
                    let aliases = if adapter.aliases.is_empty() {
                        String::new()
                    } else {
                        format!(" aliases={}", adapter.aliases.join(","))
                    };
                    println!(
                        "adapter={} title={} core_version={}{} manifest={}",
                        adapter.slug,
                        adapter.title,
                        adapter.core_version,
                        aliases,
                        adapter.manifest_path.display()
                    );
                    for namespace in &adapter.command_namespaces {
                        println!(
                            "  namespace={} argv=\"{}\"{}",
                            namespace.namespace,
                            namespace.argv.join(" "),
                            namespace
                                .description
                                .as_ref()
                                .map(|description| format!(" description={description}"))
                                .unwrap_or_default()
                        );
                    }
                    for command in &adapter.commands {
                        println!(
                            "  command={} argv=\"{}\"{}",
                            command.name,
                            command.argv.join(" "),
                            command
                                .description
                                .as_ref()
                                .map(|description| format!(" description={description}"))
                                .unwrap_or_default()
                        );
                    }
                }
            }
            Ok(())
        }
        AdapterCommand::Show(args) => {
            let Some(adapter) = registry.find(&args.slug_or_alias) else {
                bail!("adapter `{}` was not discovered", args.slug_or_alias);
            };
            if args.json {
                println!("{}", serde_json::to_string_pretty(adapter)?);
            } else {
                println!("adapter: {}", adapter.slug);
                println!("title: {}", adapter.title);
                println!("core_version: {}", adapter.core_version);
                if !adapter.aliases.is_empty() {
                    println!("aliases: {}", adapter.aliases.join(","));
                }
                println!("manifest: {}", adapter.manifest_path.display());
                println!("root: {}", adapter.root_path.display());
                println!("loop_prompt_path: {}", adapter.profile.loop_prompt_path);
                println!(
                    "default_milestone_template: {}",
                    adapter.profile.default_milestone_template
                );
                println!("spec_artifact_path: {}", adapter.profile.spec_artifact_path);
                println!("readiness_policy: {}", adapter.profile.readiness_policy);
                if let Some(digest) = &adapter.verified_manifest_digest {
                    println!("verified_manifest_digest: {digest}");
                }
                for namespace in &adapter.command_namespaces {
                    println!("namespace: {}", namespace.namespace);
                    println!("  argv: {}", namespace.argv.join(" "));
                    if !namespace.aliases.is_empty() {
                        println!("  aliases: {}", namespace.aliases.join(","));
                    }
                    if let Some(description) = &namespace.description {
                        println!("  description: {description}");
                    }
                }
                for command in &adapter.commands {
                    print_command(command);
                }
            }
            Ok(())
        }
        AdapterCommand::Dispatch(args) => {
            let commands = registry.resolve_command(&args.command);
            if commands.is_empty() {
                bail!("adapter command `{}` was not discovered", args.command);
            }
            if args.json {
                println!("{}", serde_json::to_string_pretty(&commands)?);
            } else {
                for command in commands {
                    print_command(command);
                }
            }
            Ok(())
        }
    }
}

pub fn print_adapter_command_hint(command_name: &str) {
    let registry = AdapterRegistry::discover();
    let commands = registry.resolve_command(command_name);
    if commands.is_empty() {
        return;
    }
    eprintln!();
    eprintln!("Adapter command `{command_name}` is installed but is not a core command.");
    eprintln!("Inspect it with `ldgr adapter dispatch {command_name}`.");
    for command in commands {
        eprintln!(
            "  adapter={} argv=\"{}\"",
            command.adapter_slug,
            command.argv.join(" ")
        );
    }
}

fn print_warnings(registry: &AdapterRegistry) {
    for warning in &registry.warnings {
        eprintln!(
            "warning: skipped adapter manifest {}: {}",
            warning.manifest_path.display(),
            warning.message
        );
    }
}

fn print_command(command: &RegistryCommand) {
    println!("command: {}", command.name);
    println!("  adapter: {}", command.adapter_slug);
    println!("  argv: {}", command.argv.join(" "));
    if let Some(description) = &command.description {
        println!("  description: {description}");
    }
}

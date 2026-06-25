use anyhow::bail;

use crate::store::{list_decisions, record_decision, NextWorkSpec};

use super::super::args::{DecisionArgs, DecisionCommand};
use super::super::checked_limit;
use super::super::render::emit;
use super::super::render::text::print_decisions;

pub fn handle_decision(
    connection: &rusqlite::Connection,
    args: DecisionArgs,
) -> anyhow::Result<()> {
    match args.command {
        DecisionCommand::List(args) => {
            let decisions = list_decisions(
                connection,
                args.work_slug.as_deref(),
                checked_limit(args.limit)?,
            )?;
            emit(args.json, &decisions, |decisions| {
                print_decisions(decisions)
            })?;
        }
        DecisionCommand::Record(args) => {
            let next_work = match (&args.next_slug, &args.next_title, &args.next_description) {
                (Some(slug), title, description) => Some(NextWorkSpec {
                    slug: slug.as_str(),
                    title: title.as_deref(),
                    description: description.as_deref(),
                }),
                (None, None, None) => None,
                (None, _, _) => bail!("--next-slug is required when supplying next work details"),
            };
            let decision = record_decision(
                connection,
                &args.work_slug,
                args.outcome.into(),
                &args.rationale,
                next_work,
            )?;
            println!(
                "recorded decision {} [{}]",
                decision.id,
                decision.outcome.as_str()
            );
        }
    }
    Ok(())
}

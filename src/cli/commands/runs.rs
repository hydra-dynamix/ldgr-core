use std::path::Path;

use crate::store::{
    add_artifact, add_observation, add_validation_record, close_run, finish_run, get_artifact,
    get_run, list_artifacts, list_observations, list_runs, list_validation_records, start_run,
    NextWorkSpec, RunStatus,
};

use super::super::args::{
    ArtifactArgs, ArtifactCommand, ObservationArgs, ObservationCommand, RunArgs, RunCommand,
    ValidationArgs, ValidationCommand,
};
use super::super::checked_limit;
use super::super::render::emit;
use super::super::render::text::{
    print_artifact, print_artifacts, print_observations, print_run, print_runs,
    print_validation_record, print_validations,
};

pub fn handle_run(connection: &rusqlite::Connection, args: RunArgs) -> anyhow::Result<()> {
    match args.command {
        RunCommand::List(args) => {
            let runs = list_runs(connection, args.status.map(RunStatus::from))?;
            emit(args.json, &runs, |runs| print_runs(runs))?;
        }
        RunCommand::Show(args) => {
            let run = get_run(connection, args.run_id)?;
            emit(args.json, &run, print_run)?;
        }
        RunCommand::Start(args) => {
            let run = start_run(connection, &args.work_slug, args.command.as_deref())?;
            println!("started run {} for {}", run.id, args.work_slug);
        }
        RunCommand::Finish(args) => {
            let run = finish_run(
                connection,
                args.run_id,
                args.status.into(),
                args.notes.as_deref(),
            )?;
            println!("finished run {} [{}]", run.id, run.status.as_str());
        }
        RunCommand::Close(args) => {
            let next_work = optional_next_work(
                args.next_slug.as_deref(),
                args.next_title.as_deref(),
                args.next_description.as_deref(),
            )?;
            let closed = close_run(
                connection,
                args.run_id,
                args.status.into(),
                args.notes.as_deref(),
                args.outcome.into(),
                &args.rationale,
                next_work,
            )?;
            println!(
                "closed run {} [{}] and recorded decision {} [{}] for {}",
                closed.run.id,
                closed.run.status.as_str(),
                closed.decision.id,
                closed.decision.outcome.as_str(),
                closed.work_item.slug
            );
        }
    }
    Ok(())
}

type NextWork<'a> = Option<NextWorkSpec<'a>>;

fn optional_next_work<'a>(
    slug: Option<&'a str>,
    title: Option<&'a str>,
    description: Option<&'a str>,
) -> anyhow::Result<NextWork<'a>> {
    match (slug, title, description) {
        (None, None, None) => Ok(None),
        (Some(slug), title, description) => Ok(Some(NextWorkSpec {
            slug,
            title,
            description,
        })),
        (None, _, _) => anyhow::bail!("--next-slug is required when supplying next work details"),
    }
}

pub fn handle_observation(
    connection: &rusqlite::Connection,
    args: ObservationArgs,
) -> anyhow::Result<()> {
    match args.command {
        ObservationCommand::List(args) => {
            let observations =
                list_observations(connection, args.run_id, checked_limit(args.limit)?)?;
            emit(args.json, &observations, |observations| {
                print_observations(observations)
            })?;
        }
        ObservationCommand::Add(args) => {
            let observation = add_observation(connection, args.run_id, &args.body)?;
            println!("added observation {}", observation.id);
        }
    }
    Ok(())
}

pub fn handle_artifact(
    connection: &rusqlite::Connection,
    artifact_root: &Path,
    args: ArtifactArgs,
) -> anyhow::Result<()> {
    match args.command {
        ArtifactCommand::List(args) => {
            let artifacts = list_artifacts(connection, args.run_id, checked_limit(args.limit)?)?;
            emit(args.json, &artifacts, |artifacts| {
                print_artifacts(artifacts)
            })?;
        }
        ArtifactCommand::Show(args) => {
            let artifact = get_artifact(connection, args.artifact_id)?;
            emit(args.json, &artifact, print_artifact)?;
        }
        ArtifactCommand::Add(args) => {
            let artifact = add_artifact(
                connection,
                artifact_root,
                args.run_id,
                args.kind.parse()?,
                &args.path,
                &args.description,
            )?;
            println!("added artifact {} {}", artifact.id, artifact.path.display());
        }
    }
    Ok(())
}

pub fn handle_validation(
    connection: &rusqlite::Connection,
    args: ValidationArgs,
) -> anyhow::Result<()> {
    match args.command {
        ValidationCommand::List(args) => {
            let validations =
                list_validation_records(connection, args.run_id, checked_limit(args.limit)?)?;
            emit(args.json, &validations, |validations| {
                print_validations(validations)
            })?;
        }
        ValidationCommand::Record(args) => {
            let validation = add_validation_record(
                connection,
                args.run_id,
                args.outcome.into(),
                args.command.as_deref(),
                args.rationale.as_deref(),
            )?;
            emit(false, &validation, print_validation_record)?;
        }
    }
    Ok(())
}

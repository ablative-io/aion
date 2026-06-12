//! Scaffolding for `aion new`: name validation, target-directory checks,
//! placeholder substitution, and file emission.

use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use crate::new::template::{ManifestFile, Template};
use crate::output::to_value;

/// Worker SDK languages `--worker` can scaffold.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum WorkerLanguage {
    /// A Rust crate under `worker/` built on the `aion-worker` SDK.
    Rust,
}

/// Arguments for `aion new`.
#[derive(Args, Clone, Debug)]
pub struct NewArgs {
    /// Project name; becomes the directory, the Gleam entry module, and the
    /// workflow type. Must be lowercase `snake_case`: a lowercase letter
    /// followed by lowercase letters, digits, or underscores.
    name: String,
    /// Scaffold template.
    #[arg(long, value_enum, default_value_t = Template::HelloWorld)]
    template: Template,
    /// Additionally scaffold an activity worker crate under `worker/`
    /// serving the template's activities. Only templates that dispatch
    /// activities (saga) accept this.
    #[arg(long, value_enum)]
    worker: Option<WorkerLanguage>,
}

impl NewArgs {
    /// Test constructor mirroring the clap surface.
    #[cfg(test)]
    pub(crate) fn for_tests(
        name: &str,
        template: Template,
        worker: Option<WorkerLanguage>,
    ) -> Self {
        Self {
            name: name.to_owned(),
            template,
            worker,
        }
    }
}

/// JSON document printed on stdout after a successful `new` run.
#[derive(Serialize)]
struct NewOutput {
    project: String,
    path: String,
    template: &'static str,
    worker: Option<&'static str>,
    files: Vec<String>,
    next_steps: String,
}

/// Runs the `new` subcommand against the invoker's current directory.
pub fn run(args: &NewArgs) -> Result<Value> {
    let parent = std::env::current_dir().context("failed to resolve the current directory")?;
    scaffold(&parent, args)
}

/// Scaffolds `<parent>/<name>` from the selected template.
fn scaffold(parent: &Path, args: &NewArgs) -> Result<Value> {
    validate_name(&args.name)?;
    let files = manifest(args)?;
    let target = parent.join(&args.name);
    ensure_target_is_empty(&target)?;

    let mut written = Vec::with_capacity(files.len());
    for (path_template, contents_template) in files {
        let relative = render(path_template, &args.name)?;
        let contents = render(contents_template, &args.name)?;
        let destination = target.join(&relative);
        if let Some(directory) = destination.parent() {
            std::fs::create_dir_all(directory).with_context(|| {
                format!("failed to create directory {}", directory.display())
            })?;
        }
        std::fs::write(&destination, contents)
            .with_context(|| format!("failed to write {}", destination.display()))?;
        written.push(relative);
    }

    to_value(NewOutput {
        project: args.name.clone(),
        path: target.display().to_string(),
        template: args.template.id(),
        worker: args.worker.map(|WorkerLanguage::Rust| "rust"),
        files: written,
        next_steps: format!("see {}/README.md", args.name),
    })
}

/// Resolves the full file manifest: template files plus, when requested and
/// available, the worker crate. Refuses `--worker` for templates whose
/// workflows dispatch no activities — there is nothing for a worker to
/// serve, and a dangling worker crate would be a lie.
fn manifest(args: &NewArgs) -> Result<Vec<ManifestFile>> {
    let mut files = args.template.files();
    if let Some(WorkerLanguage::Rust) = args.worker {
        if args.template.activities().is_empty() {
            bail!(
                "the {} template declares no activities, so there is no worker to scaffold; \
                 use `--template saga` or drop `--worker`",
                args.template.id()
            );
        }
        files.extend_from_slice(args.template.worker_files());
    }
    Ok(files)
}

/// Validates the project name against Gleam module naming: lowercase
/// `snake_case`, starting with a lowercase ASCII letter.
fn validate_name(name: &str) -> Result<()> {
    let mut characters = name.chars();
    let starts_with_letter = characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase());
    let rest_is_snake = characters
        .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_');
    if !starts_with_letter || !rest_is_snake {
        bail!(
            "invalid project name {name:?}: the name becomes the Gleam entry module, so it \
             must be lowercase snake_case — a lowercase ASCII letter (a-z) followed by \
             lowercase letters, digits, or underscores"
        );
    }
    if name == "gleam" {
        bail!("invalid project name \"gleam\": the Gleam build tool reserves this name");
    }
    Ok(())
}

/// Refuses to scaffold over existing content. An existing *empty* directory
/// is acceptable; anything else is a loud error.
fn ensure_target_is_empty(target: &Path) -> Result<()> {
    if !target.exists() {
        return Ok(());
    }
    if !target.is_dir() {
        bail!(
            "refusing to scaffold: {} already exists and is not a directory",
            target.display()
        );
    }
    let mut entries = std::fs::read_dir(target)
        .with_context(|| format!("failed to inspect {}", target.display()))?;
    if entries.next().is_some() {
        bail!(
            "refusing to scaffold into {}: the directory is not empty",
            target.display()
        );
    }
    Ok(())
}

/// Substitutes every supported placeholder and fails loudly on any leftover
/// `{{` — an unresolved placeholder is a template bug, never emitted code.
fn render(template_text: &str, name: &str) -> Result<String> {
    let rendered = template_text
        .replace("{{name}}", name)
        .replace("{{aion_worker_version}}", env!("CARGO_PKG_VERSION"));
    if let Some(index) = rendered.find("{{") {
        let snippet: String = rendered[index..].chars().take(40).collect();
        bail!("unresolved template placeholder: {snippet}");
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{
        NewArgs, Template, WorkerLanguage, ensure_target_is_empty, render, scaffold,
        validate_name,
    };

    type TestError = Box<dyn std::error::Error>;

    /// Unwraps the error side of a fallible call without `expect_err`.
    fn require_error<T>(result: anyhow::Result<T>) -> Result<anyhow::Error, TestError> {
        match result {
            Ok(_) => Err("expected the call to fail".into()),
            Err(error) => Ok(error),
        }
    }

    #[test]
    fn name_validation_accepts_snake_case() -> Result<(), TestError> {
        for name in ["a", "my_flow", "order_saga_2", "x9"] {
            validate_name(name)?;
        }
        Ok(())
    }

    #[test]
    fn name_validation_rejects_invalid_names_with_the_rule() -> Result<(), TestError> {
        for name in [
            "", "My_Flow", "9lives", "_hidden", "kebab-case", "with space", "emoji✨", "gleam",
        ] {
            let error = require_error(validate_name(name))?;
            assert!(
                error.to_string().contains("invalid project name"),
                "error for {name:?} must state the rejection: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn refuses_non_empty_target_directory() -> Result<(), TestError> {
        let parent = tempfile::tempdir()?;
        let target = parent.path().join("busy");
        std::fs::create_dir(&target)?;
        std::fs::write(target.join("existing.txt"), "occupied")?;

        let args = NewArgs::for_tests("busy", Template::HelloWorld, None);
        let error = require_error(scaffold(parent.path(), &args))?;
        assert!(
            error.to_string().contains("not empty"),
            "refusal must name the cause: {error}"
        );
        Ok(())
    }

    #[test]
    fn accepts_an_existing_empty_directory() -> Result<(), TestError> {
        let parent = tempfile::tempdir()?;
        std::fs::create_dir(parent.path().join("vacant"))?;
        ensure_target_is_empty(&parent.path().join("vacant"))?;
        Ok(())
    }

    #[test]
    fn refuses_worker_for_templates_without_activities() -> Result<(), TestError> {
        let parent = tempfile::tempdir()?;
        for template in [Template::HelloWorld, Template::ApprovalFlow] {
            let args = NewArgs::for_tests("flow", template, Some(WorkerLanguage::Rust));
            let error = require_error(scaffold(parent.path(), &args))?;
            assert!(
                error.to_string().contains("no activities"),
                "refusal must explain itself: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn render_substitutes_name_and_rejects_leftover_placeholders() -> Result<(), TestError> {
        assert_eq!(render("name = \"{{name}}\"", "demo")?, "name = \"demo\"");
        assert_eq!(
            render("aion-worker = \"{{aion_worker_version}}\"", "demo")?,
            format!("aion-worker = \"{}\"", env!("CARGO_PKG_VERSION"))
        );
        let error = require_error(render("oops {{nmae}}", "demo"))?;
        assert!(error.to_string().contains("unresolved template placeholder"));
        Ok(())
    }

    #[test]
    fn every_template_renders_without_leftover_placeholders() -> Result<(), TestError> {
        for template in Template::all() {
            let mut files = template.files();
            files.extend_from_slice(template.worker_files());
            for (path, contents) in files {
                render(path, "demo_flow")?;
                render(contents, "demo_flow")?;
            }
        }
        Ok(())
    }

    /// Extracts every `aion <subcommand>` instruction from rendered README
    /// text: the token following a literal `aion ` occurrence.
    fn readme_instruction_tokens(readme: &str) -> Vec<String> {
        readme
            .match_indices("aion ")
            .filter_map(|(index, marker)| {
                let token: String = readme[index + marker.len()..]
                    .chars()
                    .take_while(|character| {
                        character.is_ascii_alphanumeric() || *character == '-'
                    })
                    .collect();
                if token.is_empty() { None } else { Some(token) }
            })
            .collect()
    }

    #[test]
    fn readme_instructions_match_real_subcommands() -> Result<(), TestError> {
        use clap::CommandFactory;

        let command = crate::Cli::command();
        let known: Vec<String> = command
            .get_subcommands()
            .map(|subcommand| subcommand.get_name().to_owned())
            .collect();

        for template in Template::all() {
            let readme = template
                .files()
                .iter()
                .find(|(path, _)| *path == "README.md")
                .map(|(_, contents)| *contents)
                .ok_or("every template must carry a README")?;
            let rendered = render(readme, "demo_flow")?;
            let tokens = readme_instruction_tokens(&rendered);
            assert!(
                !tokens.is_empty(),
                "template {} README must contain aion instructions",
                template.id()
            );
            for token in tokens {
                assert!(
                    known.contains(&token),
                    "template {} README references `aion {token}`, which is not a real subcommand",
                    template.id()
                );
            }
        }
        Ok(())
    }

    #[test]
    fn scaffold_writes_every_manifest_file() -> Result<(), TestError> {
        let parent = tempfile::tempdir()?;
        let args = NewArgs::for_tests("demo_saga", Template::Saga, Some(WorkerLanguage::Rust));
        let output: Value = scaffold(parent.path(), &args)?;
        let project = parent.path().join("demo_saga");

        assert_eq!(output["project"], "demo_saga");
        assert_eq!(output["template"], "saga");
        assert_eq!(output["worker"], "rust");
        let files = output["files"].as_array().ok_or("files must be an array")?;
        for relative in files {
            let relative = relative.as_str().ok_or("file entries must be strings")?;
            assert!(
                project.join(relative).is_file(),
                "{relative} must exist on disk"
            );
        }

        let gleam = std::fs::read_to_string(project.join("src/demo_saga.gleam"))?;
        assert!(gleam.contains("pub fn handle(input: OrderInput)"));
        assert!(gleam.contains("Generated plumbing"));
        let cargo = std::fs::read_to_string(project.join("worker/Cargo.toml"))?;
        assert!(cargo.contains(&format!("aion-worker = \"{}\"", env!("CARGO_PKG_VERSION"))));
        Ok(())
    }
}

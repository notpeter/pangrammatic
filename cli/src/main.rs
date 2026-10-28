//! `pangram`: a command line interface for the Pangram API.
//!
//! Async operations (AI detection tasks and bulk jobs) are driven to
//! completion by default: the command submits, polls until the task or job is
//! terminal, and prints the final response as JSON on stdout.
//!
//! With the default `annotate` feature, `pangram annotate` runs the PDF
//! annotation pipeline from the `annotate` crate (also built standalone as
//! `pangram-annotate`).

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use pangram_rs::{
    BulkRequest, BulkRequestItem, Config, Pangram, PollOptions, TaskRequest, UploadFile,
    DEFAULT_MODEL,
};
use serde::Serialize;

/// Exit code when the API reports a terminal failure (for example
/// `STAGE_FAILED`) even though every HTTP request succeeded.
const EXIT_TASK_FAILED: i32 = 3;

#[derive(Parser, Debug)]
#[command(
    name = "pangram",
    version,
    about = "Pangram AI detection, bulk, plagiarism, and file upload client"
)]
// Help strings are kept to one short sentence each so that `--help` fits a
// 100-column terminal with one line per option.
struct Cli {
    /// API key for the x-api-key header
    #[arg(
        long,
        value_name = "KEY",
        env = "PANGRAM_API_KEY",
        hide_env_values = true,
        global = true
    )]
    api_key: Option<String>,

    /// Base URL of the text API
    #[arg(long, value_name = "URL", env = "PANGRAM_TEXT_URL", default_value = pangram_rs::TEXT_BASE_URL, hide_default_value = true, hide_env_values = true, global = true)]
    text_url: String,

    /// Base URL of the plagiarism API
    #[arg(long, value_name = "URL", env = "PANGRAM_PLAGIARISM_URL", default_value = pangram_rs::PLAGIARISM_BASE_URL, hide_default_value = true, hide_env_values = true, global = true)]
    plagiarism_url: String,

    /// Base URL of the file upload API
    #[arg(long, value_name = "URL", env = "PANGRAM_FILE_UPLOAD_URL", default_value = pangram_rs::FILE_UPLOAD_BASE_URL, hide_default_value = true, hide_env_values = true, global = true)]
    file_upload_url: String,

    /// Seconds between status polls
    #[arg(long, value_name = "SECS", default_value_t = 2.0, global = true)]
    poll_interval: f64,

    /// Give up polling after this many seconds
    #[arg(long, value_name = "SECS", global = true)]
    timeout: Option<f64>,

    /// Write every request and response to this file as JSON
    #[arg(long, value_name = "FILE", global = true)]
    history: Option<PathBuf>,

    /// Print compact JSON
    #[arg(long, global = true)]
    compact: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List the model selectors available to the API key
    Models,

    /// Run AI detection on one text and print the result
    Detect {
        #[command(flatten)]
        input: TextInput,

        /// Model selector from `pangram models`
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,

        /// Request a public dashboard link
        #[arg(long)]
        public_dashboard_link: bool,

        /// Print the task id and return without polling
        #[arg(long)]
        no_wait: bool,
    },

    /// Fetch the status or result of an existing task
    Task {
        /// Id of the task
        task_id: String,

        /// Poll until the task is terminal
        #[arg(long)]
        wait: bool,
    },

    /// Bulk AI detection jobs
    Bulk {
        #[command(subcommand)]
        command: BulkCommand,
    },

    /// Check one text for plagiarism
    Plagiarism {
        #[command(flatten)]
        input: TextInput,
    },

    /// Upload files for AI detection and get dashboard links
    Upload {
        /// Files to upload
        #[arg(required = true)]
        files: Vec<PathBuf>,

        /// Request a public dashboard link for each file
        #[arg(long)]
        public_dashboard_link: bool,
    },

    /// Run AI detection on PDFs and write annotated copies
    // Writes its own history.json per output directory; --history is unused.
    #[cfg(feature = "annotate")]
    #[command(override_usage = "\
pangram annotate [OPTIONS] <PDF>... [-o <DIR>]
       pangram annotate [OPTIONS] --offline [PDF]... [-o <DIR>]")]
    Annotate(annotate::Options),
}

#[derive(Subcommand, Debug)]
enum BulkCommand {
    /// Submit a bulk job, poll it, and print all results
    Submit {
        /// Texts to analyze inline (or use --file)
        #[arg(conflicts_with = "files")]
        texts: Vec<String>,

        /// File to analyze; its name becomes the item id (repeatable)
        #[arg(long = "file", value_name = "FILE")]
        files: Vec<PathBuf>,

        /// Model selector from `pangram models`
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,

        /// Print the created job and return without polling
        #[arg(long)]
        no_wait: bool,
    },

    /// Fetch status and counters for a bulk job
    Status {
        /// Id of the bulk job
        bulk_id: String,

        /// Poll until the job is terminal
        #[arg(long)]
        wait: bool,
    },

    /// Fetch a page of item metadata
    Items {
        /// Id of the bulk job
        bulk_id: String,
        #[command(flatten)]
        page: Page,
    },

    /// Fetch a page of results, or all of them with --all
    Results {
        /// Id of the bulk job
        bulk_id: String,
        #[command(flatten)]
        page: Page,
        /// Fetch every page and print them combined
        #[arg(long, conflicts_with_all = ["offset", "limit"])]
        all: bool,
    },
}

#[derive(Args, Debug)]
struct Page {
    /// Zero-based item offset
    #[arg(long, value_name = "N")]
    offset: Option<u32>,
    /// Items per page (max 1000)
    #[arg(long, value_name = "N")]
    limit: Option<u32>,
}

#[derive(Args, Debug)]
struct TextInput {
    /// Text to analyze (stdin when neither TEXT nor --file is given)
    #[arg(conflicts_with = "file")]
    text: Option<String>,

    /// Read the text from this file
    #[arg(long, short = 'f', value_name = "FILE")]
    file: Option<PathBuf>,
}

impl TextInput {
    fn read(self) -> Result<String> {
        if let Some(text) = self.text {
            return Ok(text);
        }
        if let Some(path) = self.file {
            return std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()));
        }
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading text from stdin")?;
        if buf.trim().is_empty() {
            bail!("no input text: pass TEXT, --file, or pipe text on stdin");
        }
        Ok(buf)
    }
}

/// Wrapper so bulk `submit` can print the job status together with results.
#[derive(Serialize)]
struct BulkOutcome<'a> {
    status: &'a pangram_rs::BulkStatus,
    results: &'a pangram_rs::BulkResultsPage,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // The annotate pipeline reports progress at info level; everything else
    // prints JSON and stays quiet unless RUST_LOG asks otherwise.
    let default_level = match &cli.command {
        #[cfg(feature = "annotate")]
        Command::Annotate(_) => "info",
        _ => "error",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_level.into()),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let poll = PollOptions {
        interval: Duration::from_secs_f64(cli.poll_interval.max(0.1)),
        timeout: cli.timeout.map(Duration::from_secs_f64),
    };

    // Annotate manages its own client (and may need none at all with
    // --dry-run or --offline), so it runs before the API key is required.
    #[cfg(feature = "annotate")]
    if let Command::Annotate(options) = &cli.command {
        let conn = annotate::Connection {
            api_key: cli.api_key.clone(),
            text_url: cli.text_url.clone(),
            poll,
        };
        let code = annotate::run(options, &conn).await?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }

    let api_key = cli
        .api_key
        .clone()
        .context("an API key is required: pass --api-key or set PANGRAM_API_KEY")?;

    let mut config = Config::new(api_key);
    config.text_base_url = cli.text_url.clone();
    config.plagiarism_base_url = cli.plagiarism_url.clone();
    config.file_upload_base_url = cli.file_upload_url.clone();
    let client = Pangram::new(config)?;

    let outcome = run(&client, &poll, cli.command, cli.compact).await;

    if let Some(path) = &cli.history {
        client
            .history()
            .save(path)
            .with_context(|| format!("writing history to {}", path.display()))?;
    }

    match outcome {
        Ok(code) if code != 0 => std::process::exit(code),
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Runs one command. Returns the process exit code.
async fn run(client: &Pangram, poll: &PollOptions, command: Command, compact: bool) -> Result<i32> {
    let print = |value: &dyn erased::Serialize| -> Result<()> { emit(value, compact) };

    match command {
        Command::Models => {
            print(&client.list_models().await?)?;
        }

        Command::Detect {
            input,
            model,
            public_dashboard_link,
            no_wait,
        } => {
            let request = TaskRequest {
                text: input.read()?,
                model,
                public_dashboard_link,
            };
            let created = client.create_task(&request).await?;
            if no_wait {
                print(&created)?;
            } else {
                let status = client.wait_for_task(&created.task_id, poll).await?;
                print(&status)?;
                if status.is_failed() {
                    return Ok(EXIT_TASK_FAILED);
                }
            }
        }

        Command::Task { task_id, wait } => {
            let status = if wait {
                client.wait_for_task(&task_id, poll).await?
            } else {
                client.get_task(&task_id).await?
            };
            print(&status)?;
            if status.is_failed() {
                return Ok(EXIT_TASK_FAILED);
            }
        }

        Command::Bulk { command } => return run_bulk(client, poll, command, compact).await,

        Command::Plagiarism { input } => {
            print(&client.check_plagiarism(input.read()?).await?)?;
        }

        Command::Upload {
            files,
            public_dashboard_link,
        } => {
            let files = files
                .iter()
                .map(|p| {
                    UploadFile::from_path(p).with_context(|| format!("reading {}", p.display()))
                })
                .collect::<Result<Vec<_>>>()?;
            print(&client.upload_files(files, public_dashboard_link).await?)?;
        }

        #[cfg(feature = "annotate")]
        Command::Annotate(_) => {
            unreachable!("annotate is dispatched in main before a client exists")
        }
    }
    Ok(0)
}

async fn run_bulk(
    client: &Pangram,
    poll: &PollOptions,
    command: BulkCommand,
    compact: bool,
) -> Result<i32> {
    let print = |value: &dyn erased::Serialize| -> Result<()> { emit(value, compact) };

    match command {
        BulkCommand::Submit {
            texts,
            files,
            model,
            no_wait,
        } => {
            let mut request = BulkRequest {
                model,
                items: Vec::new(),
                text: Vec::new(),
            };
            if !files.is_empty() {
                for path in &files {
                    let text = std::fs::read_to_string(path)
                        .with_context(|| format!("reading {}", path.display()))?;
                    request.items.push(BulkRequestItem {
                        id: Some(path.to_string_lossy().into_owned()),
                        text,
                    });
                }
            } else if !texts.is_empty() {
                request.text = texts;
            } else {
                bail!("nothing to submit: pass TEXT arguments or --file FILE");
            }

            let created = client.create_bulk(&request).await?;
            if no_wait {
                print(&created)?;
                return Ok(0);
            }
            let status = client.wait_for_bulk(&created.bulk_id, poll).await?;
            let results = client.all_bulk_results(&created.bulk_id).await?;
            print(&BulkOutcome {
                status: &status,
                results: &results,
            })?;
            if status.status == pangram_rs::bulk_status::FAILED {
                return Ok(EXIT_TASK_FAILED);
            }
        }

        BulkCommand::Status { bulk_id, wait } => {
            let status = if wait {
                client.wait_for_bulk(&bulk_id, poll).await?
            } else {
                client.get_bulk(&bulk_id).await?
            };
            print(&status)?;
        }

        BulkCommand::Items { bulk_id, page } => {
            print(
                &client
                    .list_bulk_items(&bulk_id, page.offset, page.limit)
                    .await?,
            )?;
        }

        BulkCommand::Results { bulk_id, page, all } => {
            if all {
                print(&client.all_bulk_results(&bulk_id).await?)?;
            } else {
                print(
                    &client
                        .list_bulk_results(&bulk_id, page.offset, page.limit)
                        .await?,
                )?;
            }
        }
    }
    Ok(0)
}

fn emit(value: &dyn erased::Serialize, compact: bool) -> Result<()> {
    let value = value.to_json_value()?;
    let text = if compact {
        serde_json::to_string(&value)?
    } else {
        serde_json::to_string_pretty(&value)?
    };
    println!("{text}");
    Ok(())
}

/// A tiny object-safe shim so `print` can accept any `Serialize` value.
mod erased {
    pub trait Serialize {
        fn to_json_value(&self) -> serde_json::Result<serde_json::Value>;
    }

    impl<T: serde::Serialize> Serialize for T {
        fn to_json_value(&self) -> serde_json::Result<serde_json::Value> {
            serde_json::to_value(self)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }
}

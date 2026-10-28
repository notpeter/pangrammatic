//! `pangram-annotate`: extract the text of a PDF, run Pangram AI detection on
//! it, and write an annotated copy that highlights each classified window.
//!
//! The pipeline lives in the `annotate` library; this binary only adds the
//! API connection flags. The same options are available as
//! `pangram annotate` in the CLI.

use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use pangram_rs::PollOptions;

#[derive(Parser, Debug)]
#[command(name = "pangram-annotate", version, about)]
#[command(override_usage = "\
pangram-annotate [OPTIONS] <PDF>... [-o <DIR>]
       pangram-annotate [OPTIONS] --offline [PDF]... [-o <DIR>]")]
struct Cli {
    #[command(flatten)]
    options: annotate::Options,

    /// API key for the x-api-key header
    #[arg(long, value_name = "KEY", env = "PANGRAM_API_KEY", hide_env_values = true, required_unless_present_any = ["dry_run", "offline"])]
    api_key: Option<String>,

    /// Base URL of the text API
    #[arg(long, value_name = "URL", env = "PANGRAM_TEXT_URL", default_value = pangram_rs::TEXT_BASE_URL, hide_default_value = true, hide_env_values = true)]
    text_url: String,

    /// Seconds between status polls
    #[arg(long, value_name = "SECS", default_value_t = 2.0)]
    poll_interval: f64,

    /// Give up polling after this many seconds
    #[arg(long, value_name = "SECS")]
    timeout: Option<f64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let conn = annotate::Connection {
        api_key: cli.api_key,
        text_url: cli.text_url,
        poll: PollOptions {
            interval: Duration::from_secs_f64(cli.poll_interval.max(0.1)),
            timeout: cli.timeout.map(Duration::from_secs_f64),
        },
    };
    let code = annotate::run(&cli.options, &conn).await?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn api_key_is_optional_without_requests() {
        assert!(Cli::try_parse_from(["pangram-annotate", "--dry-run", "a.pdf"]).is_ok());
        assert!(Cli::try_parse_from(["pangram-annotate", "--offline", "-o", "x"]).is_ok());
    }

    #[test]
    fn only_the_pdf_is_reported_missing() {
        let err = Cli::try_parse_from(["pangram-annotate", "--api-key", "k"]).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("<PDF>"), "{text}");
        assert!(!text.contains("--output"), "{text}");
    }
}

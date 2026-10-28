//! Extract the text of a PDF, run Pangram AI detection on it, and write an
//! annotated copy that highlights each classified window.
//!
//! [`run`] is the whole pipeline. The `pangram-annotate` binary and the
//! `pangram annotate` subcommand of the CLI both parse an [`Options`] and
//! call it; the API key and polling settings come separately as a
//! [`Connection`] because each front end has its own flags for them.
//!
//! Each PDF is submitted as its own task (`POST /task`). `--bulk` submits all
//! of them together as one bulk job (`POST /bulk`) instead.
//! `--offline` makes no API calls and re-annotates from the `response.json`
//! already in the output directory, so the PDF output can be iterated on
//! without spending credits.
//!
//! Output directory layout (one per PDF; `-o`, defaulting to `out/<PDF stem>`):
//!
//! * `original.pdf`: a copy of the input
//! * `original.txt`: the extracted plain text that was sent to Pangram
//! * `history.json`: every request and response made
//! * `response.json`: the document's task response (with `--bulk`, its item's
//!   result from the bulk job)
//! * `annotated.pdf`: the input with highlight annotations per window

mod align;
mod annotate;
mod extract;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Args;
use pangram_rs::{
    bulk_status, label, BulkItem, BulkRequest, BulkRequestItem, BulkResultsPage, Config, History,
    Pangram, PollOptions, TaskRequest, TaskStatus, DEFAULT_MODEL,
};

use align::Alignment;
use annotate::{Annotation, Highlight, Note};
use pangram_rs::Window;

/// What to annotate and how. Flattened into each front end's argument
/// parser, so the flags read the same in `pangram-annotate` and in
/// `pangram annotate`.
#[derive(Args, Debug)]
// Help strings are kept to one short sentence each so that `--help` fits a
// 100-column terminal with one line per option. The details live in the
// README and the module docs above.
pub struct Options {
    /// PDFs to analyze (optional with --offline)
    #[arg(value_name = "PDF", required_unless_present = "offline")]
    pub inputs: Vec<PathBuf>,

    /// Output directory, or parent of one per PDF [default: out/<stem>]
    #[arg(short = 'o', long = "output", value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Model selector (see `pangram models`)
    #[arg(long, default_value = DEFAULT_MODEL)]
    pub model: String,

    /// Submit all PDFs as one bulk job instead of one task each
    #[arg(long, conflicts_with = "public_dashboard_link")]
    pub bulk: bool,

    /// Request a public dashboard link (not with --bulk)
    #[arg(long)]
    pub public_dashboard_link: bool,

    /// Extract text only; do not call the API
    #[arg(long, conflicts_with = "offline")]
    pub dry_run: bool,

    /// Rebuild annotated.pdf from saved response.json; no API calls
    #[arg(long)]
    pub offline: bool,
}

/// How to reach the API. Only used when the [`Options`] call for requests
/// (neither `--dry-run` nor `--offline`), so `api_key` may be `None` until
/// then.
#[derive(Debug, Clone)]
pub struct Connection {
    pub api_key: Option<String>,
    pub text_url: String,
    pub poll: PollOptions,
}

/// Exit code when Pangram reports a terminal failure for at least one
/// document even though every HTTP request succeeded.
pub const EXIT_TASK_FAILED: i32 = 3;
/// Blocks with fewer words than this (list markers such as "1.", stray
/// headings) are highlighted but get no margin note, so their note does not
/// land on top of the note for the block that follows on the same line.
const MIN_NOTE_WORDS: u32 = 3;

/// A classification result plus the raw response body to store.
struct Outcome {
    status: TaskStatus,
    response_json: serde_json::Value,
}

/// A PDF paired with the directory its results go to. `input` is `None`
/// only in offline mode, where the directory's `original.pdf` is used.
#[derive(Debug, PartialEq)]
struct Planned {
    input: Option<PathBuf>,
    dir: PathBuf,
}

/// One PDF being processed: the loaded document, its text, and its paths.
struct Document {
    /// The PDF as shown to the user: the given path, or the copy in the
    /// output directory in offline mode.
    input: PathBuf,
    out: Paths,
    pdf: lopdf::Document,
    extracted: extract::Extracted,
    text: String,
}

/// Runs the pipeline for every PDF in `opts`. Returns the process exit
/// code: `0`, or [`EXIT_TASK_FAILED`] when Pangram could not classify one
/// or more documents (the others are still annotated).
pub async fn run(opts: &Options, conn: &Connection) -> Result<i32> {
    let offline = opts.offline;
    let plan = plan_documents(opts)?;

    // 1. Copy each PDF into its output directory and extract its text.
    let mut docs = Vec::with_capacity(plan.len());
    for planned in plan {
        docs.push(prepare(planned, offline)?);
    }
    if opts.dry_run {
        return Ok(0);
    }

    // 2. Classify: via the API, or from saved responses.
    let outcomes = if offline {
        let mut outcomes = Vec::with_capacity(docs.len());
        for doc in &docs {
            if !doc.out.response.is_file() {
                bail!(
                    "{} does not exist; run once without --offline first",
                    doc.out.response.display()
                );
            }
            outcomes.push(load_saved_response(&doc.out.response)?);
        }
        outcomes
    } else {
        let api_key = conn
            .api_key
            .clone()
            .context("--api-key or PANGRAM_API_KEY is required")?;
        let mut config = Config::new(api_key);
        config.text_base_url = conn.text_url.clone();
        let client = Pangram::new(config)?;
        let poll = &conn.poll;

        // The history is always persisted, even when the API call failed.
        if opts.bulk {
            let result = run_bulk(&client, &docs, &opts.model, poll).await;
            // One job covers every document, so each directory gets the
            // whole exchange.
            let history = client.history();
            for doc in &docs {
                save_history(&history, &doc.out)?;
            }
            result?
        } else {
            let mut outcomes = Vec::with_capacity(docs.len());
            for doc in &docs {
                let result = run_task(
                    &client,
                    &doc.text,
                    &opts.model,
                    opts.public_dashboard_link,
                    poll,
                )
                .await;
                save_history(&client.take_history(), &doc.out)?;
                outcomes.push(result?);
            }
            outcomes
        }
    };

    // 3. Store each response and annotate its PDF.
    let mut failed = 0usize;
    for (doc, outcome) in docs.iter_mut().zip(outcomes) {
        if !offline {
            std::fs::write(
                &doc.out.response,
                serde_json::to_vec_pretty(&outcome.response_json)?,
            )
            .with_context(|| format!("writing {}", doc.out.response.display()))?;
            tracing::info!("wrote {}", doc.out.response.display());
        }

        let status = outcome.status;
        if !status.is_success() {
            tracing::error!(
                document = %doc.input.display(),
                stage = %status.stage,
                headline = status.headline.as_deref().unwrap_or(""),
                "Pangram did not classify the document"
            );
            failed += 1;
            continue;
        }
        annotate_document(doc, &status)?;
        println!(
            "{}: {}: {} (ai {:.0}%, ai-assisted {:.0}%, human {:.0}%)",
            doc.input.display(),
            status.headline.as_deref().unwrap_or("done"),
            status.prediction_short.as_deref().unwrap_or("?"),
            100.0 * status.fraction_ai.unwrap_or(0.0),
            100.0 * status.fraction_ai_assisted.unwrap_or(0.0),
            100.0 * status.fraction_human.unwrap_or(0.0),
        );
    }
    Ok(if failed > 0 { EXIT_TASK_FAILED } else { 0 })
}

struct Paths {
    pdf: PathBuf,
    txt: PathBuf,
    history: PathBuf,
    response: PathBuf,
    annotated: PathBuf,
}

impl Paths {
    fn new(dir: &Path) -> Self {
        Self {
            pdf: dir.join("original.pdf"),
            txt: dir.join("original.txt"),
            history: dir.join("history.json"),
            response: dir.join("response.json"),
            annotated: dir.join("annotated.pdf"),
        }
    }
}

/// Pairs every PDF with its output directory: `-o` itself for a single PDF,
/// `-o/<stem>` for several, and `out/<stem>` when `-o` is not given. With
/// no PDF at all (offline), `-o` alone names the document.
fn plan_documents(opts: &Options) -> Result<Vec<Planned>> {
    if opts.inputs.is_empty() {
        let dir = opts
            .output
            .clone()
            .context("--offline needs a PDF or --output <DIR> to know which directory to use")?;
        return Ok(vec![Planned { input: None, dir }]);
    }
    let mut plan: Vec<Planned> = Vec::with_capacity(opts.inputs.len());
    for input in &opts.inputs {
        let dir = match (&opts.output, opts.inputs.len()) {
            (Some(dir), 1) => dir.clone(),
            (Some(parent), _) => parent.join(stem(input)?),
            (None, _) => Path::new("out").join(stem(input)?),
        };
        if let Some(other) = plan.iter().find(|p| p.dir == dir) {
            bail!(
                "{} and {} would both write to {}; rename one or pass separate --output directories",
                other.input.as_deref().unwrap_or(Path::new("?")).display(),
                input.display(),
                dir.display()
            );
        }
        plan.push(Planned {
            input: Some(input.clone()),
            dir,
        });
    }
    Ok(plan)
}

/// The file name of `input` without its extension.
fn stem(input: &Path) -> Result<&std::ffi::OsStr> {
    input
        .file_stem()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("cannot derive an output directory from {}", input.display()))
}

/// Copies the PDF into its output directory (unless offline), loads it, and
/// extracts its text into `original.txt`.
fn prepare(planned: Planned, offline: bool) -> Result<Document> {
    let Planned { input, dir } = planned;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let out = Paths::new(&dir);
    tracing::info!("output directory {}", dir.display());

    let input = match (input, offline) {
        (Some(input), false) => {
            std::fs::copy(&input, &out.pdf)
                .with_context(|| format!("copying {} to {}", input.display(), out.pdf.display()))?;
            tracing::info!("wrote {}", out.pdf.display());
            input
        }
        (Some(input), true) => input,
        (None, true) => {
            if !out.pdf.is_file() {
                bail!(
                    "{} does not exist; pass the PDF or run once without --offline",
                    out.pdf.display()
                );
            }
            out.pdf.clone()
        }
        (None, false) => bail!("a PDF is required"),
    };

    let pdf =
        lopdf::Document::load(&input).with_context(|| format!("loading {}", input.display()))?;
    if pdf.is_encrypted() {
        bail!("{} is encrypted; decrypt it first", input.display());
    }
    let extracted =
        extract::extract(&pdf).map_err(|e| anyhow::anyhow!("extracting text: {e:?}"))?;
    let text = extracted.text();
    std::fs::write(&out.txt, &text).with_context(|| format!("writing {}", out.txt.display()))?;
    tracing::info!(
        pages = extracted.page_count,
        chars = extracted.chars.len(),
        "wrote {}",
        out.txt.display()
    );
    if extracted.is_blank() {
        bail!(
            "no text could be extracted from {} (scanned PDF?)",
            input.display()
        );
    }
    Ok(Document {
        input,
        out,
        pdf,
        extracted,
        text,
    })
}

fn save_history(history: &History, out: &Paths) -> Result<()> {
    history
        .save(&out.history)
        .with_context(|| format!("writing {}", out.history.display()))?;
    tracing::info!("wrote {}", out.history.display());
    Ok(())
}

/// Writes `annotated.pdf`: the document summary first, then for each block
/// its note followed by the highlights it covers.
fn annotate_document(doc: &mut Document, status: &TaskStatus) -> Result<()> {
    let mut annotations = vec![Annotation::Note(Note {
        page: 1,
        anchor: None,
        color: (0.2, 0.4, 1.0),
        icon: "Note",
        title: "Pangram".into(),
        contents: summary(status),
        name: "pangram-summary".into(),
    })];
    annotations.extend(build_annotations(&doc.extracted, status));
    let added = annotate::apply(&mut doc.pdf, &annotations)?;
    doc.pdf
        .save(&doc.out.annotated)
        .with_context(|| format!("writing {}", doc.out.annotated.display()))?;
    tracing::info!(
        annotations = added,
        windows = status.windows.len(),
        "wrote {}",
        doc.out.annotated.display()
    );
    Ok(())
}

/// Submits every document as an item of one bulk job and polls it to
/// completion. Each document's stored response is its own item's result.
async fn run_bulk(
    client: &Pangram,
    docs: &[Document],
    model: &str,
    poll: &PollOptions,
) -> Result<Vec<Outcome>> {
    let request = BulkRequest {
        items: docs
            .iter()
            .enumerate()
            .map(|(i, doc)| BulkRequestItem {
                id: Some(item_id(doc, i)),
                text: doc.text.clone(),
            })
            .collect(),
        text: Vec::new(),
        model: model.to_string(),
    };
    let created = client.create_bulk(&request).await.context("POST /bulk")?;
    tracing::info!(
        bulk_id = %created.bulk_id,
        status = %created.status,
        items = docs.len(),
        "bulk job created; polling"
    );
    if created.status == bulk_status::FAILED {
        let reason = created
            .failed_items
            .first()
            .and_then(|f| f.error.clone())
            .unwrap_or_else(|| "unknown".into());
        bail!(
            "bulk job {} rejected every document: {reason}",
            created.bulk_id
        );
    }

    let bulk = client
        .wait_for_bulk(&created.bulk_id, poll)
        .await
        .context("polling GET /bulk/{bulk_id}")?;
    tracing::info!(status = %bulk.status, "bulk job finished");

    let results = client
        .all_bulk_results(&created.bulk_id)
        .await
        .context("GET /bulk/{bulk_id}/results")?;
    (0..docs.len() as u32)
        .map(|index| {
            let status = item_status(&results, &created.failed_items, index)?;
            Ok(Outcome {
                response_json: serde_json::to_value(&status)?,
                status,
            })
        })
        .collect()
}

/// The bulk item id for a document: its file stem, or its position.
fn item_id(doc: &Document, index: usize) -> String {
    doc.input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| index.to_string())
}

/// The result for item `index` of a bulk job. An item without a result, or
/// one that was rejected (`rejected` holds the items refused when the job
/// was created), becomes a `STAGE_FAILED`-style status so the caller handles
/// both paths alike.
fn item_status(results: &BulkResultsPage, rejected: &[BulkItem], index: u32) -> Result<TaskStatus> {
    if let Some(item) = results.items.iter().find(|i| i.index == index) {
        return Ok(match &item.result {
            Some(status) => status.clone(),
            None => TaskStatus {
                stage: item.stage.clone(),
                headline: item.error.clone(),
                task_id: item.task_id.clone(),
                ..empty_status()
            },
        });
    }
    if let Some(failed) = results
        .failed_items
        .iter()
        .chain(rejected)
        .find(|f| f.index == index)
    {
        return Ok(TaskStatus {
            stage: failed.stage.clone(),
            headline: failed.error.clone(),
            task_id: failed.task_id.clone(),
            ..empty_status()
        });
    }
    bail!(
        "bulk job {} returned nothing for item {index}",
        results.bulk_id
    )
}

fn empty_status() -> TaskStatus {
    TaskStatus {
        dashboard_link: None,
        fraction_ai: None,
        fraction_ai_assisted: None,
        fraction_human: None,
        headline: None,
        num_ai_assisted_segments: None,
        num_ai_segments: None,
        num_human_segments: None,
        prediction: None,
        prediction_short: None,
        stage: String::new(),
        task_id: None,
        text: None,
        version: None,
        windows: Vec::new(),
    }
}

/// Submits one document as a task and polls it to completion.
async fn run_task(
    client: &Pangram,
    text: &str,
    model: &str,
    public_dashboard_link: bool,
    poll: &PollOptions,
) -> Result<Outcome> {
    let request = TaskRequest {
        text: text.to_string(),
        model: model.to_string(),
        public_dashboard_link,
    };
    let created = client.create_task(&request).await.context("POST /task")?;
    tracing::info!(task_id = %created.task_id, "task created; polling");
    let status = client
        .wait_for_task(&created.task_id, poll)
        .await
        .context("polling GET /task/{task_id}")?;
    tracing::info!(stage = %status.stage, "task finished");
    let response_json = serde_json::to_value(&status)?;
    Ok(Outcome {
        status,
        response_json,
    })
}

/// Reads a saved `response.json`: a task response, or a single-item bulk
/// results page written by an earlier version.
fn load_saved_response(path: &Path) -> Result<Outcome> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    let status = if value.get("bulk_id").is_some() {
        let page: BulkResultsPage = serde_json::from_value(value.clone())
            .with_context(|| format!("parsing {} as a bulk results page", path.display()))?;
        item_status(&page, &[], 0)?
    } else {
        serde_json::from_value(value.clone())
            .with_context(|| format!("parsing {} as a task response", path.display()))?
    };
    Ok(Outcome {
        status,
        response_json: value,
    })
}

/// For every window, a margin note carrying the window's stats followed by
/// one highlight per page the window touches.
fn build_annotations(extracted: &extract::Extracted, status: &TaskStatus) -> Vec<Annotation> {
    let remote: Vec<char> = status.text.as_deref().unwrap_or("").chars().collect();
    let alignment = Alignment::new(&extracted.chars, &remote);
    let coverage = alignment.coverage(&remote);
    if coverage < 0.95 {
        tracing::warn!(
            coverage = format!("{:.1}%", coverage * 100.0),
            "returned text differs noticeably from the submitted text; highlights may be incomplete"
        );
    }

    // Left edge of the text on each page, so notes form one column in the
    // margin instead of sitting on list markers or indents.
    let mut page_left: HashMap<u32, f64> = HashMap::new();
    for g in extracted.glyphs.iter().flatten() {
        let left = page_left.entry(g.page).or_insert(f64::INFINITY);
        *left = left.min(g.x0);
    }

    let mut annotations = Vec::new();
    let total = status.windows.len();
    for (i, w) in status.windows.iter().enumerate() {
        let glyphs: Vec<_> = alignment
            .local_indices(w.start_index as usize, w.end_index as usize)
            .into_iter()
            .filter_map(|li| extracted.glyphs[li])
            .collect();
        if glyphs.is_empty() {
            tracing::warn!(window = i, label = %w.label, "no glyphs found for window; skipping");
            continue;
        }
        let title = format!("Pangram block {} of {total}: {}", i + 1, w.label);
        let contents = window_stats(w, i, total);
        let color = color_for(&w.label, &w.confidence);

        let groups = annotate::group_lines(&glyphs);
        tracing::debug!(
            window = i + 1,
            label = %w.label,
            remote = format!("{}-{}", w.start_index, w.end_index),
            glyphs = glyphs.len(),
            lines = groups.iter().map(|(_, r)| r.len()).sum::<usize>(),
            pages = ?groups.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            "window mapped"
        );
        if let Some((page, rects)) = groups.first().filter(|_| w.word_count >= MIN_NOTE_WORDS) {
            // Put the note in the margin, left of the page's text and level
            // with the top of the block's first line.
            let first = rects[0];
            let left = page_left
                .get(page)
                .copied()
                .unwrap_or(first.x0)
                .min(first.x0);
            annotations.push(Annotation::Note(Note {
                page: *page,
                anchor: Some((left - 26.0, first.y1 - 18.0)),
                color,
                icon: "Note",
                title: title.clone(),
                contents: contents.clone(),
                name: format!("pangram-block-{}", i + 1),
            }));
        }
        for (page, rects) in groups {
            annotations.push(Annotation::Highlight(Highlight {
                page,
                rects,
                color,
                title: title.clone(),
                contents: contents.clone(),
            }));
        }
    }
    annotations
}

/// The per-window statistics shown in a block's note and highlight popup.
fn window_stats(w: &Window, index: usize, total: usize) -> String {
    let mut s = format!(
        "Block {} of {total}: {}\nConfidence: {}\nAI assistance score: {:.3}",
        index + 1,
        w.label,
        w.confidence,
        w.ai_assistance_score,
    );
    match (w.is_humanized, w.humanizer_score) {
        (Some(h), Some(score)) => {
            s.push_str(&format!("\nHumanized: {} (score {score:.3})", yes_no(h)))
        }
        (Some(h), None) => s.push_str(&format!("\nHumanized: {}", yes_no(h))),
        (None, Some(score)) => s.push_str(&format!("\nHumanizer score: {score:.3}")),
        (None, None) => {}
    }
    s.push_str(&format!(
        "\nWords: {}\nTokens: {}\nCharacters: {}-{} ({} chars)\n\n{}",
        w.word_count,
        w.token_length,
        w.start_index,
        w.end_index,
        w.end_index.saturating_sub(w.start_index),
        excerpt(&w.text),
    ));
    s
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

/// Highlight colour for a window: the label picks the hue and the
/// confidence picks how strong it is, so a low-confidence block is a pale
/// tint of the same colour as a high-confidence one.
fn color_for(label_text: &str, confidence: &str) -> (f32, f32, f32) {
    let lower = label_text.to_ascii_lowercase();
    let base = if label_text == label::HUMAN_WRITTEN || lower.contains("human") {
        (0.30, 0.80, 0.30) // green
    } else if label_text == label::AI_ASSISTED || lower.contains("assist") {
        (1.0, 0.70, 0.0) // amber
    } else if label_text == label::AI_GENERATED || lower.contains("ai") {
        (1.0, 0.30, 0.30) // red
    } else {
        (0.60, 0.60, 0.60) // grey
    };
    // Fraction of the base colour kept; the rest is white.
    let strength = match confidence.to_ascii_lowercase().as_str() {
        "high" => 0.70,
        "medium" => 0.45,
        "low" => 0.22,
        _ => 0.45,
    };
    let mix = |c: f32| 1.0 - (1.0 - c) * strength;
    (mix(base.0), mix(base.1), mix(base.2))
}

/// The first line of a block, cut to 30 characters.
fn excerpt(text: &str) -> String {
    const MAX: usize = 30;
    let trimmed = text.trim();
    let first_line = trimmed.lines().next().unwrap_or("");
    let short: String = first_line.chars().take(MAX).collect();
    if short.chars().count() < trimmed.chars().count() {
        format!("{}...", short.trim_end())
    } else {
        short
    }
}

fn summary(status: &TaskStatus) -> String {
    let mut s = format!(
        "Pangram {}\n{}\n{}\n\nAI: {:.1}%\nAI-assisted: {:.1}%\nHuman: {:.1}%\nSegments: {} AI, {} AI-assisted, {} human\nBlocks: {} (each has its own note in the margin)\n\nColours: red = AI-Generated, amber = AI-Assisted, green = Human Written.\nStrong = high confidence, medium = medium, pale = low.",
        status.version.as_deref().unwrap_or(""),
        status.headline.as_deref().unwrap_or(""),
        status.prediction.as_deref().unwrap_or(""),
        100.0 * status.fraction_ai.unwrap_or(0.0),
        100.0 * status.fraction_ai_assisted.unwrap_or(0.0),
        100.0 * status.fraction_human.unwrap_or(0.0),
        status.num_ai_segments.unwrap_or(0),
        status.num_ai_assisted_segments.unwrap_or(0),
        status.num_human_segments.unwrap_or(0),
        status.windows.len(),
    );
    if let Some(link) = &status.dashboard_link {
        s.push_str("\n\n");
        s.push_str(link);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// A parser with nothing but the shared options, so the tests cover
    /// what both front ends flatten in.
    #[derive(Parser, Debug)]
    struct Cli {
        #[command(flatten)]
        opts: Options,
    }

    fn parse(args: &[&str]) -> Options {
        Cli::try_parse_from(std::iter::once("annotate").chain(args.iter().copied()))
            .unwrap()
            .opts
    }

    fn dirs(plan: &[Planned]) -> Vec<&Path> {
        plan.iter().map(|p| p.dir.as_path()).collect()
    }

    #[test]
    fn pdfs_are_positional_and_output_is_optional() {
        let opts = parse(&["--dry-run", "a.pdf", "b.pdf"]);
        assert_eq!(
            opts.inputs,
            [PathBuf::from("a.pdf"), PathBuf::from("b.pdf")]
        );
        assert!(opts.output.is_none());
        assert!(!opts.bulk);
    }

    #[test]
    fn bulk_conflicts_with_dashboard_link() {
        let args = [
            "annotate",
            "--dry-run",
            "--bulk",
            "--public-dashboard-link",
            "a.pdf",
        ];
        assert!(Cli::try_parse_from(args).is_err());
        assert!(parse(&["--dry-run", "--bulk", "a.pdf"]).bulk);
    }

    #[test]
    fn output_directories() {
        let plan = plan_documents(&parse(&["--dry-run", "x/doc.pdf"])).unwrap();
        assert_eq!(dirs(&plan), [Path::new("out/doc")]);

        let plan = plan_documents(&parse(&["--dry-run", "-o", "res", "x/doc.pdf"])).unwrap();
        assert_eq!(dirs(&plan), [Path::new("res")]);

        let plan = plan_documents(&parse(&["--dry-run", "a.pdf", "x/b.pdf"])).unwrap();
        assert_eq!(dirs(&plan), [Path::new("out/a"), Path::new("out/b")]);

        let plan = plan_documents(&parse(&["--dry-run", "-o", "res", "a.pdf", "b.pdf"])).unwrap();
        assert_eq!(dirs(&plan), [Path::new("res/a"), Path::new("res/b")]);
    }

    #[test]
    fn colliding_output_directories_are_rejected() {
        let err = plan_documents(&parse(&["--dry-run", "x/a.pdf", "y/a.pdf"])).unwrap_err();
        assert!(err.to_string().contains("out/a"), "{err}");
    }

    #[test]
    fn offline_needs_pdf_or_output() {
        assert!(plan_documents(&parse(&["--offline"])).is_err());

        let plan = plan_documents(&parse(&["--offline", "-o", "out/x"])).unwrap();
        assert_eq!(
            plan,
            [Planned {
                input: None,
                dir: PathBuf::from("out/x")
            }]
        );

        let plan = plan_documents(&parse(&["--offline", "doc.pdf"])).unwrap();
        assert_eq!(dirs(&plan), [Path::new("out/doc")]);
    }
}

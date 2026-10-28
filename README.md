# pangrammatic

Unofficial OpenAPI specification, Rust SDK and examples for the
[Pangram REST API](https://docs.pangram.com/api-reference/introduction).

## OpenAPI specifications

- [spec/pangram-file-upload.json](./spec/pangram-file-upload.json)
- [spec/pangram-plagiarism.json](./spec/pangram-plagiarism.json)
- [spec/pangram-text.json](./spec/pangram-text.json)

These were built from the Pangram website and public API docs and observed
responses.

## pangram-rs: Unofficial Pangram Rust SDK

Rust SDK generated from the above OpenAPI specifications using
[oxidecomputer/progenitor](https://github.com/oxidecomputer/progenitor).

## annotate: Example PDF Annotation app

Example consumer of the Rust SDK. It builds as the standalone
`pangram-annotate` binary and is also the `pangram annotate` subcommand of
the CLI below (default feature `annotate`).

1. User sets an environment variable with their pangram API key:
   ```
   export PANGRAM_API_KEY='sk-pg-aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999'
   ```
1. `pangram-annotate` takes a user provided PDF
1. Converts it to plaintext
1. Sends that text as an AI detection task to the Pangram API
1. Polls waiting for a response from the Pangram API
1. Uses the response and the `lopdf` crate to annotate the original pdf.

For debugging / visibility it creates a copies of all data used in this process
in a subdirectory under `out/` so for `pangram-annotate SamSparc-resume.pdf`
(or `pangram annotate SamSparc-resume.pdf`):

```
out/SamSparc-resume/original.pdf   # copy of the original PDF
out/SamSparc-resume/original.txt   # extracted text from the PDF
out/SamSparc-resume/history.json   # history of all API requests/responses
out/SamSparc-resume/response.json  # final response from the API
```

### pangram-annotate options

```
Extracts text from a PDF, runs Pangram AI detection, and writes an annotated PDF

Usage: pangram-annotate [OPTIONS] <PDF>... [-o <DIR>]
       pangram-annotate [OPTIONS] --offline [PDF]... [-o <DIR>]

Arguments:
  [PDF]...  PDFs to analyze (optional with --offline)

Options:
  -o, --output <DIR>           Output directory, or parent of one per PDF [default: out/<stem>]
      --model <MODEL>          Model selector (see `pangram models`) [default: default]
      --bulk                   Submit all PDFs as one bulk job instead of one task each
      --public-dashboard-link  Request a public dashboard link (not with --bulk)
      --dry-run                Extract text only; do not call the API
      --offline                Rebuild annotated.pdf from saved response.json; no API calls
      --api-key <KEY>          API key for the x-api-key header [env: PANGRAM_API_KEY]
      --text-url <URL>         Base URL of the text API [env: PANGRAM_TEXT_URL]
      --poll-interval <SECS>   Seconds between status polls [default: 2]
      --timeout <SECS>         Give up polling after this many seconds
  -h, --help                   Print help
  -V, --version                Print version
```

Note, use of --bulk changes the shape of `response.json`.

## cli

Command line interface for the Pangram API, built as `pangram`. It is the
workspace's default member, so `cargo run` at the repository root runs it.

```shell
export PANGRAM_API_KEY='sk-pg-aaaabbbbccccddddeeeeffff0000111122223333444455556666777788889999'
cargo run -- models
pangram annotate SamSparc-resume.pdf
```

The `annotate` subcommand is behind the default `annotate` feature; build
with `--no-default-features` for a client without the PDF dependencies.

## Development

- After editing OpenAPI specs, regenerate SDK with `cargo xtask generate`
- `cargo build` and `cargo test` at the root act on `cli` only; pass
  `--workspace` to cover every crate

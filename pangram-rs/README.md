# pangram-rs

Rust client for the [Pangram API](https://docs.pangram.com/api-reference/introduction):
AI detection tasks, bulk jobs, plagiarism checks, and file uploads.

Pangram serves its API from three hosts. The text and plagiarism clients are
generated from OpenAPI documents with
[progenitor](https://github.com/oxidecomputer/progenitor) and exposed as
`pangram_rs::text` and `pangram_rs::plagiarism`. The multipart file upload
endpoint is implemented by hand. `Pangram` wraps all three behind one API key,
adds blocking-style helpers that poll tasks and bulk jobs to completion, and
records every request and response in a `History`.

## Usage

```rust
use pangram_rs::{Pangram, PollOptions, TaskRequest};

let client = Pangram::with_api_key(std::env::var("PANGRAM_API_KEY")?)?;
let result = client
    .detect(
        &TaskRequest { text: text.into(), model: "pangram-4".into(), public_dashboard_link: false },
        &PollOptions::default(),
    )
    .await?;
for w in &result.windows {
    println!("{:>6}..{:<6} {} ({})", w.start_index, w.end_index, w.label, w.confidence);
}
let history = client.history(); // every request and response made so far
```

`Config` exposes the base URL of each host and the per-request timeout, for
pointing the client at a proxy or a mock server.

## Errors

Every non-2xx response is returned as `Error::Api { status, headers, body, .. }`
with the body kept raw, whether or not it was JSON. A task that ends in
`STAGE_FAILED` is returned as `Ok`; check `TaskStatus::is_failed`.

## License

MPL-2.0

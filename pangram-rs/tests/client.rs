//! Exercises the generated clients and the `Pangram` facade against a mock
//! server using the example payloads from the Pangram documentation.

use std::time::Duration;

use pangram_rs::{BulkRequest, BulkRequestItem, Config, Error, Pangram, PollOptions, TaskRequest};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "test-key";

async fn client(server: &MockServer) -> Pangram {
    let mut config = Config::new(KEY);
    config.text_base_url = server.uri();
    config.plagiarism_base_url = server.uri();
    config.file_upload_base_url = server.uri();
    Pangram::new(config).unwrap()
}

fn fast() -> PollOptions {
    PollOptions {
        interval: Duration::from_millis(10),
        timeout: Some(Duration::from_secs(5)),
    }
}

#[tokio::test]
async fn list_models_sends_api_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": ["default", "pangram-4"]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server).await;
    let models = c.list_models().await.unwrap();
    assert_eq!(models.models, vec!["default", "pangram-4"]);

    let history = c.history();
    assert_eq!(history.len(), 1);
    let entry = &history.entries[0];
    assert_eq!(entry.verb, "GET");
    assert_eq!(entry.request_url, format!("{}/models", server.uri()));
    assert_eq!(entry.status, Some(200));
    assert_eq!(entry.response_body["models"][1], "pangram-4");
    assert!(entry.response_headers.contains_key("content-type"));
}

#[tokio::test]
async fn detect_polls_until_terminal() {
    let server = MockServer::start().await;
    let task_id = "123e4567-e89b-12d3-a456-426614174000";

    Mock::given(method("POST"))
        .and(path("/task"))
        .and(body_json(json!({
            "text": "AI-assisted passage. Human passage.",
            "model": "pangram-4",
            "public_dashboard_link": false
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "task_id": task_id })))
        .expect(1)
        .mount(&server)
        .await;

    // First poll: in progress. Second poll: success.
    Mock::given(method("GET"))
        .and(path(format!("/task/{task_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "task_id": task_id,
            "stage": "STAGE_PREPROCESSING"
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/task/{task_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "stage": "STAGE_SUCCESS",
            "text": "AI-assisted passage. Human passage.",
            "version": "4.0",
            "headline": "AI Assisted",
            "prediction": "We believe that this text is a mix of AI-assisted and human-written content.",
            "prediction_short": "Mixed",
            "fraction_ai": 0.0,
            "fraction_ai_assisted": 0.6,
            "fraction_human": 0.4,
            "num_ai_segments": 0,
            "num_ai_assisted_segments": 1,
            "num_human_segments": 1,
            "windows": [
                {
                    "text": "AI-assisted passage. ",
                    "label": "AI-Assisted",
                    "ai_assistance_score": 0.55,
                    "confidence": "High",
                    "start_index": 0,
                    "end_index": 21,
                    "word_count": 2,
                    "token_length": 5,
                    "is_humanized": true,
                    "humanizer_score": 0.91
                },
                {
                    "text": "Human passage.",
                    "label": "Human Written",
                    "ai_assistance_score": 0.02,
                    "confidence": "Medium",
                    "start_index": 21,
                    "end_index": 35,
                    "word_count": 2,
                    "token_length": 4,
                    "is_humanized": false,
                    "humanizer_score": 0.0
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let c = client(&server).await;
    let request = TaskRequest {
        text: "AI-assisted passage. Human passage.".into(),
        model: "pangram-4".into(),
        public_dashboard_link: false,
    };
    let status = c.detect(&request, &fast()).await.unwrap();
    assert!(status.is_success());
    assert_eq!(status.prediction_short.as_deref(), Some("Mixed"));
    assert_eq!(status.windows.len(), 2);
    assert_eq!(status.windows[0].label, "AI-Assisted");
    assert_eq!(status.windows[1].end_index, 35);
    assert_eq!(status.windows[0].humanizer_score, Some(0.91));

    let history = c.history();
    let verbs: Vec<_> = history.entries.iter().map(|e| e.verb.as_str()).collect();
    assert_eq!(verbs, ["POST", "GET", "GET"]);
    assert_eq!(
        history.entries[0].request_body.as_ref().unwrap()["model"],
        "pangram-4"
    );
}

#[tokio::test]
async fn failed_stage_is_returned_not_errored() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/task/t1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "stage": "STAGE_FAILED",
            "text": "",
            "version": "",
            "headline": "preprocessing: Input text contains no valid text after preprocessing",
            "prediction": "",
            "prediction_short": "",
            "fraction_ai": 0.0,
            "fraction_ai_assisted": 0.0,
            "fraction_human": 0.0,
            "num_ai_segments": 0,
            "num_ai_assisted_segments": 0,
            "num_human_segments": 0,
            "windows": []
        })))
        .mount(&server)
        .await;

    let c = client(&server).await;
    let status = c.wait_for_task("t1", &fast()).await.unwrap();
    assert!(status.is_failed());
    assert!(status.headline.unwrap().starts_with("preprocessing"));
}

#[tokio::test]
async fn api_errors_are_mapped_and_recorded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/task"))
        .respond_with(
            ResponseTemplate::new(402).set_body_json(json!({ "detail": "Insufficient credits" })),
        )
        .mount(&server)
        .await;

    let c = client(&server).await;
    let request = TaskRequest {
        text: "x".into(),
        model: "default".into(),
        public_dashboard_link: false,
    };
    let err = c.create_task(&request).await.unwrap_err();
    match err {
        Error::Api {
            status, body, verb, ..
        } => {
            assert_eq!(status, 402);
            assert_eq!(verb, "POST");
            assert_eq!(body["detail"], "Insufficient credits");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let history = c.history();
    assert_eq!(history.entries[0].status, Some(402));
    assert_eq!(
        history.entries[0].response_body["detail"],
        "Insufficient credits"
    );
}

#[tokio::test]
async fn non_json_error_body_is_preserved() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(429).set_body_string("Too Many Requests"))
        .mount(&server)
        .await;

    let c = client(&server).await;
    let err = c.list_models().await.unwrap_err();
    match err {
        Error::Api { status, body, .. } => {
            assert_eq!(status, 429);
            assert_eq!(body, json!("Too Many Requests"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn poll_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/task/slow"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "task_id": "slow",
            "stage": "STAGE_INFERENCE"
        })))
        .mount(&server)
        .await;

    let c = client(&server).await;
    let poll = PollOptions {
        interval: Duration::from_millis(20),
        timeout: Some(Duration::from_millis(50)),
    };
    let err = c.wait_for_task("slow", &poll).await.unwrap_err();
    assert!(matches!(err, Error::Timeout(..)), "{err:?}");
}

#[tokio::test]
async fn bulk_submit_wait_and_page_results() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/bulk"))
        .and(body_json(json!({
            "items": [{"id": "row-001", "text": "First text to analyze"}],
            "model": "pangram-4"
        })))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({
            "bulk_id": "blk_123",
            "status": "queued",
            "total_items": 2,
            "accepted_items": [
                {"index": 0, "id": "row-001", "task_id": "123e4567-e89b-12d3-a456-426614174000"}
            ],
            "failed_items": [
                {"index": 1, "id": null, "task_id": null, "stage": "STAGE_FAILED", "error": "empty"}
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/bulk/blk_123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bulk_id": "blk_123",
            "status": "partial",
            "total_items": 2,
            "accepted": 1,
            "succeeded": 1,
            "failed": 1,
            "created_at": "1760000000.0",
            "completed_at": "1760000030.0"
        })))
        .mount(&server)
        .await;

    // Two pages of results: the server honours limit=1000 but we fake
    // pagination by answering offset=0 with one item and offset=1 with the
    // failed item.
    Mock::given(method("GET"))
        .and(path("/bulk/blk_123/results"))
        .and(query_param("offset", "0"))
        .and(query_param("limit", "1000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bulk_id": "blk_123",
            "offset": 0,
            "limit": 1000,
            "total_items": 2,
            "items": [{
                "index": 0,
                "id": "row-001",
                "task_id": "123e4567-e89b-12d3-a456-426614174000",
                "stage": "STAGE_SUCCESS",
                "error": null,
                "result": {
                    "stage": "STAGE_SUCCESS",
                    "text": "First text to analyze",
                    "version": "4.0",
                    "prediction": "We believe that this entire text is human-written.",
                    "prediction_short": "Human",
                    "fraction_ai": 0.0,
                    "fraction_ai_assisted": 0.0,
                    "fraction_human": 1.0,
                    "headline": "Human Written",
                    "num_ai_segments": 0,
                    "num_ai_assisted_segments": 0,
                    "num_human_segments": 1,
                    "windows": [{
                        "text": "First text to analyze",
                        "label": "Human Written",
                        "ai_assistance_score": 0.02,
                        "confidence": "High",
                        "start_index": 0,
                        "end_index": 21,
                        "word_count": 4,
                        "token_length": 5,
                        "is_humanized": false,
                        "humanizer_score": 0.0
                    }]
                }
            }],
            "failed_items": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/bulk/blk_123/results"))
        .and(query_param("offset", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "bulk_id": "blk_123",
            "offset": 1,
            "limit": 1000,
            "total_items": 2,
            "items": [],
            "failed_items": [
                {"index": 1, "id": null, "task_id": null, "stage": "STAGE_FAILED", "error": "empty"}
            ]
        })))
        .mount(&server)
        .await;

    let c = client(&server).await;
    let created = c
        .create_bulk(&BulkRequest {
            items: vec![BulkRequestItem {
                id: Some("row-001".into()),
                text: "First text to analyze".into(),
            }],
            text: vec![],
            model: "pangram-4".into(),
        })
        .await
        .unwrap();
    assert_eq!(created.bulk_id, "blk_123");
    assert_eq!(created.failed_items[0].error.as_deref(), Some("empty"));

    let status = c.wait_for_bulk("blk_123", &fast()).await.unwrap();
    assert_eq!(status.status, "partial");
    assert!(status.is_terminal());

    let results = c.all_bulk_results("blk_123").await.unwrap();
    assert_eq!(results.items.len(), 1);
    assert_eq!(results.failed_items.len(), 1);
    let result = results.items[0].result.as_ref().unwrap();
    assert_eq!(result.windows[0].word_count, 4);
}

#[tokio::test]
async fn plagiarism_and_upload() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("content-type", "application/json"))
        .and(body_json(
            json!({ "text": "The text to check for plagiarism" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "text": "The text to check for plagiarism",
            "plagiarism_detected": true,
            "plagiarized_content": [{
                "source_url": "https://example.com/source",
                "matched_text": "The text to check for plagiarism",
                "similarity_score": 0.95
            }],
            "total_sentences": 1,
            "plagiarized_sentences": 1,
            "percent_plagiarized": 100.0
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("x-api-key", KEY))
        .and(wiremock::matchers::header_regex("content-type", "^multipart/form-data; boundary="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "public_dashboard_link": "https://www.pangram.com/history/123e4567-e89b-12d3-a456-426614174000" }
        ])))
        .mount(&server)
        .await;

    let c = client(&server).await;
    let result = c
        .check_plagiarism("The text to check for plagiarism")
        .await
        .unwrap();
    assert!(result.plagiarism_detected);
    assert_eq!(result.plagiarized_content[0].similarity_score, 0.95);

    let uploaded = c
        .upload_files(
            vec![pangram_rs::UploadFile {
                file_name: "a.txt".into(),
                bytes: b"hello".to_vec(),
            }],
            true,
        )
        .await
        .unwrap();
    assert_eq!(uploaded.len(), 1);
    assert!(uploaded[0]
        .public_dashboard_link
        .as_deref()
        .unwrap()
        .contains("/history/"));

    let history = c.history();
    assert_eq!(history.len(), 2);
    assert_eq!(
        history.entries[1].request_body.as_ref().unwrap()["files"][0],
        "a.txt"
    );
}

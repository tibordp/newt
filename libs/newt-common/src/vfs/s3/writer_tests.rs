//! Uploads and error kinds against a fake S3: each request is recorded and
//! answered by the test.

use std::sync::Arc;

use aws_sdk_s3::config::http::{HttpRequest, HttpResponse};
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region, SharedCredentialsProvider};
use aws_sdk_s3::primitives::SdkBody;
use aws_smithy_runtime_api::client::http::{
    HttpConnector, HttpConnectorFuture, SharedHttpConnector, http_client_fn,
};
use aws_smithy_runtime_api::http::StatusCode;
use parking_lot::Mutex;

use super::S3Vfs;
use super::writer::{PART_SIZE, part_size};
use crate::ErrorKind;
use crate::vfs::Vfs;
use crate::vfs::attributes::WriteOptions;
use crate::vfs::path::PathBuf;

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    uri: String,
    if_none_match: Option<String>,
    copy_source: bool,
    /// The payload's own length, under aws-chunked framing too.
    length: Option<u64>,
}

impl Seen {
    /// The S3 action, told apart by method and query.
    fn action(&self) -> &'static str {
        match (self.method.as_str(), self.uri.as_str()) {
            ("POST", uri) if uri.contains("?uploads") => "CreateMultipartUpload",
            ("POST", uri) if uri.contains("uploadId=") => "CompleteMultipartUpload",
            ("PUT", uri) if uri.contains("partNumber=") => "UploadPart",
            ("PUT", _) if self.copy_source => "CopyObject",
            ("PUT", _) => "PutObject",
            ("DELETE", uri) if uri.contains("uploadId=") => "AbortMultipartUpload",
            ("HEAD", _) => "HeadObject",
            _ => "other",
        }
    }
}

type Respond = dyn Fn(&Seen) -> (u16, &'static str) + Send + Sync;

struct FakeS3 {
    seen: Arc<Mutex<Vec<Seen>>>,
    respond: Arc<Respond>,
}

impl std::fmt::Debug for FakeS3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeS3")
    }
}

impl HttpConnector for FakeS3 {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        let header = |name: &str| request.headers().get(name).map(str::to_string);
        let seen = Seen {
            method: request.method().to_string(),
            uri: request.uri().to_string(),
            if_none_match: header("if-none-match"),
            copy_source: header("x-amz-copy-source").is_some(),
            length: header("x-amz-decoded-content-length")
                .or_else(|| header("content-length"))
                .and_then(|v| v.parse().ok()),
        };
        self.seen.lock().push(seen.clone());
        let (status, body) = (self.respond)(&seen);
        let mut response =
            HttpResponse::new(StatusCode::try_from(status).unwrap(), SdkBody::from(body));
        response.headers_mut().insert("etag", "\"etag\"");
        response
            .headers_mut()
            .insert("content-length", body.len().to_string());
        HttpConnectorFuture::ready(Ok(response))
    }
}

const INITIATED: &str = "<InitiateMultipartUploadResult><Bucket>bucket</Bucket>\
    <Key>key</Key><UploadId>upload</UploadId></InitiateMultipartUploadResult>";
const COMPLETED: &str = "<CompleteMultipartUploadResult><Bucket>bucket</Bucket>\
    <Key>key</Key><ETag>\"etag\"</ETag></CompleteMultipartUploadResult>";
const COPIED: &str = "<CopyObjectResult><ETag>\"etag\"</ETag></CopyObjectResult>";
const PRECONDITION_FAILED: &str =
    "<Error><Code>PreconditionFailed</Code><Message>no</Message></Error>";

/// What S3 answers to a request that succeeds.
fn success(seen: &Seen) -> (u16, &'static str) {
    match seen.action() {
        "CreateMultipartUpload" => (200, INITIATED),
        "CompleteMultipartUpload" => (200, COMPLETED),
        "CopyObject" => (200, COPIED),
        _ => (200, ""),
    }
}

struct Fake {
    vfs: S3Vfs,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Fake {
    fn new(respond: impl Fn(&Seen) -> (u16, &'static str) + Send + Sync + 'static) -> Self {
        Self::with_endpoint(None, respond)
    }

    fn with_endpoint(
        endpoint: Option<&str>,
        respond: impl Fn(&Seen) -> (u16, &'static str) + Send + Sync + 'static,
    ) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connector = SharedHttpConnector::new(FakeS3 {
            seen: seen.clone(),
            respond: Arc::new(respond),
        });
        let mut config = aws_config::SdkConfig::builder()
            .http_client(http_client_fn(move |_, _| connector.clone()))
            .region(Region::new("us-east-1"))
            .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
                "id", "secret", None, None, "test",
            )))
            .retry_config(RetryConfig::disabled())
            .behavior_version(BehaviorVersion::latest());
        if let Some(endpoint) = endpoint {
            config = config.endpoint_url(endpoint);
        }
        let config = config.build();
        let client = aws_sdk_s3::Client::new(&config);
        Self {
            vfs: S3Vfs::new(client, config, Some("bucket".into()), true),
            seen,
        }
    }

    fn actions(&self) -> Vec<&'static str> {
        self.seen.lock().iter().map(Seen::action).collect()
    }

    fn seen(&self, action: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .iter()
            .filter(|s| s.action() == action)
            .cloned()
            .collect()
    }

    async fn write(
        &self,
        key: &str,
        options: &WriteOptions,
        len: usize,
    ) -> Result<(), crate::Error> {
        let mut writer = self.vfs.overwrite_async(&path(key), options).await?;
        let chunk = vec![7u8; 1 << 20];
        let mut left = len;
        while left > 0 {
            let n = left.min(chunk.len());
            writer.write(&chunk[..n]).await?;
            left -= n;
        }
        writer.finish().await
    }
}

fn path(key: &str) -> PathBuf {
    PathBuf::from_wire_str(&format!("/{key}"))
}

fn create_new(size: u64) -> WriteOptions {
    WriteOptions {
        create_new: true,
        size_hint: Some(size),
        ..Default::default()
    }
}

#[test]
fn parts_fit_the_hint_within_the_part_cap() {
    assert_eq!(part_size(1, None), PART_SIZE);
    assert_eq!(part_size(1, Some(3)), PART_SIZE);
    // Unhinted uploads double every thousand parts, up to S3's largest.
    assert_eq!(part_size(1_001, None), 2 * PART_SIZE);
    assert_eq!(part_size(10_000, None), 512 * PART_SIZE);
    assert_eq!(part_size(20_000, None), 5 << 30);
    // 50 TB, S3's largest object, fits 10,000 parts, each whole MiBs.
    let size = 50_000_000_000_000u64;
    let part = part_size(1, Some(size));
    assert!(part * 10_000 >= size && part <= 5 << 30);
    assert_eq!(part % (1 << 20), 0);
}

#[tokio::test]
async fn a_small_create_new_is_one_conditional_put() {
    let fake = Fake::new(success);
    fake.write("key", &create_new(3), 3).await.unwrap();

    let puts = fake.seen("PutObject");
    assert_eq!(fake.actions(), vec!["PutObject"]);
    assert_eq!(puts[0].if_none_match.as_deref(), Some("*"));
    assert_eq!(puts[0].length, Some(3));
}

#[tokio::test]
async fn a_refused_put_is_already_exists() {
    let fake = Fake::new(|seen| match seen.action() {
        "PutObject" => (412, PRECONDITION_FAILED),
        _ => success(seen),
    });
    let error = fake.write("key", &create_new(3), 3).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::AlreadyExists);
}

#[tokio::test]
async fn a_small_plain_write_is_one_unconditional_put() {
    let fake = Fake::new(success);
    fake.write("key", &WriteOptions::default(), 3)
        .await
        .unwrap();

    assert_eq!(fake.actions(), vec!["PutObject"]);
    assert_eq!(fake.seen("PutObject")[0].if_none_match, None);
}

#[tokio::test]
async fn create_new_is_not_offered_for_what_one_put_cannot_carry() {
    let fake = Fake::new(success);
    for options in [
        create_new(PART_SIZE + 1),
        WriteOptions {
            create_new: true,
            ..Default::default()
        },
    ] {
        let error = fake
            .vfs
            .overwrite_async(&path("key"), &options)
            .await
            .err()
            .unwrap();
        assert_eq!(error.kind, ErrorKind::NotSupported);
    }
    assert!(fake.actions().is_empty());
}

#[tokio::test]
async fn a_custom_endpoint_offers_no_create_new() {
    let fake = Fake::with_endpoint(Some("http://localhost:9000"), success);
    let error = fake
        .vfs
        .overwrite_async(&path("key"), &create_new(3))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::NotSupported);
    let error = fake
        .vfs
        .copy_within(&path("a"), &path("b"), &create_new(3))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotSupported);
    assert!(fake.actions().is_empty());
}

#[tokio::test]
async fn touch_on_a_custom_endpoint_leaves_an_existing_object_alone() {
    let fake = Fake::with_endpoint(Some("http://localhost:9000"), success);
    fake.vfs.touch(&path("key")).await.unwrap();
    assert_eq!(fake.actions(), vec!["HeadObject"]);
}

#[tokio::test]
async fn a_larger_object_is_a_multipart_upload_created_when_its_first_part_fills() {
    let fake = Fake::new(success);
    let len = 2 * PART_SIZE as usize + (5 << 20);
    fake.write("key", &WriteOptions::default(), len)
        .await
        .unwrap();

    assert_eq!(
        fake.actions(),
        vec![
            "CreateMultipartUpload",
            "UploadPart",
            "UploadPart",
            "UploadPart",
            "CompleteMultipartUpload",
        ]
    );
    let lengths = fake
        .seen("UploadPart")
        .iter()
        .map(|s| s.length.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lengths, vec![PART_SIZE, PART_SIZE, 5 << 20]);
    assert_eq!(fake.seen("CompleteMultipartUpload")[0].if_none_match, None);
}

#[tokio::test]
async fn an_object_of_exactly_one_part_is_one_put() {
    let fake = Fake::new(success);
    fake.write("key", &WriteOptions::default(), PART_SIZE as usize)
        .await
        .unwrap();
    assert_eq!(fake.actions(), vec!["PutObject"]);
}

#[tokio::test]
async fn a_create_new_that_outgrows_its_hint_completes_conditionally() {
    let fake = Fake::new(success);
    fake.write("key", &create_new(3), PART_SIZE as usize + 1)
        .await
        .unwrap();

    let completes = fake.seen("CompleteMultipartUpload");
    assert_eq!(completes.len(), 1);
    assert_eq!(completes[0].if_none_match.as_deref(), Some("*"));
}

#[tokio::test]
async fn a_refused_complete_aborts_the_upload() {
    let fake = Fake::new(|seen| match seen.action() {
        "CompleteMultipartUpload" => (412, PRECONDITION_FAILED),
        _ => success(seen),
    });
    let error = fake
        .write("key", &create_new(3), PART_SIZE as usize + 1)
        .await
        .unwrap_err();

    assert_eq!(error.kind, ErrorKind::AlreadyExists);
    assert_eq!(fake.actions().last(), Some(&"AbortMultipartUpload"));
}

#[tokio::test]
async fn parts_past_the_memory_bound_upload_from_a_spill_file() {
    let fake = Fake::new(success);
    let hint = 400u64 << 30;
    let part = part_size(1, Some(hint));
    assert!(part > 32 << 20, "the part must not fit in memory");
    fake.write(
        "key",
        &WriteOptions {
            size_hint: Some(hint),
            ..Default::default()
        },
        part as usize + 1,
    )
    .await
    .unwrap();

    let lengths = fake
        .seen("UploadPart")
        .iter()
        .map(|s| s.length.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lengths, vec![part, 1]);
}

#[tokio::test]
async fn copy_within_under_create_new_is_a_conditional_copy() {
    let fake = Fake::new(success);
    fake.vfs
        .copy_within(&path("a"), &path("b"), &create_new(3))
        .await
        .unwrap();

    assert_eq!(fake.actions(), vec!["HeadObject", "CopyObject"]);
    assert_eq!(
        fake.seen("CopyObject")[0].if_none_match.as_deref(),
        Some("*")
    );
}

#[tokio::test]
async fn status_codes_become_error_kinds() {
    for (status, kind) in [
        (404, ErrorKind::NotFound),
        (403, ErrorKind::PermissionDenied),
        (500, ErrorKind::Other),
    ] {
        let fake = Fake::new(move |_| (status, ""));
        let error = fake.vfs.file_info(&path("key")).await.unwrap_err();
        assert_eq!(error.kind, kind, "{status}");
    }
}

#[tokio::test]
async fn only_a_conditional_conflict_is_already_exists_among_409s() {
    for (body, kind) in [
        (
            "<Error><Code>ConditionalRequestConflict</Code><Message>no</Message></Error>",
            ErrorKind::AlreadyExists,
        ),
        (
            "<Error><Code>OperationAborted</Code><Message>no</Message></Error>",
            ErrorKind::Other,
        ),
    ] {
        let fake = Fake::new(move |seen| match seen.action() {
            "PutObject" => (409, body),
            _ => success(seen),
        });
        let error = fake.write("key", &create_new(3), 3).await.unwrap_err();
        assert_eq!(error.kind, kind, "{body}");
    }
}

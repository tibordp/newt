use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use log::info;
use newt_common::filesystem::Filesystem;
use newt_common::vfs::{VfsId, VfsPath};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;

struct FileServerState {
    token: String,
    fs: Arc<dyn Filesystem>,
}

#[derive(serde::Deserialize)]
struct FileQuery {
    path: String,
}

fn parse_range_header(header: &str, file_size: u64) -> Option<(u64, u64)> {
    let range = header.strip_prefix("bytes=")?;
    let (start_str, end_str) = range.split_once('-')?;
    let start: u64 = start_str.parse().ok()?;
    let end: u64 = if end_str.is_empty() {
        file_size.checked_sub(1)?
    } else {
        end_str.parse().ok()?
    };
    if start > end || start >= file_size {
        return None;
    }

    Some((start, end.min(file_size - 1)))
}

/// Stream file bytes from `start` to `end` (inclusive) in 1 MB chunks,
/// without buffering the entire range in memory. A range that fits one
/// chunk goes through a single stateless `read_range`; anything longer
/// holds one positioned-read handle for the whole response, so media
/// playback doesn't pay a file open (or S3 request setup) per chunk.
fn chunk_stream(fs: Arc<dyn Filesystem>, vfs_path: VfsPath, start: u64, end: u64) -> Body {
    const CHUNK_SIZE: u64 = 1024 * 1024;
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(2);

    let _task = tokio::spawn(async move {
        if end - start < CHUNK_SIZE {
            let result = tokio::select! {
                biased;
                _ = tx.closed() => return,
                result = fs.read_range(vfs_path, start, end - start + 1) => result,
            };
            let _ = match result {
                Ok(chunk) => tx.send(Ok(bytes::Bytes::from(chunk.data))).await,
                Err(e) => tx.send(Err(std::io::Error::other(e.to_string()))).await,
            };
            return;
        }

        let mut handle = match fs.open_read_at(vfs_path).await {
            Ok(handle) => handle,
            Err(e) => {
                let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                return;
            }
        };
        let mut offset = start;
        while offset <= end {
            let len = std::cmp::min(CHUNK_SIZE, end - offset + 1);
            let result = tokio::select! {
                biased;
                _ = tx.closed() => break,
                result = handle.read_at(offset, len) => result,
            };
            match result {
                Ok(data) => {
                    if data.is_empty() {
                        break;
                    }
                    offset += data.len() as u64;
                    if tx.send(Ok(bytes::Bytes::from(data))).await.is_err() {
                        break; // receiver dropped — client disconnected
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                    break;
                }
            }
        }
    });

    Body::from_stream(ReceiverStream::new(rx))
}

pub fn start(fs: Arc<dyn Filesystem>, token: String) -> (u16, JoinHandle<()>) {
    let state = Arc::new(FileServerState { token, fs });
    let app = Router::new()
        .route("/{token}/{vfs_id}", get(serve_file))
        .with_state(state);

    let listener = std::net::TcpListener::bind("localhost:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    (port, handle)
}

async fn serve_file(
    State(state): State<Arc<FileServerState>>,
    Path((token, vfs_id_str)): Path<(String, String)>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> Response {
    if token != state.token {
        return StatusCode::FORBIDDEN.into_response();
    }

    let vfs_id = match vfs_id_str.parse::<u32>() {
        Ok(id) => VfsId(id),
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let vfs_path = VfsPath::from_wire_str(vfs_id, &query.path);

    let details = match state.fs.file_details(vfs_path.clone()).await {
        Ok(d) => d,
        Err(e) => {
            log::error!("file_server: file_details error: {}", e);
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    // The type may come verbatim from a remote object's Content-Type.
    let mime = details
        .mime_type
        .and_then(|m| header::HeaderValue::from_str(&m).ok())
        .unwrap_or(header::HeaderValue::from_static("application/octet-stream"));
    let file_size = details.size;

    let range_header = headers
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if let Some((range_start, range_end)) = range_header
        .as_deref()
        .and_then(|h| parse_range_header(h, file_size))
    {
        let length = range_end - range_start + 1;
        info!(
            "file_server: 206 bytes={}-{}/{} ({})",
            range_start, range_end, file_size, length
        );

        let body = chunk_stream(state.fs.clone(), vfs_path, range_start, range_end);

        Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, &mime)
            .header(header::CONTENT_LENGTH, length.to_string())
            .header(
                header::CONTENT_RANGE,
                format!("bytes {}-{}/{}", range_start, range_end, file_size),
            )
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(body)
            .unwrap()
    } else {
        info!("file_server: 200 size={}", file_size);

        let end = file_size.saturating_sub(1);
        let body = if file_size == 0 {
            Body::empty()
        } else {
            chunk_stream(state.fs.clone(), vfs_path, 0, end)
        };

        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, &mime)
            .header(header::CONTENT_LENGTH, file_size.to_string())
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(body)
            .unwrap()
    }
}

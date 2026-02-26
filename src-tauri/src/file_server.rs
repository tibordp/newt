use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use log::info;
use newt_common::file_reader::FileReader;
use newt_common::vfs::{VfsId, VfsPath};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;

struct FileServerState {
    token: String,
    file_reader: Arc<dyn FileReader>,
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
/// without buffering the entire range in memory.
fn chunk_stream(
    file_reader: Arc<dyn FileReader>,
    vfs_path: VfsPath,
    start: u64,
    end: u64,
) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(2);

    tokio::spawn(async move {
        let chunk_size: u64 = 1024 * 1024;
        let mut offset = start;
        while offset <= end {
            let len = std::cmp::min(chunk_size, end - offset + 1);
            match file_reader.read_range(vfs_path.clone(), offset, len).await {
                Ok(chunk) => {
                    if chunk.data.is_empty() {
                        break;
                    }
                    offset += chunk.data.len() as u64;
                    if tx.send(Ok(bytes::Bytes::from(chunk.data))).await.is_err() {
                        break; // receiver dropped — client disconnected
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(std::io::Error::other(e.to_string())));
                    break;
                }
            }
        }
    });

    Body::from_stream(ReceiverStream::new(rx))
}

pub fn start(file_reader: Arc<dyn FileReader>, token: String) -> (u16, JoinHandle<()>) {
    let state = Arc::new(FileServerState { token, file_reader });
    let app = Router::new()
        .route("/{token}/{vfs_id}/{*path}", get(serve_file))
        .with_state(state);

    let listener = std::net::TcpListener::bind("[::1]:0").unwrap();
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
    Path((token, vfs_id_str, path)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    if token != state.token {
        return StatusCode::FORBIDDEN.into_response();
    }

    let vfs_id = match vfs_id_str.parse::<u32>() {
        Ok(id) => VfsId(id),
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let vfs_path = VfsPath::new(vfs_id, format!("/{}", path));

    let details = match state.file_reader.file_details(vfs_path.clone()).await {
        Ok(d) => d,
        Err(e) => {
            log::error!("file_server: file_details error: {}", e);
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    let mime = details
        .mime_type
        .unwrap_or_else(|| "application/octet-stream".to_string());
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

        let body = chunk_stream(state.file_reader.clone(), vfs_path, range_start, range_end);

        Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header("content-type", &mime)
            .header("content-length", length.to_string())
            .header(
                "content-range",
                format!("bytes {}-{}/{}", range_start, range_end, file_size),
            )
            .header("accept-ranges", "bytes")
            .body(body)
            .unwrap()
    } else {
        info!("file_server: 200 size={}", file_size);

        let end = file_size.saturating_sub(1);
        let body = if file_size == 0 {
            Body::empty()
        } else {
            chunk_stream(state.file_reader.clone(), vfs_path, 0, end)
        };

        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", &mime)
            .header("content-length", file_size.to_string())
            .header("accept-ranges", "bytes")
            .body(body)
            .unwrap()
    }
}

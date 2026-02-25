use std::collections::HashMap;
use std::path::{Path, PathBuf};

use log::{debug, info, warn};
use parking_lot::Mutex;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::file_reader::{FileChunk, FileInfo};
use crate::filesystem::{File, ListFilesOptions, Mode, VfsFileList};
use crate::vfs::{RegisteredDescriptor, Vfs, VfsAsyncWriter, VfsChangeNotifier, VfsDescriptor};
use crate::{Error, ToUnix};

const MULTIPART_CHUNK_SIZE: usize = 10 * 1024 * 1024; // 10 MB

// ---------------------------------------------------------------------------
// S3VfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct S3VfsDescriptor;

impl VfsDescriptor for S3VfsDescriptor {
    fn type_name(&self) -> &'static str {
        "s3"
    }
    fn can_watch(&self) -> bool {
        true
    }
    fn can_read_sync(&self) -> bool {
        false
    }
    fn can_read_async(&self) -> bool {
        true
    }
    fn can_overwrite_sync(&self) -> bool {
        false
    }
    fn can_overwrite_async(&self) -> bool {
        true
    }
    fn can_create_directory(&self) -> bool {
        true
    }
    fn can_create_symlink(&self) -> bool {
        false
    }
    fn can_touch(&self) -> bool {
        true
    }
    fn can_truncate(&self) -> bool {
        false
    }
    fn can_set_metadata(&self) -> bool {
        false
    }
    fn can_remove(&self) -> bool {
        true
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn has_symlinks(&self) -> bool {
        false
    }
    fn can_rename(&self) -> bool {
        false
    }
    fn can_copy_within(&self) -> bool {
        true
    }
    fn can_hard_link(&self) -> bool {
        false
    }
}

pub static S3_VFS_DESCRIPTOR: S3VfsDescriptor = S3VfsDescriptor;
inventory::submit!(RegisteredDescriptor(&S3_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// S3Vfs
// ---------------------------------------------------------------------------

pub struct S3Vfs {
    /// Default client (us-east-1) — used for ListBuckets and GetBucketLocation.
    default_client: aws_sdk_s3::Client,
    /// Shared AWS config (credentials, etc.) — used to build per-region clients.
    sdk_config: aws_sdk_s3::config::Builder,
    /// Cached per-region clients.
    region_clients: Mutex<HashMap<String, aws_sdk_s3::Client>>,
    /// Cached bucket → region mapping.
    bucket_regions: Mutex<HashMap<String, String>>,
    /// Change notifier for self-notification on mutations.
    notifier: VfsChangeNotifier,
}

impl S3Vfs {
    pub fn new(default_client: aws_sdk_s3::Client, sdk_config: aws_config::SdkConfig) -> Self {
        Self {
            default_client,
            sdk_config: aws_sdk_s3::config::Builder::from(&sdk_config),
            region_clients: Mutex::new(HashMap::new()),
            bucket_regions: Mutex::new(HashMap::new()),
            notifier: VfsChangeNotifier::new(),
        }
    }

    /// Get or create an S3 client for the given bucket's region.
    async fn client_for_bucket(&self, bucket: &str) -> Result<aws_sdk_s3::Client, Error> {
        // Check cache first
        if let Some(region) = self.bucket_regions.lock().get(bucket).cloned() {
            if let Some(client) = self.region_clients.lock().get(&region).cloned() {
                debug!("s3: client cache hit for bucket={} region={}", bucket, region);
                return Ok(client);
            }
        }

        // Discover region via GetBucketLocation
        let resp = self
            .default_client
            .get_bucket_location()
            .bucket(bucket)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        // GetBucketLocation returns None/empty for us-east-1
        let region = resp
            .location_constraint()
            .map(|lc| lc.as_str().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "us-east-1".to_string());

        info!("s3: discovered region={} for bucket={}", region, bucket);

        // Get or create client for this region
        let client = {
            let mut clients = self.region_clients.lock();
            if let Some(c) = clients.get(&region) {
                c.clone()
            } else {
                info!("s3: creating new client for region={}", region);
                let config = self
                    .sdk_config
                    .clone()
                    .region(aws_sdk_s3::config::Region::new(region.clone()))
                    .build();
                let c = aws_sdk_s3::Client::from_conf(config);
                clients.insert(region.clone(), c.clone());
                c
            }
        };

        self.bucket_regions
            .lock()
            .insert(bucket.to_string(), region);
        Ok(client)
    }

    /// Parse an absolute path into (bucket, prefix).
    /// `/` → (None, None) — list buckets
    /// `/my-bucket` → (Some("my-bucket"), None) — list bucket root
    /// `/my-bucket/some/prefix/` → (Some("my-bucket"), Some("some/prefix/"))
    fn parse_path(path: &Path) -> (Option<String>, Option<String>) {
        let s = path.to_string_lossy();
        let s = s.trim_start_matches('/');
        if s.is_empty() {
            return (None, None);
        }
        match s.find('/') {
            None => (Some(s.to_string()), None),
            Some(idx) => {
                let bucket = &s[..idx];
                let prefix = &s[idx + 1..];
                if prefix.is_empty() {
                    (Some(bucket.to_string()), None)
                } else {
                    (Some(bucket.to_string()), Some(prefix.to_string()))
                }
            }
        }
    }

    async fn list_buckets(
        &self,
        batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<VfsFileList, Error> {
        let resp = self
            .default_client
            .list_buckets()
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        debug!("s3: list_buckets returned {} buckets", resp.buckets().len());

        let mut files = Vec::new();
        for bucket in resp.buckets() {
            let name = bucket.name().unwrap_or_default().to_string();
            let created = bucket.creation_date().and_then(|d| {
                let secs = d.secs();
                let nanos = d.subsec_nanos();
                std::time::SystemTime::UNIX_EPOCH
                    .checked_add(std::time::Duration::new(secs as u64, nanos))
                    .map(|t| t.to_unix())
            });

            files.push(File {
                name,
                size: None,
                is_dir: true,
                is_hidden: false,
                is_symlink: false,
                user: None,
                group: None,
                mode: Mode(0),
                modified: None,
                accessed: None,
                created,
            });
        }

        if let Some(tx) = batch_tx {
            if !files.is_empty() {
                let _ = tx.send(files.clone());
            }
        }

        Ok(VfsFileList {
            path: PathBuf::from("/"),
            files,
            fs_stats: None,
        })
    }

    async fn list_objects(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<VfsFileList, Error> {
        // S3 requires prefixes to end with '/' to list directory contents
        let prefix = prefix.map(|p| {
            if p.ends_with('/') {
                p.to_string()
            } else {
                format!("{}/", p)
            }
        });
        let prefix = prefix.as_deref();

        let mut files = Vec::new();

        // ".." entry
        let dotdot = File {
            name: "..".to_string(),
            size: None,
            is_dir: true,
            is_hidden: false,
            is_symlink: false,
            user: None,
            group: None,
            mode: Mode(0),
            modified: None,
            accessed: None,
            created: None,
        };
        files.push(dotdot);

        debug!("s3: list_objects bucket={} prefix={:?}", bucket, prefix);

        let client = self.client_for_bucket(bucket).await?;

        let mut request = client
            .list_objects_v2()
            .bucket(bucket)
            .delimiter("/");

        if let Some(p) = prefix {
            request = request.prefix(p);
        }

        let mut continuation_token: Option<String> = None;
        loop {
            let mut req = request.clone();
            if let Some(ref token) = continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| Error::Custom(e.to_string()))?;

            debug!(
                "s3: list_objects page: {} prefixes, {} objects",
                resp.common_prefixes().len(),
                resp.contents().len()
            );

            let prefix_len = prefix.map_or(0, |p| p.len());
            let mut batch = Vec::new();

            // Common prefixes (directories)
            for cp in resp.common_prefixes() {
                if let Some(p) = cp.prefix() {
                    let name = &p[prefix_len..];
                    let name = name.trim_end_matches('/');
                    if !name.is_empty() {
                        batch.push(File {
                            name: name.to_string(),
                            size: None,
                            is_dir: true,
                            is_hidden: false,
                            is_symlink: false,
                            user: None,
                            group: None,
                            mode: Mode(0),
                            modified: None,
                            accessed: None,
                            created: None,
                        });
                    }
                }
            }

            // Objects (files)
            for obj in resp.contents() {
                if let Some(key) = obj.key() {
                    let name = &key[prefix_len..];
                    // Skip the prefix itself if it appears as a key (e.g. "dir/" marker objects)
                    if name.is_empty() || name == "/" {
                        continue;
                    }
                    let modified = obj.last_modified().and_then(|d| {
                        let secs = d.secs();
                        let nanos = d.subsec_nanos();
                        std::time::SystemTime::UNIX_EPOCH
                            .checked_add(std::time::Duration::new(secs as u64, nanos))
                            .map(|t| t.to_unix())
                    });

                    batch.push(File {
                        name: name.to_string(),
                        size: Some(obj.size().unwrap_or(0) as u64),
                        is_dir: false,
                        is_hidden: false,
                        is_symlink: false,
                        user: obj
                            .owner()
                            .and_then(|o| o.display_name())
                            .map(|n| crate::filesystem::UserGroup::Name(n.to_string())),
                        group: None,
                        mode: Mode(0),
                        modified,
                        accessed: None,
                        created: None,
                    });
                }
            }

            if let Some(tx) = &batch_tx {
                if !batch.is_empty() {
                    let _ = tx.send(batch.clone());
                }
            }
            files.extend(batch);

            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        let full_path = match prefix {
            Some(p) => format!("/{}/{}", bucket, p),
            None => format!("/{}", bucket),
        };

        Ok(VfsFileList {
            path: PathBuf::from(&full_path),
            files,
            fs_stats: None,
        })
    }
}

#[async_trait::async_trait]
impl Vfs for S3Vfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &S3_VFS_DESCRIPTOR
    }

    async fn list_files(
        &self,
        path: &Path,
        _opts: ListFilesOptions,
        batch_tx: Option<mpsc::UnboundedSender<Vec<File>>>,
    ) -> Result<VfsFileList, Error> {
        let (bucket, prefix) = Self::parse_path(path);
        match bucket {
            None => self.list_buckets(batch_tx).await,
            Some(bucket) => self.list_objects(&bucket, prefix.as_deref(), batch_tx).await,
        }
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        self.notifier.watch(path).await;
        Ok(())
    }

    async fn file_info(&self, path: &Path) -> Result<FileInfo, Error> {
        let (bucket, prefix) = Self::parse_path(path);
        let bucket = bucket.ok_or(Error::NotSupported)?;
        let key = prefix.ok_or_else(|| Error::Custom("no object key specified".into()))?;
        let client = self.client_for_bucket(&bucket).await?;

        let resp = client
            .head_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        let size = resp.content_length().unwrap_or(0) as u64;
        let content_type = resp.content_type().unwrap_or("");
        let is_binary = !content_type.starts_with("text/")
            && content_type != "application/json"
            && content_type != "application/xml";

        Ok(FileInfo { size, is_binary })
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let (bucket, prefix) = Self::parse_path(path);
        let bucket = bucket.ok_or(Error::NotSupported)?;
        let key = prefix.ok_or_else(|| Error::Custom("no object key specified".into()))?;
        let client = self.client_for_bucket(&bucket).await?;

        let end = offset + length - 1;
        let range = format!("bytes={}-{}", offset, end);

        let resp = client
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .range(range)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        // Parse total size from content_range header (e.g. "bytes 0-99/12345")
        let total_size = resp
            .content_range()
            .and_then(|r| r.rsplit('/').next())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(resp.content_length().unwrap_or(0) as u64);

        let data = resp
            .body
            .collect()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?
            .into_bytes()
            .to_vec();

        Ok(FileChunk {
            data,
            offset,
            total_size,
        })
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        let (bucket, prefix) = Self::parse_path(path);
        let bucket = bucket.ok_or(Error::NotSupported)?;
        let key = prefix.ok_or_else(|| Error::Custom("no object key specified".into()))?;
        let client = self.client_for_bucket(&bucket).await?;

        let resp = client
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        Ok(Box::new(resp.body.into_async_read()))
    }

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        let (bucket, prefix) = Self::parse_path(path);
        let bucket = bucket.ok_or(Error::NotSupported)?;
        let key = prefix.ok_or_else(|| Error::Custom("no object key specified".into()))?;
        let client = self.client_for_bucket(&bucket).await?;

        debug!("s3: initiating multipart upload bucket={} key={}", bucket, key);

        let resp = client
            .create_multipart_upload()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        let upload_id = resp
            .upload_id()
            .ok_or_else(|| Error::Custom("no upload_id returned".into()))?
            .to_string();

        debug!("s3: multipart upload_id={}", upload_id);

        Ok(Box::new(S3AsyncWriter {
            client,
            bucket,
            key,
            upload_id,
            buffer: Vec::new(),
            part_number: 1,
            completed_parts: Vec::new(),
            notifier: self.notifier.clone(),
            path: path.to_path_buf(),
        }))
    }

    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        let (bucket, prefix) = Self::parse_path(path);
        let bucket = bucket.ok_or(Error::NotSupported)?;
        let key = prefix.ok_or(Error::NotSupported)?;

        debug!("s3: remove_file bucket={} key={}", bucket, key);

        let client = self.client_for_bucket(&bucket).await?;
        client
            .delete_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        self.notifier.notify(path);
        Ok(())
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        let (bucket, prefix) = Self::parse_path(path);
        let bucket = bucket.ok_or(Error::NotSupported)?;
        let key = prefix.ok_or(Error::NotSupported)?;

        // S3 directory markers are stored with a trailing slash
        let dir_key = if key.ends_with('/') {
            key
        } else {
            format!("{}/", key)
        };

        debug!("s3: remove_dir bucket={} key={}", bucket, dir_key);

        let client = self.client_for_bucket(&bucket).await?;
        client
            .delete_object()
            .bucket(&bucket)
            .key(&dir_key)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        self.notifier.notify(path);
        Ok(())
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let (src_bucket, src_key) = Self::parse_path(from);
        let src_bucket = src_bucket.ok_or(Error::NotSupported)?;
        let src_key = src_key.ok_or_else(|| Error::Custom("no source key".into()))?;

        let (dst_bucket, dst_key) = Self::parse_path(to);
        let dst_bucket = dst_bucket.ok_or(Error::NotSupported)?;
        let dst_key = dst_key.ok_or_else(|| Error::Custom("no destination key".into()))?;

        debug!(
            "s3: copy_within {}/{} -> {}/{}",
            src_bucket, src_key, dst_bucket, dst_key
        );

        let client = self.client_for_bucket(&dst_bucket).await?;
        let copy_source = format!("{}/{}", src_bucket, src_key);

        client
            .copy_object()
            .bucket(&dst_bucket)
            .key(&dst_key)
            .copy_source(&copy_source)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;
        self.notifier.notify(to);
        Ok(())
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let (bucket, prefix) = Self::parse_path(path);
        let bucket = bucket.ok_or(Error::NotSupported)?;
        let key = prefix.ok_or_else(|| Error::Custom("no key specified".into()))?;
        let client = self.client_for_bucket(&bucket).await?;

        debug!("s3: touch bucket={} key={}", bucket, key);

        // Conditional put: only create if the object doesn't already exist
        let result = client
            .put_object()
            .bucket(&bucket)
            .key(&key)
            .if_none_match("*")
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b""))
            .send()
            .await;

        match result {
            Ok(_) => {
                self.notifier.notify(path);
                Ok(())
            }
            Err(e) => {
                // 412 Precondition Failed means the object already exists — that's fine for touch
                let is_precondition_failed = e
                    .raw_response()
                    .map(|r| r.status().as_u16() == 412)
                    .unwrap_or(false);

                if is_precondition_failed {
                    debug!("s3: touch object already exists (412), no-op");
                    Ok(())
                } else {
                    Err(Error::Custom(e.to_string()))
                }
            }
        }
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let (bucket, prefix) = Self::parse_path(path);
        let bucket = bucket.ok_or(Error::NotSupported)?;
        let key = prefix.ok_or_else(|| Error::Custom("no key specified".into()))?;
        let client = self.client_for_bucket(&bucket).await?;

        debug!("s3: create_directory bucket={} key={}", bucket, key);

        // S3 "directories" are zero-byte objects with a trailing /
        let dir_key = if key.ends_with('/') {
            key
        } else {
            format!("{}/", key)
        };

        client
            .put_object()
            .bucket(&bucket)
            .key(&dir_key)
            .body(aws_sdk_s3::primitives::ByteStream::from_static(b""))
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;
        self.notifier.notify(path);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// S3AsyncWriter — multipart upload writer
// ---------------------------------------------------------------------------

struct S3AsyncWriter {
    client: aws_sdk_s3::Client,
    bucket: String,
    key: String,
    upload_id: String,
    buffer: Vec<u8>,
    part_number: i32,
    completed_parts: Vec<aws_sdk_s3::types::CompletedPart>,
    notifier: VfsChangeNotifier,
    path: PathBuf,
}

impl S3AsyncWriter {
    async fn flush_part(&mut self) -> Result<(), Error> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.flush_part_unconditional().await
    }

    /// Upload the current buffer as a part, even if it's empty.
    async fn flush_part_unconditional(&mut self) -> Result<(), Error> {
        debug!(
            "s3: uploading part {} ({} bytes) for upload_id={}",
            self.part_number,
            self.buffer.len(),
            self.upload_id
        );
        let data = std::mem::take(&mut self.buffer);
        let body = aws_sdk_s3::primitives::ByteStream::from(data);

        let resp = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(&self.upload_id)
            .part_number(self.part_number)
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        self.completed_parts.push(
            aws_sdk_s3::types::CompletedPart::builder()
                .part_number(self.part_number)
                .e_tag(resp.e_tag().unwrap_or_default())
                .build(),
        );
        self.part_number += 1;

        Ok(())
    }

    async fn abort(&self) {
        warn!(
            "s3: aborting multipart upload upload_id={} bucket={} key={}",
            self.upload_id, self.bucket, self.key
        );
        let _ = self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(&self.upload_id)
            .send()
            .await;
    }
}

#[async_trait::async_trait]
impl VfsAsyncWriter for S3AsyncWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        self.buffer.extend_from_slice(buf);
        if self.buffer.len() >= MULTIPART_CHUNK_SIZE {
            if let Err(e) = self.flush_part().await {
                self.abort().await;
                return Err(e);
            }
        }
        Ok(buf.len())
    }

    async fn finish(mut self: Box<Self>) -> Result<(), Error> {
        // Always flush remaining data (even if empty) when no parts have been
        // uploaded yet — CompleteMultipartUpload requires at least one part.
        if self.completed_parts.is_empty() || !self.buffer.is_empty() {
            if let Err(e) = self.flush_part_unconditional().await {
                self.abort().await;
                return Err(e);
            }
        }

        let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(self.completed_parts.clone()))
            .build();

        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(&self.upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .map_err(|e| Error::Custom(e.to_string()))?;

        info!(
            "s3: completed multipart upload upload_id={} bucket={} key={} ({} parts)",
            self.upload_id,
            self.bucket,
            self.key,
            self.completed_parts.len()
        );
        self.notifier.notify(&self.path);
        Ok(())
    }
}

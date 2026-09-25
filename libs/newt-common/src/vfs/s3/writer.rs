//! Uploads. An object that fits in one part is a single PutObject; a larger
//! one becomes a multipart upload, created when its first part fills. A
//! `create_new` write puts `If-None-Match: *` on whichever request ends the
//! upload.

use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use log::{debug, info, warn};
use tokio::io::AsyncWriteExt;

use crate::Error;
use crate::vfs::attributes::WriteOptions;
use crate::vfs::path::PathBuf;
use crate::vfs::{VfsAsyncWriter, VfsChangeNotifier};

use super::{attributes, local_err, sdk_err};

/// The smallest part, and the most a single PutObject carries: an object
/// up to this size is one request.
pub(super) const PART_SIZE: u64 = 10 << 20;
/// S3's limits on a multipart upload.
const MAX_PARTS: u64 = 10_000;
const MAX_PART_SIZE: u64 = 5 << 30;
/// Part bytes held in memory; a larger part spills to a temp file.
const PART_MEMORY: u64 = 32 << 20;
/// Parts double in size every this many, so an upload that outgrows its
/// size hint, or had none, still fits the part cap: about 10 TB without a
/// hint.
const PARTS_PER_DOUBLING: u64 = 1_000;

/// How large part `number` (from 1) grows before it is sent: big enough
/// for `size_hint` to fit the part cap, and growing with the part count.
pub(super) fn part_size(number: u64, size_hint: Option<u64>) -> u64 {
    let hinted = size_hint.map_or(0, |size| size.div_ceil(MAX_PARTS).next_multiple_of(1 << 20));
    let grown = PART_SIZE << (number.saturating_sub(1) / PARTS_PER_DOUBLING).min(16);
    hinted.max(grown).clamp(PART_SIZE, MAX_PART_SIZE)
}

/// Applies the attributes decided at creation to a PutObject or a
/// CreateMultipartUpload, which take the same settings.
macro_rules! with_write_options {
    ($request:expr, $options:expr) => {{
        let options: &WriteOptions = $options;
        let mut request = $request
            .set_storage_class(
                options
                    .object_storage_class
                    .as_deref()
                    .map(aws_sdk_s3::types::StorageClass::from),
            )
            .set_acl(
                options
                    .object_canned_acl
                    .as_deref()
                    .map(aws_sdk_s3::types::ObjectCannedAcl::from),
            );
        if let Some(meta) = &options.object_metadata {
            request = request
                .set_metadata(Some(meta.metadata.clone().into_iter().collect()))
                .set_content_type(meta.content_type.clone())
                .set_content_encoding(meta.content_encoding.clone())
                .set_content_language(meta.content_language.clone())
                .set_content_disposition(meta.content_disposition.clone())
                .set_cache_control(meta.cache_control.clone())
                .set_expires(attributes::expires(meta)?);
        }
        request
    }};
}

/// One part's bytes, in memory or spilled to a temp file.
enum Part {
    Memory(Vec<u8>),
    Disk {
        file: tokio::fs::File,
        path: tempfile::TempPath,
        len: u64,
    },
}

impl Part {
    fn empty() -> Self {
        Part::Memory(Vec::new())
    }

    fn len(&self) -> u64 {
        match self {
            Part::Memory(buf) => buf.len() as u64,
            Part::Disk { len, .. } => *len,
        }
    }

    async fn push(&mut self, data: &[u8]) -> Result<(), Error> {
        if let Part::Memory(buf) = self
            && (buf.len() + data.len()) as u64 > PART_MEMORY
        {
            let held = std::mem::take(buf);
            let (file, path) = tokio::task::spawn_blocking(|| {
                tempfile::Builder::new().prefix("newt-s3-part-").tempfile()
            })
            .await?
            .map_err(|e| Error::custom(format!("could not create an upload part file: {e}")))?
            .into_parts();
            let mut file = tokio::fs::File::from_std(file);
            file.write_all(&held).await?;
            *self = Part::Disk {
                file,
                path,
                len: held.len() as u64,
            };
        }
        match self {
            Part::Memory(buf) => buf.extend_from_slice(data),
            Part::Disk { file, len, .. } => {
                file.write_all(data).await?;
                *len += data.len() as u64;
            }
        }
        Ok(())
    }

    /// The part as a request body. A spilled part's file is kept by the
    /// returned path until the request is done: the SDK re-reads it to
    /// retry.
    async fn into_body(self) -> Result<(ByteStream, Option<tempfile::TempPath>), Error> {
        match self {
            Part::Memory(buf) => Ok((ByteStream::from(buf), None)),
            Part::Disk { mut file, path, .. } => {
                file.flush().await?;
                drop(file);
                let body = ByteStream::from_path(&path).await.map_err(local_err)?;
                Ok((body, Some(path)))
            }
        }
    }
}

struct Upload {
    id: String,
    parts: Vec<CompletedPart>,
}

pub(super) struct S3AsyncWriter {
    client: aws_sdk_s3::Client,
    bucket: String,
    key: String,
    options: WriteOptions,
    part: Part,
    upload: Option<Upload>,
    /// Set once the upload was completed or aborted. The drop guard aborts
    /// an upload discarded mid-stream (a cancelled or failed copy), which
    /// would otherwise leak.
    terminated: bool,
    notifier: VfsChangeNotifier,
    path: PathBuf,
}

impl S3AsyncWriter {
    pub(super) fn new(
        client: aws_sdk_s3::Client,
        bucket: String,
        key: String,
        options: WriteOptions,
        notifier: VfsChangeNotifier,
        path: PathBuf,
    ) -> Self {
        Self {
            client,
            bucket,
            key,
            options,
            part: Part::empty(),
            upload: None,
            terminated: false,
            notifier,
            path,
        }
    }

    fn next_part_size(&self) -> u64 {
        let number = self.upload.as_ref().map_or(0, |u| u.parts.len()) as u64 + 1;
        part_size(number, self.options.size_hint)
    }

    fn if_none_match(&self) -> Option<String> {
        self.options.create_new.then(|| "*".to_string())
    }

    /// Send the buffered part, creating the upload first for the first one.
    async fn send_part(&mut self) -> Result<(), Error> {
        if self.upload.is_none() {
            let request = with_write_options!(
                self.client
                    .create_multipart_upload()
                    .bucket(&self.bucket)
                    .key(&self.key),
                &self.options
            );
            let resp = request.send().await.map_err(sdk_err)?;
            let id = resp
                .upload_id()
                .ok_or_else(|| Error::custom("no upload_id returned"))?
                .to_string();
            debug!(
                "s3: multipart upload_id={} bucket={} key={}",
                id, self.bucket, self.key
            );
            self.upload = Some(Upload {
                id,
                parts: Vec::new(),
            });
        }
        let part = std::mem::replace(&mut self.part, Part::empty());
        let len = part.len();
        let (body, _spill) = part.into_body().await?;
        let upload = self.upload.as_mut().expect("created above");
        let number = upload.parts.len() as i32 + 1;
        debug!(
            "s3: uploading part {} ({} bytes) for upload_id={}",
            number, len, upload.id
        );
        let resp = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(&upload.id)
            .part_number(number)
            .body(body)
            .send()
            .await
            .map_err(sdk_err)?;
        upload.parts.push(
            CompletedPart::builder()
                .part_number(number)
                .e_tag(resp.e_tag().unwrap_or_default())
                .build(),
        );
        Ok(())
    }

    async fn push(&mut self, mut data: &[u8]) -> Result<(), Error> {
        while !data.is_empty() {
            let size = self.next_part_size();
            // A full part is sent only once more data arrives, so an
            // object of exactly one part still ends in a single PutObject.
            if self.part.len() >= size {
                self.send_part().await?;
                continue;
            }
            let room = (size - self.part.len()).min(data.len() as u64) as usize;
            let (now, rest) = data.split_at(room);
            self.part.push(now).await?;
            data = rest;
        }
        Ok(())
    }

    /// The whole object in one PutObject.
    async fn put(&mut self) -> Result<(), Error> {
        let part = std::mem::replace(&mut self.part, Part::empty());
        let len = part.len();
        let (body, _spill) = part.into_body().await?;
        let request = with_write_options!(
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&self.key)
                .body(body),
            &self.options
        );
        request
            .set_if_none_match(self.if_none_match())
            .send()
            .await
            .map_err(sdk_err)?;
        debug!(
            "s3: put bucket={} key={} ({} bytes)",
            self.bucket, self.key, len
        );
        Ok(())
    }

    async fn complete(&mut self) -> Result<(), Error> {
        if self.part.len() > 0 {
            self.send_part().await?;
        }
        let if_none_match = self.if_none_match();
        let upload = self.upload.as_ref().expect("an upload in progress");
        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(&upload.id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(upload.parts.clone()))
                    .build(),
            )
            .set_if_none_match(if_none_match)
            .send()
            .await
            .map_err(sdk_err)?;
        info!(
            "s3: completed multipart upload upload_id={} bucket={} key={} ({} parts)",
            upload.id,
            self.bucket,
            self.key,
            upload.parts.len()
        );
        Ok(())
    }

    async fn abort(&mut self) {
        self.terminated = true;
        let Some(upload) = &self.upload else {
            return;
        };
        warn!(
            "s3: aborting multipart upload upload_id={} bucket={} key={}",
            upload.id, self.bucket, self.key
        );
        let _ = self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(&upload.id)
            .send()
            .await;
    }
}

impl Drop for S3AsyncWriter {
    fn drop(&mut self) {
        if self.terminated {
            return;
        }
        let Some(upload) = &self.upload else {
            return;
        };
        warn!(
            "s3: writer dropped mid-upload, aborting multipart upload upload_id={} bucket={} key={}",
            upload.id, self.bucket, self.key
        );
        let request = self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(&upload.id);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = request.send().await;
            });
        }
    }
}

#[async_trait::async_trait]
impl VfsAsyncWriter for S3AsyncWriter {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        if let Err(e) = self.push(buf).await {
            self.abort().await;
            return Err(e);
        }
        Ok(buf.len())
    }

    async fn finish(mut self: Box<Self>) -> Result<(), Error> {
        let result = if self.upload.is_none() {
            self.put().await
        } else {
            self.complete().await
        };
        if let Err(e) = result {
            self.abort().await;
            return Err(e);
        }
        self.terminated = true;
        self.notifier.notify(&self.path);
        Ok(())
    }
}

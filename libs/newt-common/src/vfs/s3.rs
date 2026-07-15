use std::collections::HashMap;
use std::sync::Arc;

use log::{debug, info, warn};
use parking_lot::Mutex;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::api::MountContext;
use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::{File, FsStats};
use crate::vfs::S3Credentials;
use crate::vfs::path::{Path, PathBuf};
use crate::{Error, ToUnix};

use super::properties::{
    PropertyField, PropertyFieldValue, PropertyGrant, PropertyGrantee, PropertyGroup,
    PropertyPatch, PropertySheet, PropertyValuePatch,
};
use super::{
    Breadcrumb, DisplayPathMatch, RegisteredDescriptor, Vfs, VfsAsyncWriter, VfsChangeNotifier,
    VfsDescriptor,
};

const MULTIPART_CHUNK_SIZE: usize = 10 * 1024 * 1024; // 10 MB

/// `SdkError`'s bare `Display` is just "service error" — render the full
/// source chain (service error code and message included) instead.
fn sdk_err<E: std::error::Error>(e: E) -> Error {
    Error::custom(aws_sdk_s3::error::DisplayErrorContext(&e).to_string())
}

// ---------------------------------------------------------------------------
// S3VfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct S3VfsDescriptor;

impl VfsDescriptor for S3VfsDescriptor {
    fn type_name(&self) -> &'static str {
        "s3"
    }
    fn display_name(&self) -> &'static str {
        "S3"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
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
    fn has_extended_properties(&self) -> bool {
        true
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
    fn can_stat_directories(&self) -> bool {
        false
    }
    fn can_fs_stats(&self) -> bool {
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
    fn auto_refresh(&self) -> bool {
        false
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        let bucket_prefix = String::from_utf8_lossy(mount_meta);
        let s = path.components().collect::<Vec<_>>().join("/");
        if bucket_prefix.is_empty() {
            if s.is_empty() {
                "s3://".to_string()
            } else {
                format!("s3://{}", s)
            }
        } else if s.is_empty() {
            format!("s3://{}/", bucket_prefix)
        } else {
            format!("s3://{}/{}", bucket_prefix, s)
        }
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        let bucket_prefix = String::from_utf8_lossy(mount_meta);
        let mut crumbs = super::unix_breadcrumbs(path);
        if let Some(root) = crumbs.first_mut() {
            root.label = if bucket_prefix.is_empty() {
                "s3://".to_string()
            } else {
                format!("s3://{}/", bucket_prefix)
            };
        }
        crumbs
    }

    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        let s = String::from_utf8_lossy(mount_meta);
        if s.is_empty() {
            None
        } else {
            Some(s.into_owned())
        }
    }

    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        let rest = input.strip_prefix("s3://")?;
        let bucket_prefix = String::from_utf8_lossy(mount_meta);

        if bucket_prefix.is_empty() {
            // Unscoped: any s3:// path matches generically
            Some(DisplayPathMatch::generic(PathBuf::from_wire_str(rest)))
        } else {
            // Scoped: only match if the bucket prefix matches
            let after_bucket = rest
                .strip_prefix(bucket_prefix.as_ref())
                .and_then(|s| s.strip_prefix('/').or(Some(s)))?;
            Some(DisplayPathMatch::exact(PathBuf::from_wire_str(
                after_bucket,
            )))
        }
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
    /// When set, the VFS is scoped to this bucket (root = bucket contents).
    scoped_bucket: Option<String>,
    /// Change notifier for self-notification on mutations.
    notifier: VfsChangeNotifier,
}

impl S3Vfs {
    /// Build an `S3Vfs` from a `MountRequest::S3` payload. Resolves
    /// region/profile/credentials, optionally performs an AssumeRole
    /// round-trip, and constructs the SDK client.
    pub async fn mount(
        region: Option<String>,
        bucket: Option<String>,
        credentials: S3Credentials,
        _ctx: &MountContext<'_>,
    ) -> Result<Arc<dyn Vfs>, Error> {
        let region = aws_config::Region::new(region.unwrap_or_else(|| "us-east-1".to_string()));

        let mut config_loader = aws_config::from_env().region(region.clone());

        // Custom endpoint for S3-compatible services
        if let Some(ref endpoint) = credentials.endpoint_url {
            config_loader = config_loader.endpoint_url(endpoint);
        }

        // Use explicit profile if specified
        if let Some(ref profile) = credentials.profile {
            config_loader = config_loader.profile_name(profile);
        }

        // Use explicit IAM credentials if provided
        if let (Some(access_key), Some(secret_key)) =
            (&credentials.access_key_id, &credentials.secret_access_key)
        {
            let creds = aws_sdk_s3::config::Credentials::new(
                access_key,
                secret_key,
                credentials.session_token.clone(),
                None,
                "newt-explicit",
            );
            config_loader = config_loader.credentials_provider(creds);
        }

        let mut sdk_config = config_loader.load().await;

        // AssumeRole: use the resolved credentials to assume a role,
        // then rebuild the config with the temporary credentials.
        if let Some(ref role_arn) = credentials.role_arn {
            let sts_client = aws_sdk_sts::Client::new(&sdk_config);
            let mut assume = sts_client
                .assume_role()
                .role_arn(role_arn)
                .role_session_name("newt-session");
            if let Some(ref ext_id) = credentials.external_id {
                assume = assume.external_id(ext_id);
            }
            let resp = assume.send().await.map_err(|e| Error {
                kind: crate::ErrorKind::Other,
                message: format!(
                    "AssumeRole failed: {}",
                    aws_sdk_s3::error::DisplayErrorContext(&e)
                ),
            })?;
            let sts_creds = resp.credentials().ok_or_else(|| Error {
                kind: crate::ErrorKind::Other,
                message: "AssumeRole returned no credentials".into(),
            })?;
            let temp_creds = aws_sdk_s3::config::Credentials::new(
                sts_creds.access_key_id(),
                sts_creds.secret_access_key(),
                Some(sts_creds.session_token().to_string()),
                None,
                "newt-assume-role",
            );
            sdk_config = aws_config::from_env()
                .region(region)
                .credentials_provider(temp_creds)
                .load()
                .await;
        }

        let client = aws_sdk_s3::Client::new(&sdk_config);
        Ok(Arc::new(S3Vfs::new(client, sdk_config, bucket)))
    }

    pub fn new(
        default_client: aws_sdk_s3::Client,
        sdk_config: aws_config::SdkConfig,
        scoped_bucket: Option<String>,
    ) -> Self {
        Self {
            default_client,
            sdk_config: aws_sdk_s3::config::Builder::from(&sdk_config),
            region_clients: Mutex::new(HashMap::new()),
            bucket_regions: Mutex::new(HashMap::new()),
            scoped_bucket,
            notifier: VfsChangeNotifier::new(),
        }
    }

    /// Get or create an S3 client for the given bucket's region.
    async fn client_for_bucket(&self, bucket: &str) -> Result<aws_sdk_s3::Client, Error> {
        // Check cache first
        if let Some(region) = self.bucket_regions.lock().get(bucket).cloned()
            && let Some(client) = self.region_clients.lock().get(&region).cloned()
        {
            debug!(
                "s3: client cache hit for bucket={} region={}",
                bucket, region
            );
            return Ok(client);
        }

        // Discover region via GetBucketLocation
        let resp = self
            .default_client
            .get_bucket_location()
            .bucket(bucket)
            .send()
            .await
            .map_err(sdk_err)?;

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
    ///
    /// When scoped to a bucket, the bucket is prepended to the path:
    /// `/` → (Some("scoped-bucket"), None)
    /// `/some/prefix/` → (Some("scoped-bucket"), Some("some/prefix/"))
    fn parse_path(&self, path: &Path) -> (Option<String>, Option<String>) {
        let s = path.as_wire_str();
        let s = s.trim_start_matches('/');

        // When scoped to a bucket, treat the entire path as a prefix within it
        if let Some(ref bucket) = self.scoped_bucket {
            if s.is_empty() {
                return (Some(bucket.clone()), None);
            }
            return (Some(bucket.clone()), Some(s.to_string()));
        }

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
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<Vec<File>, Error> {
        let resp = self
            .default_client
            .list_buckets()
            .send()
            .await
            .map_err(sdk_err)?;

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
                symlink_target: None,
                user: None,
                group: None,
                mode: None,
                modified: None,
                accessed: None,
                created,
                key: None,
                source: None,
            });
        }

        if let Some(tx) = batch_tx
            && !files.is_empty()
        {
            let _ = tx.send(files.clone()).await;
        }

        Ok(files)
    }

    async fn list_objects(
        &self,
        bucket: &str,
        prefix: Option<&str>,
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<Vec<File>, Error> {
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

        // ".." entry — skip at the root of a scoped bucket (nowhere to go up to)
        let at_scoped_root = self.scoped_bucket.is_some() && prefix.is_none();
        if !at_scoped_root {
            files.push(File {
                name: "..".to_string(),
                size: None,
                is_dir: true,
                is_hidden: false,
                is_symlink: false,
                symlink_target: None,
                user: None,
                group: None,
                mode: None,
                modified: None,
                accessed: None,
                created: None,
                key: None,
                source: None,
            });
        }

        debug!("s3: list_objects bucket={} prefix={:?}", bucket, prefix);

        let client = self.client_for_bucket(bucket).await?;

        let mut request = client.list_objects_v2().bucket(bucket).delimiter("/");

        if let Some(p) = prefix {
            request = request.prefix(p);
        }

        let mut continuation_token: Option<String> = None;
        loop {
            let mut req = request.clone();
            if let Some(ref token) = continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req.send().await.map_err(sdk_err)?;

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
                            symlink_target: None,
                            user: None,
                            group: None,
                            mode: None,
                            modified: None,
                            accessed: None,
                            created: None,
                            key: None,
                            source: None,
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
                        symlink_target: None,
                        user: obj
                            .owner()
                            .and_then(|o| o.display_name())
                            .map(|n| crate::filesystem::UserGroup::Name(n.to_string())),
                        group: None,
                        mode: None,
                        modified,
                        accessed: None,
                        created: None,
                        key: None,
                        source: None,
                    });
                }
            }

            if let Some(tx) = &batch_tx
                && !batch.is_empty()
            {
                let _ = tx.send(batch.clone()).await;
            }
            files.extend(batch);

            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        Ok(files)
    }
}

// ---------------------------------------------------------------------------
// Property sheet — ACLs, user metadata, storage class, system headers
// ---------------------------------------------------------------------------

fn grant_from_s3(grant: &aws_sdk_s3::types::Grant) -> Option<PropertyGrant> {
    use aws_sdk_s3::types::Type;
    let grantee = grant.grantee()?;
    let permission = grant.permission()?.as_str().to_string();
    let grantee = match grantee.r#type() {
        Type::CanonicalUser => PropertyGrantee::User {
            id: grantee.id()?.to_string(),
            display_name: grantee.display_name().map(str::to_string),
        },
        Type::Group => PropertyGrantee::Group {
            uri: grantee.uri()?.to_string(),
        },
        Type::AmazonCustomerByEmail => PropertyGrantee::Email {
            address: grantee.email_address()?.to_string(),
        },
        _ => return None,
    };
    Some(PropertyGrant {
        grantee,
        permission,
    })
}

fn grant_to_s3(grant: &PropertyGrant) -> Result<aws_sdk_s3::types::Grant, Error> {
    use aws_sdk_s3::types::{Grant, Grantee, Permission, Type};
    let grantee = match &grant.grantee {
        PropertyGrantee::User { id, display_name } => Grantee::builder()
            .r#type(Type::CanonicalUser)
            .id(id)
            .set_display_name(display_name.clone())
            .build(),
        PropertyGrantee::Group { uri } => Grantee::builder().r#type(Type::Group).uri(uri).build(),
        PropertyGrantee::Email { address } => Grantee::builder()
            .r#type(Type::AmazonCustomerByEmail)
            .email_address(address)
            .build(),
    }
    .map_err(sdk_err)?;
    Ok(Grant::builder()
        .grantee(grantee)
        .permission(Permission::from(grant.permission.as_str()))
        .build())
}

/// The default ACL every object gets: the bucket owner with FULL_CONTROL
/// and nothing else. Rewrites skip re-putting this — both to save a call
/// and to keep metadata edits working on ACL-disabled buckets (where
/// PutObjectAcl with an explicit policy is rejected).
fn is_owner_only_acl(acl: &aws_sdk_s3::operation::get_object_acl::GetObjectAclOutput) -> bool {
    let owner_id = acl.owner().and_then(|o| o.id());
    acl.grants().len() <= 1
        && acl.grants().iter().all(|g| {
            g.permission() == Some(&aws_sdk_s3::types::Permission::FullControl)
                && g.grantee()
                    .is_some_and(|gr| gr.id().is_some() && gr.id() == owner_id)
        })
}

/// Snapshot an object's ACL when it differs from the owner default.
/// `None` on read failure too (GetObjectAcl needs its own permission) —
/// callers skip the restore in both cases.
async fn snapshot_nontrivial_acl(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
) -> Option<aws_sdk_s3::operation::get_object_acl::GetObjectAclOutput> {
    client
        .get_object_acl()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .ok()
        .filter(|acl| !is_owner_only_acl(acl))
}

/// Re-put a snapshotted ACL onto an object (Copy/CopyObject always reset
/// the destination ACL to the bucket-owner default).
async fn restore_acl(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    acl: &aws_sdk_s3::operation::get_object_acl::GetObjectAclOutput,
) -> Result<(), Error> {
    let policy = aws_sdk_s3::types::AccessControlPolicy::builder()
        .set_owner(acl.owner().cloned())
        .set_grants(Some(acl.grants().to_vec()))
        .build();
    client
        .put_object_acl()
        .bucket(bucket)
        .key(key)
        .access_control_policy(policy)
        .send()
        .await
        .map_err(sdk_err)?;
    Ok(())
}

fn text_field(key: &str, label: &str, value: &str) -> PropertyField {
    PropertyField {
        key: key.to_string(),
        label: label.to_string(),
        value: PropertyFieldValue::Text {
            value: Some(value.to_string()),
        },
        editable: true,
        write_only: false,
    }
}

impl S3Vfs {
    async fn build_property_sheet(&self, path: &Path) -> Result<PropertySheet, Error> {
        use aws_sdk_s3::types::{ObjectCannedAcl, Permission, StorageClass};

        let (bucket, key) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = key.ok_or_else(|| Error::custom("no object key specified"))?;
        let client = self.client_for_bucket(&bucket).await?;

        let head = client
            .head_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(sdk_err)?;

        let meta_entries = head
            .metadata()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), Some(v.clone())))
                    .collect()
            })
            .unwrap_or_default();
        // HeadObject omits the storage class for STANDARD objects.
        let storage_class = head
            .storage_class()
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| StorageClass::Standard.as_str().to_string());

        let mut groups = vec![PropertyGroup {
            label: "S3 metadata".to_string(),
            fields: vec![
                PropertyField {
                    key: "s3.meta".to_string(),
                    label: "User metadata".to_string(),
                    value: PropertyFieldValue::Map {
                        entries: meta_entries,
                    },
                    editable: true,
                    write_only: false,
                },
                PropertyField {
                    key: "s3.storage_class".to_string(),
                    label: "Storage class".to_string(),
                    value: PropertyFieldValue::Choice {
                        choices: StorageClass::values()
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        value: Some(storage_class),
                    },
                    editable: true,
                    write_only: false,
                },
                text_field(
                    "s3.content_type",
                    "Content-Type",
                    head.content_type().unwrap_or_default(),
                ),
                text_field(
                    "s3.cache_control",
                    "Cache-Control",
                    head.cache_control().unwrap_or_default(),
                ),
            ],
        }];

        // ACL read requires a separate permission (s3:GetObjectAcl) —
        // degrade to a metadata-only sheet rather than failing outright.
        match client
            .get_object_acl()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(acl) => {
                let grants: Vec<PropertyGrant> =
                    acl.grants().iter().filter_map(grant_from_s3).collect();
                groups.push(PropertyGroup {
                    label: "S3 access control".to_string(),
                    fields: vec![
                        PropertyField {
                            key: "s3.acl.grants".to_string(),
                            label: "Grants".to_string(),
                            value: PropertyFieldValue::Grants {
                                permission_choices: Permission::values()
                                    .iter()
                                    .map(|s| s.to_string())
                                    .collect(),
                                value: Some(grants),
                            },
                            editable: true,
                            write_only: false,
                        },
                        PropertyField {
                            key: "s3.acl.canned".to_string(),
                            label: "Canned ACL".to_string(),
                            value: PropertyFieldValue::Choice {
                                choices: ObjectCannedAcl::values()
                                    .iter()
                                    .map(|s| s.to_string())
                                    .collect(),
                                value: None,
                            },
                            editable: true,
                            write_only: true,
                        },
                    ],
                });
            }
            Err(e) => {
                debug!("s3: get_object_acl failed, omitting ACL fields: {}", e);
            }
        }

        Ok(PropertySheet {
            groups,
            apply_hint: Some(
                "Metadata changes rewrite the object in place; this can be slow for large objects."
                    .to_string(),
            ),
        })
    }

    async fn apply_property_patch(&self, path: &Path, patch: &PropertyPatch) -> Result<(), Error> {
        use aws_sdk_s3::types::{
            AccessControlPolicy, MetadataDirective, ObjectCannedAcl, StorageClass,
        };

        let (bucket, key) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = key.ok_or_else(|| Error::custom("no object key specified"))?;
        let client = self.client_for_bucket(&bucket).await?;

        let mut meta_patch = None;
        let mut content_type = None;
        let mut cache_control = None;
        let mut storage_class = None;
        let mut canned_acl = None;
        let mut grants = None;

        for op in &patch.ops {
            match (op.key.as_str(), &op.op) {
                ("s3.meta", PropertyValuePatch::MapPatch { set, delete }) => {
                    meta_patch = Some((set, delete));
                }
                ("s3.content_type", PropertyValuePatch::Set { value }) => {
                    content_type = Some(value.clone());
                }
                ("s3.cache_control", PropertyValuePatch::Set { value }) => {
                    cache_control = Some(value.clone());
                }
                ("s3.storage_class", PropertyValuePatch::Set { value }) => {
                    storage_class = Some(value.clone());
                }
                ("s3.acl.canned", PropertyValuePatch::Set { value }) => {
                    canned_acl = Some(value.clone());
                }
                ("s3.acl.grants", PropertyValuePatch::ReplaceGrants { grants: g }) => {
                    grants = Some(g);
                }
                (key, _) => {
                    return Err(Error::custom(format!(
                        "unknown property patch target: {}",
                        key
                    )));
                }
            }
        }

        let needs_rewrite = meta_patch.is_some()
            || content_type.is_some()
            || cache_control.is_some()
            || storage_class.is_some();

        if needs_rewrite {
            // CopyObject with REPLACE wipes every header not re-specified,
            // so read the current state and merge the patch over it.
            let head = client
                .head_object()
                .bucket(&bucket)
                .key(&key)
                .send()
                .await
                .map_err(sdk_err)?;

            // CopyObject also resets the ACL to the owner default —
            // snapshot a non-trivial ACL so it survives the rewrite
            // (unless this patch is about to overwrite it anyway).
            let acl_snapshot = if canned_acl.is_none() && grants.is_none() {
                snapshot_nontrivial_acl(&client, &bucket, &key).await
            } else {
                None
            };

            let mut metadata = head.metadata().cloned().unwrap_or_default();
            if let Some((set, delete)) = meta_patch {
                for k in delete {
                    metadata.remove(k);
                }
                for (k, v) in set {
                    metadata.insert(k.clone(), v.clone());
                }
            }

            debug!(
                "s3: rewriting {}/{} for property apply ({} metadata keys)",
                bucket,
                key,
                metadata.len()
            );

            let mut req = client
                .copy_object()
                .bucket(&bucket)
                .key(&key)
                .copy_source(format!("{}/{}", bucket, key))
                .metadata_directive(MetadataDirective::Replace)
                .set_metadata(Some(metadata))
                .set_content_type(content_type.or_else(|| head.content_type().map(str::to_string)))
                .set_cache_control(
                    cache_control.or_else(|| head.cache_control().map(str::to_string)),
                )
                .set_content_encoding(head.content_encoding().map(str::to_string))
                .set_content_disposition(head.content_disposition().map(str::to_string))
                .set_content_language(head.content_language().map(str::to_string));

            // The destination defaults to STANDARD regardless of the
            // source, so always re-specify the (possibly unchanged) class.
            let class = storage_class
                .map(|s| StorageClass::from(s.as_str()))
                .or_else(|| head.storage_class().cloned());
            req = req.set_storage_class(class);

            req.send().await.map_err(sdk_err)?;

            if let Some(acl) = acl_snapshot {
                restore_acl(&client, &bucket, &key, &acl)
                    .await
                    .map_err(|e| {
                        Error::custom(format!(
                            "metadata updated, but restoring the ACL failed: {}",
                            e
                        ))
                    })?;
            }
        }

        // ACL ops after the rewrite so they land on the new object.
        if let Some(canned) = canned_acl {
            client
                .put_object_acl()
                .bucket(&bucket)
                .key(&key)
                .acl(ObjectCannedAcl::from(canned.as_str()))
                .send()
                .await
                .map_err(sdk_err)?;
        }

        if let Some(grants) = grants {
            // PutObjectAcl with an explicit grant list requires the owner.
            let current = client
                .get_object_acl()
                .bucket(&bucket)
                .key(&key)
                .send()
                .await
                .map_err(sdk_err)?;
            let s3_grants: Vec<_> = grants.iter().map(grant_to_s3).collect::<Result<_, _>>()?;
            let policy = AccessControlPolicy::builder()
                .set_owner(current.owner().cloned())
                .set_grants(Some(s3_grants))
                .build();
            client
                .put_object_acl()
                .bucket(&bucket)
                .key(&key)
                .access_control_policy(policy)
                .send()
                .await
                .map_err(sdk_err)?;
        }

        self.notifier.notify(path);
        Ok(())
    }
}

#[async_trait::async_trait]
impl Vfs for S3Vfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &S3_VFS_DESCRIPTOR
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.scoped_bucket
            .as_deref()
            .unwrap_or("")
            .as_bytes()
            .to_vec()
    }

    async fn list_files(
        &self,
        path: &Path,
        batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<super::VfsFileList, Error> {
        let (bucket, prefix) = self.parse_path(path);
        let files = match bucket {
            None => self.list_buckets(batch_tx).await?,
            Some(bucket) => {
                self.list_objects(&bucket, prefix.as_deref(), batch_tx)
                    .await?
            }
        };
        Ok(files.into())
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<FsStats>, Error> {
        Ok(None)
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        self.notifier.watch(path).await;
        Ok(())
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        let (bucket, prefix) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = prefix.ok_or_else(|| Error::custom("no object key specified"))?;
        let client = self.client_for_bucket(&bucket).await?;

        let resp = client
            .head_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(sdk_err)?;

        let size = resp.content_length().unwrap_or(0) as u64;
        let modified = resp.last_modified().and_then(|d| {
            let secs = d.secs();
            let nanos = d.subsec_nanos();
            std::time::SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::new(secs as u64, nanos))
                .map(|t| t.to_unix())
        });

        let name = path.file_name().map(|n| n.to_string()).unwrap_or_default();

        Ok(File {
            name,
            size: Some(size),
            is_dir: false,
            is_hidden: false,
            is_symlink: false,
            symlink_target: None,
            user: None,
            group: None,
            mode: None,
            modified,
            accessed: None,
            created: None,
            key: None,
            source: None,
        })
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        let (bucket, prefix) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = prefix.ok_or_else(|| Error::custom("no object key specified"))?;
        let client = self.client_for_bucket(&bucket).await?;

        let resp = client
            .head_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(sdk_err)?;

        let size = resp.content_length().unwrap_or(0) as u64;
        let mime_type = resp
            .content_type()
            .map(|s| s.to_string())
            .filter(|s| s != "application/octet-stream")
            .or_else(|| {
                crate::file_reader::guess_mime_type(std::path::Path::new(path.as_wire_str()))
            });
        let is_dir = key.ends_with('/') && size == 0;

        let modified = resp.last_modified().and_then(|d| {
            let secs = d.secs();
            let nanos = d.subsec_nanos();
            std::time::SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::new(secs as u64, nanos))
                .map(|t| t.to_unix())
        });

        Ok(FileDetails {
            size,
            mime_type,
            is_dir,
            is_symlink: false,
            symlink_target: None,
            user: None,
            group: None,
            mode: None,
            modified,
            accessed: None,
            created: None,
        })
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        let (bucket, prefix) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = prefix.ok_or_else(|| Error::custom("no object key specified"))?;
        let client = self.client_for_bucket(&bucket).await?;

        let end = offset + length - 1;
        let range = format!("bytes={}-{}", offset, end);

        let resp = match client
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .range(range)
            .send()
            .await
        {
            Ok(resp) => resp,
            // A range starting at or past the object size gets 416 rather
            // than an empty body — map it to POSIX read-at-EOF semantics.
            // The 416 response still carries the size ("Content-Range:
            // bytes */12345").
            Err(e) if e.raw_response().is_some_and(|r| r.status().as_u16() == 416) => {
                let total_size = e
                    .raw_response()
                    .and_then(|r| r.headers().get("content-range"))
                    .and_then(|v| v.rsplit('/').next())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(offset);
                return Ok(FileChunk {
                    data: Vec::new(),
                    offset,
                    total_size,
                });
            }
            Err(e) => return Err(sdk_err(e)),
        };

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
            .map_err(sdk_err)?
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
        let (bucket, prefix) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = prefix.ok_or_else(|| Error::custom("no object key specified"))?;
        let client = self.client_for_bucket(&bucket).await?;

        let resp = client
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(sdk_err)?;

        Ok(Box::new(resp.body.into_async_read()))
    }

    async fn overwrite_async(&self, path: &Path) -> Result<Box<dyn VfsAsyncWriter>, Error> {
        let (bucket, prefix) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = prefix.ok_or_else(|| Error::custom("no object key specified"))?;
        let client = self.client_for_bucket(&bucket).await?;

        debug!(
            "s3: initiating multipart upload bucket={} key={}",
            bucket, key
        );

        let resp = client
            .create_multipart_upload()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(sdk_err)?;

        let upload_id = resp
            .upload_id()
            .ok_or_else(|| Error::custom("no upload_id returned"))?
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
            path: path.to_owned(),
            terminated: false,
        }))
    }

    async fn remove_file(&self, path: &Path) -> Result<(), Error> {
        let (bucket, prefix) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = prefix.ok_or(Error::not_supported())?;

        debug!("s3: remove_file bucket={} key={}", bucket, key);

        let client = self.client_for_bucket(&bucket).await?;
        client
            .delete_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .map_err(sdk_err)?;

        self.notifier.notify(path);
        Ok(())
    }

    async fn remove_dir(&self, path: &Path) -> Result<(), Error> {
        let (bucket, prefix) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = prefix.ok_or(Error::not_supported())?;

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
            .map_err(sdk_err)?;

        self.notifier.notify(path);
        Ok(())
    }

    async fn copy_within(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let (src_bucket, src_key) = self.parse_path(from);
        let src_bucket = src_bucket.ok_or(Error::not_supported())?;
        let src_key = src_key.ok_or_else(|| Error::custom("no source key"))?;

        let (dst_bucket, dst_key) = self.parse_path(to);
        let dst_bucket = dst_bucket.ok_or(Error::not_supported())?;
        let dst_key = dst_key.ok_or_else(|| Error::custom("no destination key"))?;

        debug!(
            "s3: copy_within {}/{} -> {}/{}",
            src_bucket, src_key, dst_bucket, dst_key
        );

        let client = self.client_for_bucket(&dst_bucket).await?;
        let src_client = self.client_for_bucket(&src_bucket).await?;
        let copy_source = format!("{}/{}", src_bucket, src_key);

        // CopyObject carries user metadata and headers over by default
        // (MetadataDirective COPY), but the destination's storage class
        // defaults to STANDARD and its ACL always resets to the bucket-
        // owner default — snapshot both from the source explicitly.
        let head = src_client
            .head_object()
            .bucket(&src_bucket)
            .key(&src_key)
            .send()
            .await
            .map_err(sdk_err)?;

        // CopyObject caps out at 5 GiB per atomic copy — signal
        // NotSupported so the copy cascade falls back to streaming.
        if head.content_length().unwrap_or(0) as u64 > 5 * 1024 * 1024 * 1024 {
            return Err(Error::not_supported());
        }

        let acl_snapshot = snapshot_nontrivial_acl(&src_client, &src_bucket, &src_key).await;

        client
            .copy_object()
            .bucket(&dst_bucket)
            .key(&dst_key)
            .copy_source(&copy_source)
            .set_storage_class(head.storage_class().cloned())
            .send()
            .await
            .map_err(sdk_err)?;

        if let Some(acl) = acl_snapshot {
            // Losing the grants beats failing the copy: the caller's next
            // strategy (streaming re-upload) couldn't restore them either.
            if let Err(e) = restore_acl(&client, &dst_bucket, &dst_key, &acl).await {
                warn!(
                    "s3: copy_within: failed to restore ACL on {}/{}: {}",
                    dst_bucket, dst_key, e
                );
            }
        }

        self.notifier.notify(to);
        Ok(())
    }

    async fn touch(&self, path: &Path) -> Result<(), Error> {
        let (bucket, prefix) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = prefix.ok_or_else(|| Error::custom("no key specified"))?;
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
                    Err(sdk_err(e))
                }
            }
        }
    }

    async fn get_property_sheet(&self, path: &Path) -> Result<PropertySheet, Error> {
        self.build_property_sheet(path).await
    }

    async fn apply_properties(&self, path: &Path, patch: &PropertyPatch) -> Result<(), Error> {
        self.apply_property_patch(path, patch).await
    }

    async fn create_directory(&self, path: &Path) -> Result<(), Error> {
        let (bucket, prefix) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = prefix.ok_or_else(|| Error::custom("no key specified"))?;
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
            .map_err(sdk_err)?;
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
    /// Set once the upload was completed or aborted — the Drop guard only
    /// fires for writers discarded mid-stream (cancelled/failed operations),
    /// which would otherwise leak the multipart upload.
    terminated: bool,
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
            .map_err(sdk_err)?;

        self.completed_parts.push(
            aws_sdk_s3::types::CompletedPart::builder()
                .part_number(self.part_number)
                .e_tag(resp.e_tag().unwrap_or_default())
                .build(),
        );
        self.part_number += 1;

        Ok(())
    }

    async fn abort(&mut self) {
        warn!(
            "s3: aborting multipart upload upload_id={} bucket={} key={}",
            self.upload_id, self.bucket, self.key
        );
        self.terminated = true;
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

impl Drop for S3AsyncWriter {
    fn drop(&mut self) {
        if self.terminated {
            return;
        }
        warn!(
            "s3: writer dropped mid-upload, aborting multipart upload upload_id={} bucket={} key={}",
            self.upload_id, self.bucket, self.key
        );
        let request = self
            .client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(&self.key)
            .upload_id(&self.upload_id);
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
        self.buffer.extend_from_slice(buf);
        if self.buffer.len() >= MULTIPART_CHUNK_SIZE
            && let Err(e) = self.flush_part().await
        {
            self.abort().await;
            return Err(e);
        }
        Ok(buf.len())
    }

    async fn finish(mut self: Box<Self>) -> Result<(), Error> {
        // Always flush remaining data (even if empty) when no parts have been
        // uploaded yet — CompleteMultipartUpload requires at least one part.
        if (self.completed_parts.is_empty() || !self.buffer.is_empty())
            && let Err(e) = self.flush_part_unconditional().await
        {
            self.abort().await;
            return Err(e);
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
            .map_err(sdk_err)?;
        self.terminated = true;

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

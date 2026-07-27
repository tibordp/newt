//! The S3 property sheet: ACL ↔ `PropertyGrant` mapping, user metadata,
//! storage class and system headers — the crate's only `PropertySheet`
//! producer.

use log::debug;

use crate::Error;
use crate::vfs::path::Path;
use crate::vfs::properties::{
    PropertyField, PropertyFieldValue, PropertyGrant, PropertyGrantee, PropertyGroup,
    PropertyPatch, PropertySheet, PropertyValuePatch, text_field,
};

use super::{S3Vfs, sdk_err};

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
pub(super) async fn snapshot_nontrivial_acl(
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
pub(super) async fn restore_acl(
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

impl S3Vfs {
    pub(super) async fn build_property_sheet(&self, path: &Path) -> Result<PropertySheet, Error> {
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

    pub(super) async fn apply_property_patch(
        &self,
        path: &Path,
        patch: &PropertyPatch,
    ) -> Result<(), Error> {
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

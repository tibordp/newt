use super::{S3Vfs, local_err, sdk_err};
use crate::{
    Error,
    vfs::{
        attributes::{Attribute, AttributeKind, ObjectMetadata},
        path::Path,
    },
};

impl S3Vfs {
    pub(super) async fn read_object_attribute(
        &self,
        path: &Path,
        kind: AttributeKind,
    ) -> Result<Option<Attribute>, Error> {
        if !matches!(
            kind,
            AttributeKind::ObjectMetadata | AttributeKind::ObjectTags | AttributeKind::ObjectAccess
        ) {
            return Ok(None);
        }
        let (bucket, key) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = key.ok_or(Error::not_supported())?;
        let client = self.client_for_bucket(&bucket).await?;
        Ok(Some(match kind {
            AttributeKind::ObjectMetadata => {
                let head = client
                    .head_object()
                    .bucket(bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(sdk_err)?;
                Attribute::ObjectMetadata(ObjectMetadata {
                    metadata: head
                        .metadata()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                    content_type: head.content_type().map(str::to_owned),
                    content_encoding: head.content_encoding().map(str::to_owned),
                    content_language: head.content_language().map(str::to_owned),
                    content_disposition: head.content_disposition().map(str::to_owned),
                    cache_control: head.cache_control().map(str::to_owned),
                    expires: head.expires_string().map(str::to_owned),
                })
            }
            AttributeKind::ObjectTags => {
                let tags = client
                    .get_object_tagging()
                    .bucket(bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(sdk_err)?;
                Attribute::ObjectTags(
                    tags.tag_set()
                        .iter()
                        .map(|t| (t.key().to_owned(), t.value().to_owned()))
                        .collect(),
                )
            }
            AttributeKind::ObjectAccess => {
                let acl = client
                    .get_object_acl()
                    .bucket(bucket)
                    .key(key)
                    .send()
                    .await
                    .map_err(sdk_err)?;
                // Owner-only grants refer to the destination owner implicitly.
                if super::properties::is_owner_only_acl(&acl) {
                    return Ok(None);
                }
                let grants = acl
                    .grants()
                    .iter()
                    .map(|grant| {
                        super::properties::grant_from_s3(grant)
                            .ok_or_else(|| Error::custom("unrepresentable S3 ACL grant"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Attribute::ObjectAccess(grants)
            }
            _ => unreachable!(),
        }))
    }

    pub(super) async fn write_object_attribute(
        &self,
        path: &Path,
        property: &Attribute,
    ) -> Result<(), Error> {
        let (bucket, key) = self.parse_path(path);
        let bucket = bucket.ok_or(Error::not_supported())?;
        let key = key.ok_or(Error::not_supported())?;
        let client = self.client_for_bucket(&bucket).await?;
        match property {
            Attribute::ObjectTags(tags) => {
                let tags = tags
                    .iter()
                    .map(|(key, value)| {
                        aws_sdk_s3::types::Tag::builder()
                            .key(key)
                            .value(value)
                            .build()
                            .map_err(local_err)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let tagging = aws_sdk_s3::types::Tagging::builder()
                    .set_tag_set(Some(tags))
                    .build()
                    .map_err(local_err)?;
                client
                    .put_object_tagging()
                    .bucket(bucket)
                    .key(key)
                    .tagging(tagging)
                    .send()
                    .await
                    .map_err(sdk_err)?;
            }
            Attribute::ObjectAccess(grants) => {
                let owner = client
                    .get_object_acl()
                    .bucket(&bucket)
                    .key(&key)
                    .send()
                    .await
                    .map_err(sdk_err)?
                    .owner()
                    .cloned();
                let grants = grants
                    .iter()
                    .map(super::properties::grant_to_s3)
                    .collect::<Result<Vec<_>, _>>()?;
                let policy = aws_sdk_s3::types::AccessControlPolicy::builder()
                    .set_owner(owner)
                    .set_grants(Some(grants))
                    .build();
                client
                    .put_object_acl()
                    .bucket(bucket)
                    .key(key)
                    .access_control_policy(policy)
                    .send()
                    .await
                    .map_err(sdk_err)?;
            }
            _ => return Err(Error::not_supported()),
        }
        Ok(())
    }
}

pub(super) fn expires(
    meta: &ObjectMetadata,
) -> Result<Option<aws_sdk_s3::primitives::DateTime>, Error> {
    meta.expires
        .as_deref()
        .map(|s| {
            aws_sdk_s3::primitives::DateTime::from_str(
                s,
                aws_sdk_s3::primitives::DateTimeFormat::HttpDate,
            )
            .map_err(local_err)
        })
        .transpose()
}

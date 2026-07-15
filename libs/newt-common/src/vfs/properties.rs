//! VFS property sheets: a generic, schema-driven surface for per-VFS
//! extras (S3 ACLs / user metadata, xattrs, …) that the fixed
//! `VfsMetadata` cannot express. See DESIGN_VFS_PROPERTY_SHEETS.md.
//!
//! The same types serve two roles: a VFS emits a concrete per-path sheet
//! (`Option` values always `Some`), and the host folds sheets across a
//! multi-selection into one merged sheet where `None` means "mixed".
//! Everything here crosses both RPC layers, so: plain serializable data,
//! no `std::path`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Sheet
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct PropertySheet {
    pub groups: Vec<PropertyGroup>,
    /// Shown next to the apply button (e.g. S3: applying rewrites objects
    /// in place, which may be slow on large ones).
    pub apply_hint: Option<String>,
}

impl PropertySheet {
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|g| g.fields.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct PropertyGroup {
    pub label: String,
    pub fields: Vec<PropertyField>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct PropertyField {
    /// Stable key (e.g. `s3.meta`) — patch target and i18n/docs anchor.
    pub key: String,
    pub label: String,
    pub value: PropertyFieldValue,
    pub editable: bool,
    /// Accepts a value but has no readable current state (e.g. S3 canned
    /// ACL, which reads back as grants). Rendered without a current value.
    pub write_only: bool,
}

/// Field kind + current value. `None` values mean "mixed across the
/// selection" in a folded sheet (or "no current value" on a write-only
/// field); a VFS itself always emits concrete values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum PropertyFieldValue {
    Text {
        value: Option<String>,
    },
    Choice {
        choices: Vec<String>,
        value: Option<String>,
    },
    /// String map (e.g. `x-amz-meta-*`). A `None` entry value = key
    /// present on only part of the selection, or with differing values.
    Map {
        entries: BTreeMap<String, Option<String>>,
    },
    /// Access-grant list, compared whole across a selection.
    Grants {
        permission_choices: Vec<String>,
        value: Option<Vec<PropertyGrant>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub struct PropertyGrant {
    pub grantee: PropertyGrantee,
    pub permission: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum PropertyGrantee {
    User {
        id: String,
        display_name: Option<String>,
    },
    Group {
        uri: String,
    },
    Email {
        address: String,
    },
}

// ---------------------------------------------------------------------------
// Patch
// ---------------------------------------------------------------------------

/// Carries only the fields the user changed; unmentioned fields are left
/// alone (same philosophy as the permission editor's set/clear masks).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct PropertyPatch {
    pub ops: Vec<PropertyPatchOp>,
}

impl PropertyPatch {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct PropertyPatchOp {
    /// Field key the op targets.
    pub key: String,
    pub op: PropertyValuePatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum PropertyValuePatch {
    /// Text/Choice: set the value.
    Set { value: String },
    /// Map: per-key set/delete applied over each file's existing map.
    /// "Replace the whole map" is expressible as deletes for dropped keys
    /// — merge is the only primitive.
    MapPatch {
        set: BTreeMap<String, String>,
        delete: Vec<String>,
    },
    /// Grants: whole-list replace (no per-grant merge semantics).
    ReplaceGrants { grants: Vec<PropertyGrant> },
}

// ---------------------------------------------------------------------------
// Folding — merge per-path sheets into one multi-selection sheet
// ---------------------------------------------------------------------------

/// Fold per-path sheets into a single sheet for a multi-selection.
/// Structure (groups/fields) is intersected by key across all sheets;
/// per-kind value fold: equal → shown, differing → `None` (indeterminate).
/// Maps fold per key over the union of keys. Grant lists are compared
/// whole. A field is editable only if editable in every sheet.
pub fn fold_sheets(sheets: &[PropertySheet]) -> PropertySheet {
    let Some((first, rest)) = sheets.split_first() else {
        return PropertySheet::default();
    };
    if rest.is_empty() {
        return first.clone();
    }

    let find = |sheet: &PropertySheet, group: &str, key: &str| -> Option<PropertyField> {
        sheet
            .groups
            .iter()
            .find(|g| g.label == group)
            .and_then(|g| g.fields.iter().find(|f| f.key == key))
            .cloned()
    };

    let mut groups = Vec::new();
    for group in &first.groups {
        let mut fields = Vec::new();
        for field in &group.fields {
            let others: Option<Vec<PropertyField>> = rest
                .iter()
                .map(|s| find(s, &group.label, &field.key))
                .collect();
            // Field missing (or of a different kind) in any sheet → drop it.
            let Some(others) = others else { continue };
            if let Some(folded) = fold_field(field, &others) {
                fields.push(folded);
            }
        }
        if !fields.is_empty() {
            groups.push(PropertyGroup {
                label: group.label.clone(),
                fields,
            });
        }
    }

    PropertySheet {
        groups,
        apply_hint: first.apply_hint.clone(),
    }
}

fn fold_field(first: &PropertyField, others: &[PropertyField]) -> Option<PropertyField> {
    let value = match &first.value {
        PropertyFieldValue::Text { value } => {
            let mut folded = value.clone();
            for other in others {
                let PropertyFieldValue::Text { value: v } = &other.value else {
                    return None;
                };
                if *v != folded {
                    folded = None;
                }
            }
            PropertyFieldValue::Text { value: folded }
        }
        PropertyFieldValue::Choice { choices, value } => {
            let mut folded = value.clone();
            for other in others {
                let PropertyFieldValue::Choice { value: v, .. } = &other.value else {
                    return None;
                };
                if *v != folded {
                    folded = None;
                }
            }
            PropertyFieldValue::Choice {
                choices: choices.clone(),
                value: folded,
            }
        }
        PropertyFieldValue::Map { entries } => {
            let mut folded: BTreeMap<String, Option<String>> = entries.clone();
            for other in others {
                let PropertyFieldValue::Map { entries: e } = &other.value else {
                    return None;
                };
                for (k, v) in e {
                    match folded.get(k) {
                        Some(existing) if existing == v => {}
                        Some(_) => {
                            folded.insert(k.clone(), None);
                        }
                        // Key absent so far → present on only part of the
                        // selection → indeterminate.
                        None => {
                            folded.insert(k.clone(), None);
                        }
                    }
                }
                // Keys we hold that this sheet lacks are partial too.
                for k in folded.keys().cloned().collect::<Vec<_>>() {
                    if !e.contains_key(&k) {
                        folded.insert(k, None);
                    }
                }
            }
            PropertyFieldValue::Map { entries: folded }
        }
        PropertyFieldValue::Grants {
            permission_choices,
            value,
        } => {
            let mut folded = value.clone();
            for other in others {
                let PropertyFieldValue::Grants { value: v, .. } = &other.value else {
                    return None;
                };
                if *v != folded {
                    folded = None;
                }
            }
            PropertyFieldValue::Grants {
                permission_choices: permission_choices.clone(),
                value: folded,
            }
        }
    };

    Some(PropertyField {
        key: first.key.clone(),
        label: first.label.clone(),
        value,
        editable: first.editable && others.iter().all(|f| f.editable),
        write_only: first.write_only,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_field(key: &str, value: &str) -> PropertyField {
        PropertyField {
            key: key.to_string(),
            label: key.to_string(),
            value: PropertyFieldValue::Text {
                value: Some(value.to_string()),
            },
            editable: true,
            write_only: false,
        }
    }

    fn map_field(key: &str, entries: &[(&str, &str)]) -> PropertyField {
        PropertyField {
            key: key.to_string(),
            label: key.to_string(),
            value: PropertyFieldValue::Map {
                entries: entries
                    .iter()
                    .map(|(k, v)| (k.to_string(), Some(v.to_string())))
                    .collect(),
            },
            editable: true,
            write_only: false,
        }
    }

    fn sheet(fields: Vec<PropertyField>) -> PropertySheet {
        PropertySheet {
            groups: vec![PropertyGroup {
                label: "G".to_string(),
                fields,
            }],
            apply_hint: None,
        }
    }

    #[test]
    fn fold_equal_and_mixed_text() {
        let folded = fold_sheets(&[
            sheet(vec![text_field("a", "same"), text_field("b", "one")]),
            sheet(vec![text_field("a", "same"), text_field("b", "two")]),
        ]);
        let fields = &folded.groups[0].fields;
        assert_eq!(
            fields[0].value,
            PropertyFieldValue::Text {
                value: Some("same".to_string())
            }
        );
        assert_eq!(fields[1].value, PropertyFieldValue::Text { value: None });
    }

    #[test]
    fn fold_map_per_key() {
        let folded = fold_sheets(&[
            sheet(vec![map_field(
                "m",
                &[("k1", "v"), ("k2", "x"), ("k3", "v")],
            )]),
            sheet(vec![map_field("m", &[("k1", "v"), ("k2", "y")])]),
        ]);
        let PropertyFieldValue::Map { entries } = &folded.groups[0].fields[0].value else {
            panic!("expected map");
        };
        // Same everywhere → value; differing → None; partial → None.
        assert_eq!(entries["k1"], Some("v".to_string()));
        assert_eq!(entries["k2"], None);
        assert_eq!(entries["k3"], None);
    }

    #[test]
    fn fold_drops_fields_missing_in_any_sheet() {
        let folded = fold_sheets(&[
            sheet(vec![text_field("a", "v"), text_field("b", "v")]),
            sheet(vec![text_field("a", "v")]),
        ]);
        let fields = &folded.groups[0].fields;
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].key, "a");
    }

    #[test]
    fn fold_single_sheet_is_identity() {
        let s = sheet(vec![text_field("a", "v")]);
        assert_eq!(fold_sheets(std::slice::from_ref(&s)), s);
    }

    #[test]
    fn fold_editable_requires_all() {
        let mut ro = text_field("a", "v");
        ro.editable = false;
        let folded = fold_sheets(&[sheet(vec![text_field("a", "v")]), sheet(vec![ro])]);
        assert!(!folded.groups[0].fields[0].editable);
    }
}

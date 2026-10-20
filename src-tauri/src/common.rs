use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;

use parking_lot::Mutex;
use tauri::Emitter;
use tauri::WebviewWindow;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Tauri(#[from] tauri::Error),
    #[error("{0}")]
    Open(#[from] opener::OpenError),
    #[error("{0}")]
    Custom(String),
    #[error("operation cancelled")]
    Cancelled,
}

/// `specta::Type` for a state wrapper whose hand-written `Serialize` emits
/// exactly `$wire` (typically a lock guard's contents), so published state
/// types in `bindings.ts` without a derive on the wrapper itself.
macro_rules! specta_as {
    ($wrapper:ty => $wire:ty) => {
        impl specta::Type for $wrapper {
            fn inline(
                types: &mut specta::TypeCollection,
                generics: specta::Generics,
            ) -> specta::DataType {
                <$wire as specta::Type>::inline(types, generics)
            }

            fn reference(
                types: &mut specta::TypeCollection,
                generics: &[specta::DataType],
            ) -> specta::datatype::reference::Reference {
                <$wire as specta::Type>::reference(types, generics)
            }
        }
    };
}
pub(crate) use specta_as;

/// A command result sent as a raw body rather than JSON: the page receives
/// an `ArrayBuffer`, not an array of numbers. The bindings type it
/// `unknown`; `unwrapBytes()` in `src/lib/ipc.ts` turns it into a `Uint8Array`.
pub struct RawBytes(pub Vec<u8>);

impl tauri::ipc::IpcResponse for RawBytes {
    fn body(self) -> tauri::Result<tauri::ipc::InvokeResponseBody> {
        Ok(tauri::ipc::InvokeResponseBody::Raw(self.0))
    }
}

impl specta::Type for RawBytes {
    fn inline(_: &mut specta::TypeCollection, _: specta::Generics) -> specta::DataType {
        specta::DataType::Unknown
    }
}

/// A command's arguments sent alongside bytes, as one raw request body:
/// a little-endian `u32` length, that many bytes of JSON arguments, then
/// the bytes. Sent by `invokeRaw` in `src/lib/ipc.ts`; a command taking
/// one has no parameters in its generated binding.
pub struct RawArgs<T> {
    pub args: T,
    pub data: Vec<u8>,
}

impl<'de, R: tauri::Runtime, T: serde::de::DeserializeOwned> tauri::ipc::CommandArg<'de, R>
    for RawArgs<T>
{
    fn from_command(
        command: tauri::ipc::CommandItem<'de, R>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        let malformed =
            || tauri::ipc::InvokeError::from(format!("{}: malformed raw body", command.name));
        let tauri::ipc::InvokeBody::Raw(body) = command.message.payload() else {
            return Err(malformed());
        };
        let (json, data) = split_raw_args(body).ok_or_else(malformed)?;
        Ok(Self {
            args: serde_json::from_slice(json).map_err(|_| malformed())?,
            data: data.to_vec(),
        })
    }
}

/// A `RawArgs` body's JSON arguments and bytes.
fn split_raw_args(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let (len, rest) = body.split_first_chunk::<4>()?;
    rest.split_at_checked(u32::from_le_bytes(*len) as usize)
}

impl<T> specta::function::FunctionArg for RawArgs<T> {
    fn to_datatype(_: &mut specta::TypeCollection) -> Option<specta::DataType> {
        None
    }
}

impl From<newt_common::Error> for Error {
    fn from(value: newt_common::Error) -> Self {
        match value.kind {
            newt_common::ErrorKind::Cancelled => Error::Cancelled,
            _ => Error::Custom(value.message),
        }
    }
}

impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}

// Error is wire-encoded as a plain string (see Serialize above), so as far as
// the frontend is concerned it's just a string. Tell specta the same.
impl specta::Type for Error {
    fn inline(
        _type_collection: &mut specta::TypeCollection,
        _generics: specta::Generics<'_>,
    ) -> specta::datatype::DataType {
        String::inline(_type_collection, _generics)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum PatchKey {
    Index(usize),
    String(String),
}

/// A patch operation in Immer format (path is an array rather than a
/// /-separated string). Operations apply in order; `Add` and `Remove` at
/// an array index splice, shifting the elements after it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum PatchOperation {
    Add {
        path: Vec<PatchKey>,
        value: serde_json::Value,
    },
    Remove {
        path: Vec<PatchKey>,
    },
    Replace {
        path: Vec<PatchKey>,
        value: serde_json::Value,
    },
}

/// One step of a [`KeyedArray`] path.
pub enum PathSegment {
    Field(&'static str),
    /// Any array index or object key.
    Any,
}

/// An array diffed by element identity rather than position, so elements
/// inserted or removed ahead of the rest patch as single splices instead
/// of rewriting every element after them. An element's identity is the
/// first of `id_fields` holding a string. The array is diffed by position
/// when an element has no identity, two share one, or the elements present
/// on both sides changed order.
pub struct KeyedArray {
    pub path: &'static [PathSegment],
    pub id_fields: &'static [&'static str],
}

/// The patch would exceed its operation budget.
struct OverBudget;

struct Differ<'k> {
    keyed: &'k [KeyedArray],
    remaining: Option<usize>,
    path: Vec<PatchKey>,
    ops: Vec<PatchOperation>,
}

impl Differ<'_> {
    fn emit(&mut self, op: PatchOperation) -> Result<(), OverBudget> {
        if let Some(remaining) = self.remaining.as_mut() {
            if *remaining == 0 {
                return Err(OverBudget);
            }
            *remaining -= 1;
        }
        self.ops.push(op);
        Ok(())
    }

    fn child(&self, key: PatchKey) -> Vec<PatchKey> {
        let mut path = self.path.clone();
        path.push(key);
        path
    }

    fn descend(
        &mut self,
        key: PatchKey,
        old: &serde_json::Value,
        new: &serde_json::Value,
    ) -> Result<(), OverBudget> {
        self.path.push(key);
        self.value(old, new)?;
        self.path.pop();
        Ok(())
    }

    fn value(
        &mut self,
        old: &serde_json::Value,
        new: &serde_json::Value,
    ) -> Result<(), OverBudget> {
        use serde_json::Value;
        match (old, new) {
            (Value::Object(old), Value::Object(new)) => {
                for key in old.keys().filter(|k| !new.contains_key(*k)) {
                    let path = self.child(PatchKey::String(key.clone()));
                    self.emit(PatchOperation::Remove { path })?;
                }
                for (key, new) in new {
                    match old.get(key) {
                        Some(old) => self.descend(PatchKey::String(key.clone()), old, new)?,
                        None => {
                            let path = self.child(PatchKey::String(key.clone()));
                            let value = new.clone();
                            self.emit(PatchOperation::Add { path, value })?;
                        }
                    }
                }
                Ok(())
            }
            (Value::Array(old), Value::Array(new)) => match self.id_fields() {
                Some(id_fields) => self.keyed_array(old, new, id_fields),
                None => self.positional_array(old, new),
            },
            _ if old == new => Ok(()),
            _ => {
                let path = self.path.clone();
                let value = new.clone();
                self.emit(PatchOperation::Replace { path, value })
            }
        }
    }

    fn positional_array(
        &mut self,
        old: &[serde_json::Value],
        new: &[serde_json::Value],
    ) -> Result<(), OverBudget> {
        let common = old.len().min(new.len());
        for i in 0..common {
            self.descend(PatchKey::Index(i), &old[i], &new[i])?;
        }
        for i in (common..old.len()).rev() {
            let path = self.child(PatchKey::Index(i));
            self.emit(PatchOperation::Remove { path })?;
        }
        for (i, value) in new.iter().enumerate().skip(common) {
            let path = self.child(PatchKey::Index(i));
            let value = value.clone();
            self.emit(PatchOperation::Add { path, value })?;
        }
        Ok(())
    }

    fn keyed_array(
        &mut self,
        old: &[serde_json::Value],
        new: &[serde_json::Value],
        id_fields: &[&str],
    ) -> Result<(), OverBudget> {
        let (Some(old_ids), Some(new_ids)) =
            (identities(old, id_fields), identities(new, id_fields))
        else {
            return self.positional_array(old, new);
        };
        let old_index: HashMap<&str, usize> =
            old_ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let new_index: HashMap<&str, usize> =
            new_ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        if !old_ids
            .iter()
            .filter_map(|id| new_index.get(id))
            .is_sorted()
        {
            return self.positional_array(old, new);
        }

        // Removing back to front keeps the earlier indices valid; inserting
        // front to back then leaves every survivor at its new index by the
        // time it is reached.
        for (i, id) in old_ids.iter().enumerate().rev() {
            if !new_index.contains_key(id) {
                let path = self.child(PatchKey::Index(i));
                self.emit(PatchOperation::Remove { path })?;
            }
        }
        for (j, (id, value)) in new_ids.iter().zip(new).enumerate() {
            match old_index.get(id) {
                Some(&i) => self.descend(PatchKey::Index(j), &old[i], value)?,
                None => {
                    let path = self.child(PatchKey::Index(j));
                    let value = value.clone();
                    self.emit(PatchOperation::Add { path, value })?;
                }
            }
        }
        Ok(())
    }

    fn id_fields(&self) -> Option<&'static [&'static str]> {
        self.keyed
            .iter()
            .find(|keyed| {
                keyed.path.len() == self.path.len()
                    && keyed.path.iter().zip(&self.path).all(|(segment, key)| {
                        match (segment, key) {
                            (PathSegment::Any, _) => true,
                            (PathSegment::Field(field), PatchKey::String(key)) => field == key,
                            (PathSegment::Field(_), PatchKey::Index(_)) => false,
                        }
                    })
            })
            .map(|keyed| keyed.id_fields)
    }
}

/// Each element's identity, or `None` if one has none or two share one.
fn identities<'v>(items: &'v [serde_json::Value], id_fields: &[&str]) -> Option<Vec<&'v str>> {
    let mut seen = HashSet::with_capacity(items.len());
    items
        .iter()
        .map(|item| {
            let id = id_fields.iter().find_map(|f| item.get(f)?.as_str())?;
            seen.insert(id).then_some(id)
        })
        .collect()
}

/// The operations that turn `previous` into `serialized`, or `None` if
/// there would be more than `max_ops` of them.
pub fn diff(
    previous: &serde_json::Value,
    serialized: &serde_json::Value,
    keyed: &[KeyedArray],
    max_ops: Option<usize>,
) -> Option<Vec<PatchOperation>> {
    let mut differ = Differ {
        keyed,
        remaining: max_ops,
        path: Vec::new(),
        ops: Vec::new(),
    };
    differ.value(previous, serialized).ok()?;
    Some(differ.ops)
}

const MAX_PATCH_OPS: usize = 100;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdatePayloadKind {
    State(serde_json::Value),
    Patch(Vec<PatchOperation>),
}

#[derive(Clone, serde::Serialize)]
pub struct UpdatePayload {
    pub version: usize,
    #[serde(flatten)]
    pub kind: UpdatePayloadKind,
}

pub struct UpdatePublisher<T> {
    window: WebviewWindow,
    event_name: String,
    base: Mutex<(usize, serde_json::Value)>,
    keyed: &'static [KeyedArray],
    state: T,
    _phantom: PhantomData<T>,
}

impl<T: serde::Serialize> UpdatePublisher<T> {
    pub fn new(window: WebviewWindow, event_name: &str, state: T) -> Self {
        Self {
            event_name: format!("update:{}", event_name),
            window,
            state,
            base: Mutex::new((0, serde_json::Value::Null)),
            keyed: &[],
            _phantom: PhantomData,
        }
    }

    pub fn with_keyed_arrays(mut self, keyed: &'static [KeyedArray]) -> Self {
        self.keyed = keyed;
        self
    }

    pub fn state(&self) -> &T {
        &self.state
    }

    pub fn window(&self) -> &WebviewWindow {
        &self.window
    }

    /// Send what changed since the last publish. The snapshot, the diff and
    /// the emit all happen under `base`: a snapshot taken outside it could
    /// be older than the base it is diffed against, rolling the frontend
    /// back, and an emit outside it could overtake its predecessor.
    pub fn publish(&self) -> Result<(), Error> {
        let mut base = self.base.lock();
        let serialized = serde_json::to_value(&self.state).unwrap();
        let patch = diff(&base.1, &serialized, self.keyed, Some(MAX_PATCH_OPS));
        if patch.as_ref().is_some_and(|p| p.is_empty()) {
            // If there are no changes, don't publish anything and don't increment the version
            return Ok(());
        }
        let version = base.0;
        let kind = match patch {
            Some(patch) => UpdatePayloadKind::Patch(patch),
            None => UpdatePayloadKind::State(serialized.clone()),
        };
        *base = (version + 1, serialized);
        self.window.emit_to(
            self.window.label(),
            &self.event_name,
            UpdatePayload { version, kind },
        )?;
        Ok(())
    }

    pub fn publish_full(&self) -> Result<(), Error> {
        let mut base = self.base.lock();
        let serialized = serde_json::to_value(&self.state).unwrap();
        let version = base.0;
        *base = (version + 1, serialized.clone());
        self.window.emit_to(
            self.window.label(),
            &self.event_name,
            UpdatePayload {
                version,
                kind: UpdatePayloadKind::State(serialized),
            },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn raw_args_split_at_the_length_prefix() {
        let mut body = 7u32.to_le_bytes().to_vec();
        body.extend_from_slice(br#"{"a":1}"#);
        body.extend_from_slice(&[0, 255, 10]);
        assert_eq!(
            split_raw_args(&body),
            Some((&br#"{"a":1}"#[..], &[0u8, 255, 10][..]))
        );
    }

    #[test]
    fn raw_args_reject_a_short_body() {
        assert_eq!(split_raw_args(&[1, 0]), None);
        assert_eq!(split_raw_args(&[9, 0, 0, 0, b'{']), None);
    }

    const ROWS: &[KeyedArray] = &[KeyedArray {
        path: &[PathSegment::Field("rows")],
        id_fields: &["key", "name"],
    }];

    /// Applies `ops` the way Immer's `applyPatches` does.
    fn apply(mut state: Value, ops: &[PatchOperation]) -> Value {
        for op in ops {
            let (path, value) = match op {
                PatchOperation::Add { path, value } | PatchOperation::Replace { path, value } => {
                    (path, Some(value.clone()))
                }
                PatchOperation::Remove { path } => (path, None),
            };
            let Some((last, parent)) = path.split_last() else {
                state = value.unwrap();
                continue;
            };
            let parent = parent.iter().fold(&mut state, |v, key| match key {
                PatchKey::Index(i) => &mut v[*i],
                PatchKey::String(k) => &mut v[k.as_str()],
            });
            match (parent, last, op) {
                (Value::Array(a), PatchKey::Index(i), PatchOperation::Add { .. }) => {
                    a.insert(*i, value.unwrap())
                }
                (Value::Array(a), PatchKey::Index(i), PatchOperation::Remove { .. }) => {
                    a.remove(*i);
                }
                (Value::Array(a), PatchKey::Index(i), _) => a[*i] = value.unwrap(),
                (Value::Object(o), PatchKey::String(k), PatchOperation::Remove { .. }) => {
                    o.remove(k);
                }
                (Value::Object(o), PatchKey::String(k), _) => {
                    o.insert(k.clone(), value.unwrap());
                }
                _ => panic!("patch path does not fit the state"),
            }
        }
        state
    }

    fn round_trip(old: Value, new: Value) -> Vec<PatchOperation> {
        let ops = diff(&old, &new, ROWS, None).unwrap();
        assert_eq!(apply(old, &ops), new, "{ops:?}");
        ops
    }

    fn row(name: &str, size: u64) -> Value {
        json!({ "name": name, "key": null, "size": size })
    }

    fn rows(names: &[&str]) -> Value {
        json!({ "rows": names.iter().map(|n| row(n, 1)).collect::<Vec<_>>() })
    }

    #[test]
    fn rows_sorting_in_above_patch_as_entering_and_leaving_rows() {
        let listing: Vec<Value> = (0..200).map(|i| row(&format!("f{i:03}"), i)).collect();
        let window = |from: usize| json!({ "rows": listing[from..from + 100] });
        // Three entries sorted in above the window: three rows from above
        // it enter at the top and three leave at the bottom.
        let ops = round_trip(window(50), window(47));
        assert_eq!(ops.len(), 6, "{ops:?}");
        assert!(diff(&window(50), &window(47), &[], Some(100)).is_none());
    }

    #[test]
    fn keyed_rows_insert_remove_and_change_in_place() {
        let mut new = rows(&["a", "x", "c", "y", "e"]);
        new["rows"][2]["size"] = json!(7);
        let ops = round_trip(rows(&["a", "b", "c", "d", "e"]), new);
        // Two removals, two insertions, one changed field.
        assert_eq!(ops.len(), 5, "{ops:?}");
    }

    #[test]
    fn keyed_rows_use_key_over_name() {
        let entry = |key: &str, name: &str| json!({ "name": name, "key": key });
        let old = json!({ "rows": [entry("a/x", "x"), entry("b/x", "x")] });
        let new = json!({ "rows": [entry("0/x", "x"), entry("a/x", "x"), entry("b/x", "x")] });
        assert_eq!(round_trip(old, new).len(), 1);
    }

    #[test]
    fn reordered_or_ambiguous_rows_fall_back_to_positions() {
        round_trip(rows(&["a", "b", "c"]), rows(&["c", "b", "a"]));
        round_trip(rows(&["a", "a", "b"]), rows(&["b", "a"]));
        round_trip(
            json!({ "rows": [{ "size": 1 }, row("a", 1)] }),
            json!({ "rows": [row("a", 1)] }),
        );
    }

    #[test]
    fn unkeyed_arrays_and_objects_diff_structurally() {
        let ops = round_trip(
            json!({ "list": [1, 2, 3], "gone": true, "nested": { "a": 1 } }),
            json!({ "list": [1, 5], "added": [], "nested": { "a": 2, "b": null } }),
        );
        assert_eq!(ops.len(), 6, "{ops:?}");
        round_trip(json!({ "list": [1] }), json!({ "list": [1, 2, 3] }));
        round_trip(Value::Null, json!({ "a": 1 }));
    }

    #[test]
    fn over_budget_diffs_are_none() {
        let old = json!({ "list": [1, 2, 3] });
        let new = json!({ "list": [4, 5, 6] });
        assert!(diff(&old, &new, &[], Some(2)).is_none());
        assert_eq!(diff(&old, &new, &[], Some(3)).unwrap().len(), 3);
        assert!(diff(&old, &old, &[], Some(0)).unwrap().is_empty());
    }
}

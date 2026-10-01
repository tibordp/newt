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
    Arboard(#[from] arboard::Error),
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

impl From<treediff::value::Key> for PatchKey {
    fn from(k: treediff::value::Key) -> Self {
        use treediff::value::Key::*;

        match k {
            Index(i) => PatchKey::Index(i),
            String(s) => PatchKey::String(s),
        }
    }
}

/// A patch operation in Immer format (path is an array rather than a /-separated string)
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

#[derive(Default)]
struct PatchDelegate {
    remaining: Option<usize>,
    path: Vec<PatchKey>,
    removals: Vec<PatchOperation>,
    additions: Vec<PatchOperation>,
}

macro_rules! step {
    ($self:expr, true) => {{
        if let Some(remaining) = $self.remaining.as_mut() {
            if *remaining == 0 {
                return;
            }
            *remaining -= 1;
        }
    }};
    ($self:expr) => {{
        if let Some(0) = $self.remaining {
            return;
        }
    }};
}

impl<'a> treediff::Delegate<'a, treediff::value::Key, serde_json::Value> for PatchDelegate {
    fn push(&mut self, k: &treediff::value::Key) {
        step!(self);
        self.path.push(k.clone().into());
    }
    fn pop(&mut self) {
        step!(self);
        self.path.pop();
    }
    fn removed<'b>(&mut self, k: &'b treediff::value::Key, _v: &'a serde_json::Value) {
        step!(self, true);
        self.removals.push(PatchOperation::Remove {
            path: self
                .path
                .iter()
                .cloned()
                .chain(std::iter::once(k.clone().into()))
                .collect(),
        });
    }
    fn added<'b>(&mut self, k: &'b treediff::value::Key, v: &'a serde_json::Value) {
        step!(self, true);
        self.additions.push(PatchOperation::Add {
            path: self
                .path
                .iter()
                .cloned()
                .chain(std::iter::once(k.clone().into()))
                .collect(),
            value: v.clone(),
        });
    }
    fn unchanged(&mut self, _v: &'a serde_json::Value) {}
    fn modified(&mut self, _old: &'a serde_json::Value, new: &'a serde_json::Value) {
        step!(self, true);
        self.additions.push(PatchOperation::Replace {
            path: self.path.clone(),
            value: new.clone(),
        });
    }
}

impl PatchDelegate {
    fn new(max_ops: Option<usize>) -> Self {
        Self {
            remaining: max_ops,
            ..Default::default()
        }
    }

    fn try_into_patch(self) -> Option<Vec<PatchOperation>> {
        if let Some(0) = self.remaining {
            None
        } else {
            Some(
                self.removals
                    .into_iter()
                    .rev()
                    .chain(self.additions)
                    .collect(),
            )
        }
    }
}

pub fn diff(
    previous: &serde_json::Value,
    serialized: &serde_json::Value,
    max_ops: Option<usize>,
) -> Option<Vec<PatchOperation>> {
    let mut delegate = PatchDelegate::new(max_ops);
    treediff::diff(previous, serialized, &mut delegate);
    delegate.try_into_patch()
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
            _phantom: PhantomData,
        }
    }

    pub fn state(&self) -> &T {
        &self.state
    }

    pub fn window(&self) -> &WebviewWindow {
        &self.window
    }

    pub fn publish(&self) -> Result<(), Error> {
        let serialized = serde_json::to_value(&self.state).unwrap();

        let (version, patch) = {
            let mut base = self.base.lock();
            let patch = diff(&base.1, &serialized, Some(MAX_PATCH_OPS));
            if patch.as_ref().is_some_and(|p| p.is_empty()) {
                // If there are no changes, don't publish anything and don't increment the version
                return Ok(());
            }

            let version = base.0;
            *base = (version + 1, serialized.clone());
            (version, patch)
        };

        self.window.emit_to(
            self.window.label(),
            &self.event_name,
            UpdatePayload {
                version,
                kind: patch
                    .map(UpdatePayloadKind::Patch)
                    .unwrap_or(UpdatePayloadKind::State(serialized)),
            },
        )?;

        Ok(())
    }

    pub fn publish_full(&self) -> Result<(), Error> {
        let serialized = serde_json::to_value(&self.state).unwrap();
        let (version, _) = {
            let mut base = self.base.lock();
            let version = base.0 + 1;

            std::mem::replace(&mut *base, (version, serialized.clone()))
        };

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
    use super::split_raw_args;

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
}

use std::time::SystemTime;

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
    PtyProcess(#[from] pty_process::Error),
    #[error("{0}")]
    Custom(String),
}

// we must manually implement serde::Serialize
impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}

pub trait ToUnix {
    fn to_unix(&self) -> i128;
}

impl ToUnix for SystemTime {
    fn to_unix(&self) -> i128 {
        self.duration_since(SystemTime::UNIX_EPOCH)
            .map(|t| t.as_millis() as i128)
            .unwrap_or_else(|e| -(e.duration().as_millis() as i128))
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
                    .chain(self.additions.into_iter())
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

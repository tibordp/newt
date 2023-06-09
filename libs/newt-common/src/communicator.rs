use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
};

use bytes::{BufMut, Bytes};
use parking_lot::Mutex;
use tokio::io::{AsyncBufRead, AsyncRead};

use crate::Error;

#[derive(Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Api(usize);

#[derive(Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RequestId(usize);

#[derive(serde::Serialize, serde::Deserialize)]
pub enum Message {
    Request(Api, RequestId, bytes::Bytes),
    Response(RequestId, bytes::Bytes),
    Cancel(RequestId),
}

struct Communicator {
    id: AtomicUsize,
    response: Mutex<HashMap<RequestId, tokio::sync::oneshot::Sender<bytes::Bytes>>>,
    outbox: tokio::sync::mpsc::UnboundedSender<Message>,
}

pub struct CancelGuard<'a>(&'a Communicator, RequestId);
impl<'a> Drop for CancelGuard<'a> {
    fn drop(&mut self) {
        self.0.response.lock().remove(&self.1);
        let _ = self.0.outbox.send(Message::Cancel(self.1));
    }
}

impl Communicator {
    pub async fn process_read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<(), Error> {
        use tokio::io::AsyncReadExt;

        loop {
            match reader.read_u8().await? {
                0 => return Ok(()),
                1 => {
                    let api = Api(reader.read_u64().await? as usize);
                    let request_id = RequestId(reader.read_u64().await? as usize);
                    let len = reader.read_u64().await? as usize;
                    let buf = Vec::with_capacity(len);
                    reader.read_buf(buf.spare_capacity_mut())
                }
            }
        }

        Ok(())
    }

    pub async fn invoke<Req, Resp>(&self, api: Api, req: &Req) -> Result<Resp, Error>
    where
        Req: serde::Serialize + for<'de> serde::Deserialize<'de>,
        Resp: serde::Serialize + for<'de> serde::Deserialize<'de>,
    {
        let id = RequestId(self.id.fetch_add(0, Ordering::SeqCst));
        let mut bytes: Vec<u8> = Vec::new();
        ciborium::into_writer(req, &mut bytes).unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.response.lock().insert(id, tx);
        let guard = CancelGuard(self, id);

        self.outbox
            .send(Message::Request(api, id, bytes.into()))
            .map_err(|_| Error::Custom("could not send".into()))?;

        let resp = rx
            .await
            .map_err(|_| Error::Custom("could not receive".into()))?;

        std::mem::forget(guard);
        Ok(ciborium::from_reader(std::io::Cursor::new(resp)).unwrap())
    }
}

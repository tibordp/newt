use bytes::{Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use super::{Api, Message, MessageCodec, RequestId, StreamId};

fn codec() -> MessageCodec {
    MessageCodec {}
}

fn round_trip(msg: Message) -> Message {
    let mut c = codec();
    let mut buf = BytesMut::new();
    c.encode(msg, &mut buf).unwrap();
    c.decode(&mut buf).unwrap().unwrap()
}

#[test]
fn decode_handles_torn_reads_at_every_byte_boundary() {
    // Regression: the Notify/Signal arms sliced the length field past a
    // partial header, panicking when a read ended mid-header.
    let messages = [
        Message::InvokeRequest(Api(42), RequestId(1), Bytes::from_static(b"payload")),
        Message::InvokeResponse(RequestId(2), Bytes::from_static(b"payload")),
        Message::InvokeCancel(RequestId(3)),
        Message::Notify(Api(4), Bytes::from_static(b"payload")),
        Message::Signal(Api(5), Bytes::from_static(b"payload")),
        Message::StreamData(StreamId(6), Bytes::from_static(b"payload")),
        Message::StreamEnd(StreamId(7)),
        Message::StreamReset(StreamId(8), crate::Error::custom("reset")),
        Message::StreamCredit(StreamId(9), 16),
        Message::StreamCancel(StreamId(10)),
    ];
    for msg in messages {
        let mut c = codec();
        let expected = std::mem::discriminant(&msg);
        let mut full = BytesMut::new();
        c.encode(msg, &mut full).unwrap();

        let mut buf = BytesMut::new();
        let mut decoded = None;
        for (i, byte) in full.iter().enumerate() {
            buf.extend_from_slice(&[*byte]);
            match c.decode(&mut buf).unwrap() {
                Some(m) => {
                    assert_eq!(i, full.len() - 1, "decoded before the frame was complete");
                    decoded = Some(m);
                }
                None => assert_ne!(i, full.len() - 1, "complete frame failed to decode"),
            }
        }
        assert_eq!(std::mem::discriminant(&decoded.unwrap()), expected);
    }
}

#[test]
fn invoke_request_round_trip() {
    let payload = Bytes::from_static(b"hello world");
    let decoded = round_trip(Message::InvokeRequest(
        Api(42),
        RequestId(12345),
        payload.clone(),
    ));
    match decoded {
        Message::InvokeRequest(api, id, data) => {
            assert_eq!(api, Api(42));
            assert_eq!(id, RequestId(12345));
            assert_eq!(data, payload);
        }
        other => panic!("expected InvokeRequest, got {:?}", other),
    }
}

#[test]
fn invoke_response_round_trip() {
    let payload = Bytes::from_static(b"response data");
    let decoded = round_trip(Message::InvokeResponse(RequestId(99), payload.clone()));
    match decoded {
        Message::InvokeResponse(id, data) => {
            assert_eq!(id, RequestId(99));
            assert_eq!(data, payload);
        }
        other => panic!("expected InvokeResponse, got {:?}", other),
    }
}

#[test]
fn invoke_cancel_round_trip() {
    let decoded = round_trip(Message::InvokeCancel(RequestId(7)));
    match decoded {
        Message::InvokeCancel(id) => assert_eq!(id, RequestId(7)),
        other => panic!("expected InvokeCancel, got {:?}", other),
    }
}

#[test]
fn notify_round_trip() {
    let payload = Bytes::from_static(b"notification");
    let decoded = round_trip(Message::Notify(Api(100), payload.clone()));
    match decoded {
        Message::Notify(api, data) => {
            assert_eq!(api, Api(100));
            assert_eq!(data, payload);
        }
        other => panic!("expected Notify, got {:?}", other),
    }
}

#[test]
fn signal_round_trip() {
    let payload = Bytes::from_static(b"keystroke");
    let decoded = round_trip(Message::Signal(Api(103), payload.clone()));
    match decoded {
        Message::Signal(api, data) => {
            assert_eq!(api, Api(103));
            assert_eq!(data, payload);
        }
        other => panic!("expected Signal, got {:?}", other),
    }
}

#[test]
fn empty_payload_invoke_request() {
    let decoded = round_trip(Message::InvokeRequest(Api(0), RequestId(0), Bytes::new()));
    match decoded {
        Message::InvokeRequest(api, id, data) => {
            assert_eq!(api, Api(0));
            assert_eq!(id, RequestId(0));
            assert!(data.is_empty());
        }
        other => panic!("expected InvokeRequest, got {:?}", other),
    }
}

#[test]
fn empty_payload_invoke_response() {
    let decoded = round_trip(Message::InvokeResponse(RequestId(0), Bytes::new()));
    match decoded {
        Message::InvokeResponse(id, data) => {
            assert_eq!(id, RequestId(0));
            assert!(data.is_empty());
        }
        other => panic!("expected InvokeResponse, got {:?}", other),
    }
}

#[test]
fn empty_payload_notify() {
    let decoded = round_trip(Message::Notify(Api(0), Bytes::new()));
    match decoded {
        Message::Notify(api, data) => {
            assert_eq!(api, Api(0));
            assert!(data.is_empty());
        }
        other => panic!("expected Notify, got {:?}", other),
    }
}

#[test]
fn large_payload() {
    let payload = Bytes::from(vec![0xAB; 100_000]);
    let decoded = round_trip(Message::InvokeResponse(RequestId(1), payload.clone()));
    match decoded {
        Message::InvokeResponse(_, data) => {
            assert_eq!(data.len(), 100_000);
            assert_eq!(data, payload);
        }
        other => panic!("expected InvokeResponse, got {:?}", other),
    }
}

#[test]
fn max_api_and_request_id() {
    let decoded = round_trip(Message::InvokeRequest(
        Api(u16::MAX),
        RequestId(u64::MAX),
        Bytes::from_static(b"x"),
    ));
    match decoded {
        Message::InvokeRequest(api, id, _) => {
            assert_eq!(api, Api(u16::MAX));
            assert_eq!(id, RequestId(u64::MAX));
        }
        other => panic!("expected InvokeRequest, got {:?}", other),
    }
}

// --- Partial buffer / incremental decode tests ---

#[test]
fn partial_buffer_returns_none() {
    let mut c = codec();
    let mut buf = BytesMut::new();

    // Encode a full message
    c.encode(Message::InvokeCancel(RequestId(1)), &mut buf)
        .unwrap();

    // Feed only one byte at a time — should return None until complete
    let full = buf.split();
    let mut partial = BytesMut::new();

    for i in 0..full.len() - 1 {
        partial.extend_from_slice(&full[i..i + 1]);
        assert!(
            c.decode(&mut partial).unwrap().is_none(),
            "should return None with {} of {} bytes",
            i + 1,
            full.len()
        );
    }

    // Feed the last byte — should decode
    partial.extend_from_slice(&full[full.len() - 1..]);
    assert!(c.decode(&mut partial).unwrap().is_some());
}

#[test]
fn partial_invoke_request_returns_none() {
    let mut c = codec();
    let mut buf = BytesMut::new();
    c.encode(
        Message::InvokeRequest(Api(1), RequestId(2), Bytes::from_static(b"data")),
        &mut buf,
    )
    .unwrap();

    // Feed header without payload
    let full = buf.split();
    let mut partial = BytesMut::new();
    partial.extend_from_slice(&full[..15]); // header is 15 bytes
    assert!(c.decode(&mut partial).unwrap().is_none());

    // Feed remaining
    partial.extend_from_slice(&full[15..]);
    assert!(c.decode(&mut partial).unwrap().is_some());
}

#[test]
fn multiple_messages_in_one_buffer() {
    let mut c = codec();
    let mut buf = BytesMut::new();
    c.encode(Message::StreamEnd(StreamId(3)), &mut buf).unwrap();
    c.encode(Message::StreamCancel(StreamId(4)), &mut buf)
        .unwrap();
    c.encode(Message::InvokeCancel(RequestId(5)), &mut buf)
        .unwrap();

    let msg1 = c.decode(&mut buf).unwrap().unwrap();
    assert!(matches!(msg1, Message::StreamEnd(StreamId(3))));

    let msg2 = c.decode(&mut buf).unwrap().unwrap();
    assert!(matches!(msg2, Message::StreamCancel(StreamId(4))));

    let msg3 = c.decode(&mut buf).unwrap().unwrap();
    assert!(matches!(msg3, Message::InvokeCancel(RequestId(5))));

    assert!(c.decode(&mut buf).unwrap().is_none());
}

#[test]
fn empty_buffer_returns_none() {
    let mut c = codec();
    let mut buf = BytesMut::new();
    assert!(c.decode(&mut buf).unwrap().is_none());
}

#[test]
fn invalid_message_type_returns_error() {
    let mut c = codec();
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&[255]); // invalid tag
    assert!(c.decode(&mut buf).is_err());
}

// --- Priority classification tests ---

#[test]
fn message_priority_classification() {
    assert!(Message::InvokeRequest(Api(0), RequestId(0), Bytes::new()).is_high_priority());
    assert!(Message::InvokeCancel(RequestId(0)).is_high_priority());
    assert!(!Message::InvokeResponse(RequestId(0), Bytes::new()).is_high_priority());
    assert!(!Message::Notify(Api(0), Bytes::new()).is_high_priority());
    assert!(Message::Signal(Api(0), Bytes::new()).is_high_priority());
    assert!(!Message::StreamData(StreamId(0), Bytes::new()).is_high_priority());
    assert!(!Message::StreamEnd(StreamId(0)).is_high_priority());
    assert!(Message::StreamReset(StreamId(0), crate::Error::cancelled()).is_high_priority());
    assert!(Message::StreamCredit(StreamId(0), 1).is_high_priority());
    assert!(Message::StreamCancel(StreamId(0)).is_high_priority());
}

#[test]
fn stream_frames_round_trip() {
    match round_trip(Message::StreamData(
        StreamId(u64::MAX),
        Bytes::from_static(b"abc"),
    )) {
        Message::StreamData(id, data) => {
            assert_eq!(id, StreamId(u64::MAX));
            assert_eq!(&data[..], b"abc");
        }
        other => panic!("expected StreamData, got {:?}", other),
    }
    match round_trip(Message::StreamReset(
        StreamId(3),
        crate::Error::not_found("gone"),
    )) {
        Message::StreamReset(id, error) => {
            assert_eq!(id, StreamId(3));
            assert_eq!(error.kind, crate::ErrorKind::NotFound);
            assert_eq!(error.message, "gone");
        }
        other => panic!("expected StreamReset, got {:?}", other),
    }
    assert!(matches!(
        round_trip(Message::StreamCredit(StreamId(4), 7)),
        Message::StreamCredit(StreamId(4), 7)
    ));
}

// --- Outbox priority tests ---

#[tokio::test]
async fn outbox_high_priority_preferred() {
    let (tx, mut rx) = super::create_outbox();

    // Send low-priority first, then high-priority
    tx.send(Message::Notify(Api(1), Bytes::new()))
        .await
        .unwrap();
    tx.send(Message::InvokeCancel(RequestId(7))).await.unwrap();

    // High-priority should come first due to biased select
    let msg = rx.recv().await.unwrap();
    assert!(matches!(msg, Message::InvokeCancel(RequestId(7))));

    let msg = rx.recv().await.unwrap();
    assert!(matches!(msg, Message::Notify(Api(1), _)));
}

#[tokio::test]
async fn outbox_try_recv_prefers_high() {
    let (tx, mut rx) = super::create_outbox();

    // Queue both before receiving.
    tx.send_high(Message::InvokeCancel(RequestId(8))).unwrap();
    tx.send(Message::Notify(Api(2), Bytes::new()))
        .await
        .unwrap();

    // try_recv should return high first
    let msg = rx.try_recv().unwrap();
    assert!(matches!(msg, Message::InvokeCancel(RequestId(8))));

    let msg = rx.try_recv().unwrap();
    assert!(matches!(msg, Message::Notify(Api(2), _)));
}

#[test]
#[should_panic(expected = "send_high called with low-priority message")]
fn send_high_panics_for_low_priority() {
    let (tx, _rx) = super::create_outbox();
    tx.send_high(Message::Notify(Api(0), Bytes::new())).unwrap();
}

#[tokio::test]
async fn shutdown_closes_the_connection_for_the_peer() {
    use super::Communicator;
    let (ours, theirs) = tokio::io::duplex(1024);
    let peer = Communicator::new(theirs);
    let communicator = Communicator::new(ours);
    // A clone kept alive must not keep the connection open.
    let _kept = communicator.clone();
    communicator.shutdown();
    peer.closed().await;
}

/// Panics in every handler, the way a programming error would.
struct Panicking;

#[async_trait::async_trait]
impl super::Dispatcher for Panicking {
    async fn invoke(&self, _api: Api, _req: Bytes) -> Option<Bytes> {
        panic!("invoke handler bug");
    }
    fn notify(&self, _api: Api, _req: Bytes) -> bool {
        panic!("notify handler bug");
    }
}

#[tokio::test]
async fn a_panicking_invoke_handler_fails_the_call_instead_of_hanging() {
    use super::Communicator;
    let (ours, theirs) = tokio::io::duplex(1024);
    let _peer = Communicator::with_dispatcher(theirs, |_| Panicking);
    let communicator = Communicator::new(ours);
    let result: Result<(), crate::Error> = communicator.invoke(Api(1), &()).await;
    assert_eq!(result.unwrap_err().kind, crate::ErrorKind::Connection);
}

#[tokio::test]
async fn a_panicking_notify_handler_closes_the_connection() {
    use super::Communicator;
    let (ours, theirs) = tokio::io::duplex(1024);
    let _peer = Communicator::with_dispatcher(theirs, |_| Panicking);
    let communicator = Communicator::new(ours);
    communicator.outbox().notify(Api(1), &()).await.unwrap();
    communicator.closed().await;
}

#[tokio::test]
async fn an_api_nothing_serves_fails_the_call_instead_of_hanging() {
    use super::Communicator;
    let (ours, theirs) = tokio::io::duplex(1024);
    let _peer = Communicator::new(theirs);
    let communicator = Communicator::new(ours);
    let result: Result<(), crate::Error> = communicator.invoke(Api(1), &()).await;
    assert_eq!(result.unwrap_err().kind, crate::ErrorKind::Connection);
}

/// Reads never complete; every write fails.
struct BrokenWrites;

impl tokio::io::AsyncRead for BrokenWrites {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Pending
    }
}

impl tokio::io::AsyncWrite for BrokenWrites {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn a_failed_write_fails_the_call_instead_of_waiting_on_the_read_side() {
    use super::Communicator;
    let communicator = Communicator::new(BrokenWrites);
    let result: Result<(), crate::Error> = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        communicator.invoke(Api(1), &()),
    )
    .await
    .expect("the call waited on a connection whose writes had failed");
    assert_eq!(result.unwrap_err().kind, crate::ErrorKind::Connection);
    communicator.closed().await;
}

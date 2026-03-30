use bytes::{Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use super::{Api, Message, MessageCodec, RequestId};

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
fn ping_false_round_trip() {
    let decoded = round_trip(Message::Ping(false));
    assert!(matches!(decoded, Message::Ping(false)));
}

#[test]
fn ping_true_round_trip() {
    let decoded = round_trip(Message::Ping(true));
    assert!(matches!(decoded, Message::Ping(true)));
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
    c.encode(Message::Ping(false), &mut buf).unwrap();

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
    c.encode(Message::Ping(false), &mut buf).unwrap();
    c.encode(Message::Ping(true), &mut buf).unwrap();
    c.encode(Message::InvokeCancel(RequestId(5)), &mut buf)
        .unwrap();

    let msg1 = c.decode(&mut buf).unwrap().unwrap();
    assert!(matches!(msg1, Message::Ping(false)));

    let msg2 = c.decode(&mut buf).unwrap().unwrap();
    assert!(matches!(msg2, Message::Ping(true)));

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
    assert!(Message::Ping(false).is_high_priority());
    assert!(Message::Ping(true).is_high_priority());
    assert!(Message::InvokeRequest(Api(0), RequestId(0), Bytes::new()).is_high_priority());
    assert!(Message::InvokeCancel(RequestId(0)).is_high_priority());
    assert!(!Message::InvokeResponse(RequestId(0), Bytes::new()).is_high_priority());
    assert!(!Message::Notify(Api(0), Bytes::new()).is_high_priority());
}

// --- Outbox priority tests ---

#[tokio::test]
async fn outbox_high_priority_preferred() {
    let (tx, mut rx) = super::create_outbox();

    // Send low-priority first, then high-priority
    tx.send(Message::Notify(Api(1), Bytes::new()))
        .await
        .unwrap();
    tx.send(Message::Ping(false)).await.unwrap();

    // High-priority should come first due to biased select
    let msg = rx.recv().await.unwrap();
    assert!(matches!(msg, Message::Ping(false)));

    let msg = rx.recv().await.unwrap();
    assert!(matches!(msg, Message::Notify(Api(1), _)));
}

#[test]
fn outbox_try_recv_prefers_high() {
    let (tx, mut rx) = super::create_outbox();

    // Send both synchronously
    tx.send_high(Message::Ping(true)).unwrap();
    tx.blocking_send_low(Message::Notify(Api(2), Bytes::new()))
        .unwrap();

    // try_recv should return high first
    let msg = rx.try_recv().unwrap();
    assert!(matches!(msg, Message::Ping(true)));

    let msg = rx.try_recv().unwrap();
    assert!(matches!(msg, Message::Notify(Api(2), _)));
}

#[test]
#[should_panic(expected = "send_high called with low-priority message")]
fn send_high_panics_for_low_priority() {
    let (tx, _rx) = super::create_outbox();
    tx.send_high(Message::Notify(Api(0), Bytes::new())).unwrap();
}

#[test]
#[should_panic(expected = "blocking_send_low called with high-priority message")]
fn blocking_send_low_panics_for_high_priority() {
    let (tx, _rx) = super::create_outbox();
    tx.blocking_send_low(Message::Ping(false)).unwrap();
}

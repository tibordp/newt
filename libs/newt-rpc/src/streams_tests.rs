use std::time::Duration;

use bytes::Bytes;
use futures::FutureExt;
use tokio::io::AsyncReadExt;

use super::*;
use crate::{Api, Communicator, Dispatcher, create_outbox};
use newt_common::ErrorKind;

/// Two ends of one connection.
fn pair() -> (Communicator, Communicator) {
    let (a, b) = tokio::io::duplex(64 * 1024);
    (Communicator::new(a), Communicator::new(b))
}

async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("timed out")
}

#[tokio::test]
async fn items_arrive_in_order_across_many_windows() {
    let (reader_side, writer_side) = pair();
    let (id, mut rx) = reader_side.open_stream();
    let mut tx = writer_side.stream_sender(id);

    let count = 10 * WINDOW;
    let writer = tokio::spawn(async move {
        for i in 0..count {
            tx.send_item(&i).await.unwrap();
        }
        tx.finish().await.unwrap();
    });
    for i in 0..count {
        assert_eq!(within(rx.recv_item::<u32>()).await.unwrap().unwrap(), i);
    }
    assert!(within(rx.recv()).await.is_none());
    writer.await.unwrap();
}

#[tokio::test]
async fn a_writer_waits_for_the_reader_to_take_frames() {
    let (reader_side, writer_side) = pair();
    let (id, mut rx) = reader_side.open_stream();
    let mut tx = writer_side.stream_sender(id);

    for _ in 0..WINDOW {
        tx.send(Bytes::from_static(b"x")).await.unwrap();
    }
    let mut blocked = Box::pin(tx.send(Bytes::from_static(b"x")));
    assert!((&mut blocked).now_or_never().is_none());

    for _ in 0..WINDOW / 2 {
        within(rx.recv()).await.unwrap().unwrap();
    }
    within(blocked).await.unwrap();
}

struct Echo;

#[async_trait::async_trait]
impl Dispatcher for Echo {
    async fn invoke(&self, _api: Api, req: Bytes) -> Option<Bytes> {
        Some(req)
    }
}

#[tokio::test]
async fn a_reader_that_stops_taking_frames_stalls_only_its_stream() {
    let (a, b) = tokio::io::duplex(64 * 1024);
    let reader_side = Communicator::new(a);
    let writer_side = Communicator::with_dispatcher(b, |_| Echo);
    let (id, _rx) = reader_side.open_stream();
    let mut tx = writer_side.stream_sender(id);
    for _ in 0..WINDOW {
        tx.send(Bytes::from(vec![0; CHUNK_SIZE])).await.unwrap();
    }

    let echoed: u32 = within(reader_side.invoke(Api(1), &7u32)).await.unwrap();
    assert_eq!(echoed, 7);
}

#[tokio::test]
async fn dropping_the_receiver_cancels_the_writer() {
    let (reader_side, writer_side) = pair();
    let (id, rx) = reader_side.open_stream();
    let mut tx = writer_side.stream_sender(id);

    drop(rx);
    within(tx.cancelled()).await;
    let e = tx.send(Bytes::from_static(b"x")).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
}

#[tokio::test]
async fn dropping_the_writer_fails_the_stream() {
    let (reader_side, writer_side) = pair();
    let (id, mut rx) = reader_side.open_stream();
    let mut tx = writer_side.stream_sender(id);
    tx.send(Bytes::from_static(b"x")).await.unwrap();
    drop(tx);

    // The reset may overtake the data, which it discards.
    loop {
        match within(rx.recv()).await.unwrap() {
            Ok(frame) => assert_eq!(&frame[..], b"x"),
            Err(e) => {
                assert_eq!(e.kind, ErrorKind::Cancelled);
                break;
            }
        }
    }
    assert!(within(rx.recv()).await.is_none());
}

#[tokio::test]
async fn a_reset_carries_its_error() {
    let (reader_side, writer_side) = pair();
    let (id, mut rx) = reader_side.open_stream();
    writer_side
        .stream_sender(id)
        .reset(Error::not_found("no such file"));

    let e = within(rx.recv()).await.unwrap().unwrap_err();
    assert_eq!(e.kind, ErrorKind::NotFound);
    assert_eq!(e.message, "no such file");
}

#[tokio::test]
async fn a_byte_stream_keeps_the_error_whole() {
    let (reader_side, writer_side) = pair();
    let (id, rx) = reader_side.open_stream();
    writer_side
        .stream_sender(id)
        .reset(Error::not_found("no such file"));

    let mut out = Vec::new();
    let e = within(rx.into_async_read().read_to_end(&mut out))
        .await
        .unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    let e = Error::from(e);
    assert_eq!(e.kind, ErrorKind::NotFound);
    assert_eq!(e.message, "no such file");
}

#[tokio::test]
async fn the_connection_ending_fails_both_ends() {
    let (reader_side, writer_side) = pair();
    let (id, mut rx) = reader_side.open_stream();
    let (other_id, _other_rx) = writer_side.open_stream();
    let mut tx = reader_side.stream_sender(other_id);

    reader_side.shutdown();
    let e = within(rx.recv()).await.unwrap().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Connection);
    within(tx.cancelled()).await;
    let e = tx.send(Bytes::from_static(b"x")).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Connection);

    // Streams opened afterwards fail straight away.
    let (_, mut late) = reader_side.open_stream();
    let e = within(late.recv()).await.unwrap().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Connection);
    let _ = id;
}

#[tokio::test]
async fn pump_copies_a_reader_and_ends_the_stream() {
    let (reader_side, writer_side) = pair();
    let (id, rx) = reader_side.open_stream();
    let content: Vec<u8> = (0..3 * CHUNK_SIZE as u32 + 5).map(|i| i as u8).collect();

    let source = content.clone();
    let tx = writer_side.stream_sender(id);
    let pump = tokio::spawn(async move { pump(&mut &source[..], tx).await });

    let mut out = Vec::new();
    within(rx.into_async_read().read_to_end(&mut out))
        .await
        .unwrap();
    assert_eq!(out, content);
    within(pump).await.unwrap().unwrap();
}

#[tokio::test]
async fn pump_stops_when_the_receiver_is_dropped() {
    let (reader_side, writer_side) = pair();
    let (id, mut rx) = reader_side.open_stream();
    let tx = writer_side.stream_sender(id);
    let pump = tokio::spawn(async move { pump(&mut tokio::io::repeat(0), tx).await });

    within(rx.recv()).await.unwrap().unwrap();
    drop(rx);
    let e = within(pump).await.unwrap().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
}

#[tokio::test]
async fn forward_sends_what_the_work_produces_then_its_result() {
    let (reader_side, writer_side) = pair();
    let (id, rx) = reader_side.open_stream();
    let tx = writer_side.stream_sender(id);

    let (items_tx, items_rx) = mpsc::channel(1);
    let work = async move {
        for i in 0..3 * WINDOW {
            items_tx.send(i).await.unwrap();
        }
        "done"
    };
    let forwarded = tokio::spawn(tx.forward(items_rx, work));

    let (out_tx, mut out_rx) = mpsc::channel(4 * WINDOW as usize);
    within(rx.forward_items::<u32>(&out_tx)).await.unwrap();
    assert_eq!(within(forwarded).await.unwrap().unwrap(), "done");
    for i in 0..3 * WINDOW {
        assert_eq!(out_rx.try_recv().unwrap(), i);
    }
    assert!(out_rx.try_recv().is_err());
}

#[test]
#[should_panic(expected = "never opened")]
fn a_frame_for_a_stream_never_opened_panics() {
    let (outbox, _inbox) = create_outbox();
    outbox.streams.on_data(StreamId(0), Bytes::new());
}

#[test]
#[should_panic(expected = "overran its window")]
fn a_frame_beyond_the_window_panics() {
    let (outbox, _inbox) = create_outbox();
    let (id, _rx) = outbox.open_stream();
    for _ in 0..=WINDOW {
        outbox.streams.on_data(id, Bytes::new());
    }
}

#[test]
fn late_frames_for_a_cancelled_stream_are_dropped() {
    let (outbox, _inbox) = create_outbox();
    let (id, rx) = outbox.open_stream();
    drop(rx);
    outbox.streams.on_data(id, Bytes::new());
    outbox.streams.on_end(id, Ok(()));
}

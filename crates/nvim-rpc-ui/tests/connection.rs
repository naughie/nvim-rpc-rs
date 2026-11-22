use nvim_rpc::mp::encode::{Primitive, TokioWrite, WriterSyncExt as _};
use nvim_rpc::read::TokioRead;
use nvim_rpc::{Neovim, Receiver, api, rpc::Message};

use nvim_rpc_ui::{ConnectCheckpointError, Event, GridId, NoHandler, connect_with_checkpoint};
use nvim_rpc_ui::{ConnectionOptions, connect as connect_with_handler};

use tokio::io::{AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, duplex, split};
use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

use std::pin::Pin;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use nvim_rpc_plugin::ConnectionError;

fn connect<R, W>(
    reader: R,
    writer: W,
    options: ConnectionOptions,
) -> Result<nvim_rpc_ui::Connection<W>, nvim_rpc_ui::Error>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    connect_with_handler(reader, writer, nvim_rpc_ui::NoHandler, options)
}

#[tokio::test]
async fn reconnect_restores_the_last_flushed_checkpoint() {
    async fn send(peer: &mut (impl AsyncWrite + Unpin), method: &str, params: impl Primitive) {
        let mut buf = Vec::new();
        buf.write_primitive((2u8, method, params)).consume();
        peer.write_all(&buf).await.unwrap();
        peer.flush().await.unwrap();
    }

    timeout(Duration::from_secs(2), async {
        let (local, mut peer) = duplex(65536);
        let (reader, writer) = split(local);
        let options = ConnectionOptions::default();
        let mut connection = connect(reader, writer, options).unwrap();

        send(&mut peer, "nvim_error_event", (0, "before flush")).await;
        connection.frames.recv().await.unwrap().unwrap();
        assert!(connection.frames.checkpoint().is_none());

        send(
            &mut peer,
            "redraw",
            (
                ("grid_resize", (4, 2, 1)),
                ("grid_line", (4, 0, 0, (("A", 0), ("B",)), false)),
                ("bell", ()),
                ("flush", ()),
                ("grid_clear", (4,)),
            ),
        )
        .await;
        let frame = connection.frames.recv().await.unwrap().unwrap();
        assert!(matches!(frame.events.as_slice(), [Event::Bell]));

        send(&mut peer, "nvim_error_event", (0, "after grid clear")).await;
        let api_error = connection.frames.recv().await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&frame.state, &api_error.state));
        connection.close_and_wait().await.unwrap();
        drop(peer);

        let checkpoint = connection.frames.checkpoint().unwrap();
        assert!(ptr::eq(checkpoint.state(), frame.state.as_ref()));
        drop(connection);

        let (local, _peer) = duplex(65536);
        let (reader, writer) = split(local);
        let mut limited = options;
        limited.limits.max_cells = 1;
        assert!(matches!(
            connect_with_checkpoint(reader, writer, NoHandler, limited, checkpoint.clone()),
            Err(ConnectCheckpointError::Checkpoint(_))
        ));

        let (local, mut peer) = duplex(65536);
        let (reader, writer) = split(local);
        let mut connection =
            connect_with_checkpoint(reader, writer, NoHandler, options, checkpoint).unwrap();
        assert!(connection.frames.checkpoint().is_some());

        send(&mut peer, "nvim_error_event", (0, "after restore")).await;
        let first = connection.frames.recv().await.unwrap().unwrap();
        assert_eq!(first.sequence, 1);
        assert!(first.damage.full);
        assert!(matches!(first.events.as_slice(), [Event::RpcError { .. }]));
        assert_eq!(
            &*first.state.grids[&GridId(4)].cell(0, 0).unwrap().text,
            "A"
        );

        send(
            &mut peer,
            "redraw",
            (
                ("grid_cursor_goto", (4, 0, 1)),
                ("grid_line", (4, 0, 1, (("C", 0),), false)),
                ("flush", ()),
            ),
        )
        .await;
        let frame = connection.frames.recv().await.unwrap().unwrap();
        assert!(!frame.damage.full);
        assert!(frame.damage.cursor);
        assert!(frame.events.is_empty());
        assert_eq!(frame.state.cursor.unwrap().grid, GridId(4));
        let checkpoint = connection.frames.checkpoint().unwrap();
        assert_eq!(
            &*checkpoint.state().grids[&GridId(4)]
                .cell(0, 1)
                .unwrap()
                .text,
            "C"
        );
        connection.close_and_wait().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_partial_write_poisoning_wakes_other_writer_waiters() {
    struct PartialWriter {
        wrote: Arc<AtomicBool>,
        entered: Arc<Notify>,
    }
    impl AsyncWrite for PartialWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if !self.wrote.swap(true, Ordering::SeqCst) {
                self.entered.notify_one();
                Poll::Ready(Ok(bytes.len().min(1)))
            } else {
                Poll::Pending
            }
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    let (reader, _keep_open) = duplex(64);
    let entered = Arc::new(Notify::new());
    let mut connection = connect(
        reader,
        PartialWriter {
            wrote: Arc::new(AtomicBool::new(false)),
            entered: entered.clone(),
        },
        ConnectionOptions::default(),
    )
    .unwrap();
    let client = connection.client.clone();
    let first =
        tokio::spawn(async move { client.nvim().request::<api::NvimEval, i64>(("1",)).await });
    timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let client = connection.client.clone();
    let second =
        tokio::spawn(async move { client.nvim().request::<api::NvimEval, i64>(("2",)).await });
    first.abort();
    first.await.unwrap_err();
    assert!(
        timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(
        connection
            .client
            .nvim()
            .request::<api::NvimEval, i64>(("3",))
            .await
            .is_err()
    );
    connection.close();
}

#[tokio::test]
async fn eof_wakes_pending_requests_and_drains_frames_while_presentation_is_idle() {
    let (local, remote) = duplex(65536);
    let (r, w) = split(local);
    let mut connection = connect(r, w, ConnectionOptions::default()).unwrap();
    let (r, w) = split(remote);
    let peer = tokio::spawn(async move {
        let mut sender = Neovim::new(TokioWrite::new(w));
        sender
            .send_async(
                nvim_rpc::mp::encode::rpc::Notification {
                    method: "redraw",
                    params: (("grid_resize", (1, 2, 2)), ("flush", (), (), ())),
                }
                .to_primitive(),
            )
            .await
            .unwrap();
        sender.flush_async().await.unwrap();
        let mut receiver = Receiver::new(TokioRead(r), vec![0; 65536]);
        let Message::Request(_) = receiver.recv_async().await.unwrap() else {
            panic!()
        };
        // Drop both halves without replying. A full GUI queue must not delay EOF.
    });
    assert!(
        timeout(
            Duration::from_secs(2),
            connection
                .client
                .nvim()
                .request::<api::NvimEval, i64>(("1",))
        )
        .await
        .unwrap()
        .is_err()
    );
    peer.await.unwrap();
    timeout(Duration::from_secs(2), connection.wait())
        .await
        .unwrap()
        .unwrap();
    let final_frame = connection.frames.recv().await.unwrap().unwrap();
    assert_eq!(final_frame.sequence, 3);
    assert!(final_frame.damage.full);
    assert!(connection.frames.recv().await.is_none());
}

#[tokio::test]
async fn malformed_rpc_envelope_wakes_waiters_and_reports_failure() {
    let (local, remote) = duplex(65536);
    let (r, w) = split(local);
    let mut connection = connect(r, w, ConnectionOptions::default()).unwrap();
    let (r, mut w) = split(remote);
    let peer = tokio::spawn(async move {
        let mut receiver = Receiver::new(TokioRead(r), vec![0; 65536]);
        receiver.recv_async().await.unwrap();
        // A complete MessagePack object with an invalid RPC method type.
        let mut bytes = Vec::new();
        bytes.write_primitive((2u8, 42u8, ())).consume();
        w.write_all(&bytes).await.unwrap();
        w.flush().await.unwrap();
    });
    assert!(
        timeout(
            Duration::from_secs(2),
            connection
                .client
                .nvim()
                .request::<api::NvimEval, i64>(("1",))
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(matches!(
        timeout(Duration::from_secs(2), connection.wait())
            .await
            .unwrap(),
        Err(ConnectionError::InvalidFormat)
    ));
    assert!(connection.frames.recv().await.is_none());
    peer.await.unwrap();
}

#[tokio::test]
async fn asynchronous_neovim_errors_are_events_and_leave_rpc_usable() {
    let (local, remote) = duplex(65536);
    let (r, w) = split(local);
    let mut connection = connect(r, w, ConnectionOptions::default()).unwrap();
    let (r, w) = split(remote);
    let peer = tokio::spawn(async move {
        let mut sender = Neovim::new(TokioWrite::new(w));
        sender
            .send_async(
                nvim_rpc::mp::encode::rpc::Notification {
                    method: "nvim_error_event",
                    params: (0, "invalid command"),
                }
                .to_primitive(),
            )
            .await
            .unwrap();
        sender.flush_async().await.unwrap();
        let mut receiver = Receiver::new(TokioRead(r), vec![0; 65536]);
        let Message::Request(request) = receiver.recv_async().await.unwrap() else {
            panic!()
        };
        sender
            .send_async(
                nvim_rpc::mp::encode::rpc::Response {
                    msgid: request.msgid,
                    result: Ok::<_, &str>(42),
                }
                .to_primitive(),
            )
            .await
            .unwrap();
        sender.flush_async().await.unwrap();
        receiver.recv_async().await.ok();
    });
    let frame = timeout(Duration::from_secs(2), connection.frames.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        matches!(&frame.events[0], Event::RpcError {kind:0,message} if &**message == "invalid command")
    );
    assert_eq!(
        timeout(
            Duration::from_secs(2),
            connection
                .client
                .nvim()
                .request::<api::NvimEval, i64>(("42",))
        )
        .await
        .unwrap()
        .unwrap(),
        42
    );
    connection.close();
    peer.abort();
}

#[tokio::test]
async fn unseen_grid_destruction_keeps_frames_and_rpc_responses_available() {
    timeout(Duration::from_secs(2), async {
        let (local, remote) = duplex(65536);
        let (reader, writer) = split(local);
        let mut connection = connect(reader, writer, ConnectionOptions::default()).unwrap();
        let (reader, writer) = split(remote);

        let peer = tokio::spawn(async move {
            let mut receiver = Receiver::new(TokioRead(reader), vec![0; 65536]);
            let mut sender = Neovim::new(TokioWrite::new(writer));

            for result in [42, 43] {
                let Message::Request(request) = receiver.recv_async().await.unwrap() else {
                    panic!("expected evaluation request");
                };
                assert_eq!(request.method.as_slice(), b"nvim_eval");

                if result == 42 {
                    sender
                        .send_async(
                            nvim_rpc::mp::encode::rpc::Notification {
                                method: "redraw",
                                params: (
                                    ("grid_resize", (1, 2, 2)),
                                    ("flush", ()),
                                    ("win_close", (4,)),
                                    ("grid_destroy", (4,)),
                                    ("set_title", ("after destroy",)),
                                    ("flush", ()),
                                ),
                            }
                            .to_primitive(),
                        )
                        .await
                        .unwrap();
                } else {
                    sender
                        .send_async(
                            nvim_rpc::mp::encode::rpc::Notification {
                                method: "redraw",
                                params: (
                                    ("grid_line", (1, 0, 0, (("x", 0),), false)),
                                    ("bell", ()),
                                    ("flush", ()),
                                ),
                            }
                            .to_primitive(),
                        )
                        .await
                        .unwrap();
                }

                sender
                    .send_async(
                        nvim_rpc::mp::encode::rpc::Response {
                            msgid: request.msgid,
                            result: Ok::<_, &str>(result),
                        }
                        .to_primitive(),
                    )
                    .await
                    .unwrap();
                sender.flush_async().await.unwrap();
            }

            receiver.recv_async().await.ok();
        });

        for (expression, expected) in [("6 * 7", 42), ("40 + 3", 43)] {
            let result = connection
                .client
                .nvim()
                .request::<api::NvimEval, i64>((expression,))
                .await
                .unwrap();
            assert_eq!(result, expected);

            let frame = connection.frames.recv().await.unwrap().unwrap();

            if expected == 42 {
                assert_eq!(frame.sequence, 2);
                assert!(matches!(
                    frame.events.as_slice(),
                    [Event::SetTitle(title)] if &**title == "after destroy"
                ));
            } else {
                assert_eq!(frame.sequence, 3);
                assert!(matches!(frame.events.as_slice(), [Event::Bell]));
                assert_eq!(
                    frame.state.grids[&GridId(1)]
                        .cell(0, 0)
                        .unwrap()
                        .text
                        .as_ref(),
                    "x"
                );
            }
        }

        connection.close_and_wait().await.unwrap();
        peer.await.unwrap();
    })
    .await
    .expect("unseen grid destruction interrupted the connection");
}

#[tokio::test]
async fn buffered_incoming_requests_are_rejected_in_reverse_order() {
    let (local, mut remote) = duplex(65536);
    let (r, w) = split(local);
    let mut connection = connect(r, w, ConnectionOptions::default()).unwrap();
    let mut bytes = Vec::new();
    for id in [41u32, 42, 43] {
        bytes.write_primitive((0u8, id, "x", ())).consume();
    }
    remote.write_all(&bytes).await.unwrap();
    remote.flush().await.unwrap();
    let mut responses = Receiver::new(TokioRead(remote), vec![0; 65536]);
    for expected in [43u32, 42, 41] {
        let Message::Response(response) = timeout(Duration::from_secs(2), responses.recv_async())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(response.msgid.0, expected);
        assert!(response.result.is_err());
    }
    connection.close();
}

#[tokio::test]
async fn nested_requests_arriving_while_writer_is_blocked_reply_in_reverse_order() {
    let (local, remote) = duplex(64);
    let (r, w) = split(local);
    let mut connection = connect(r, w, ConnectionOptions::default()).unwrap();
    let (r, mut w) = split(remote);
    let client = connection.client.clone();
    let outgoing = tokio::spawn(async move {
        client
            .nvim()
            .notify::<api::NvimSetCurrentLine>((&"a".repeat(1024),))
            .await
    });
    // The outgoing notification fills the pipe and keeps the writer locked.
    tokio::task::yield_now().await;
    for id in [41u32, 42] {
        let mut bytes = Vec::new();
        bytes.write_primitive((0u8, id, "x", ())).consume();
        bytes
            .write_primitive((2u8, "redraw", (("flush", ()),)))
            .consume();
        w.write_all(&bytes).await.unwrap();
        timeout(Duration::from_secs(2), connection.frames.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    let mut receiver = Receiver::new(TokioRead(r), vec![0; 4096]);
    assert!(matches!(
        timeout(Duration::from_secs(2), receiver.recv_async())
            .await
            .unwrap()
            .unwrap(),
        Message::Notification(_)
    ));
    for expected in [42u32, 41] {
        let Message::Response(response) = timeout(Duration::from_secs(2), receiver.recv_async())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected method-error response")
        };
        assert_eq!(response.msgid.0, expected);
        assert!(response.result.is_err());
    }
    outgoing.await.unwrap().unwrap();
    connection.close();
}

#[tokio::test]
async fn coalescing_preserves_event_order_and_marks_full_damage() {
    let (local, remote) = duplex(65536);
    let (r, w) = split(local);
    let mut connection = connect(r, w, ConnectionOptions::default()).unwrap();
    let (r, mut w) = split(remote);
    let (ready, processed) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut bytes = Vec::new();
        bytes
            .write_primitive((
                2u8,
                "redraw",
                (
                    ("set_title", ("first",)),
                    ("flush", ()),
                    ("set_title", ("second",)),
                    ("flush", ()),
                    ("set_title", ("third",)),
                    ("flush", ()),
                ),
            ))
            .consume();
        // This request dispatches after the preceding UI notification finishes.
        bytes.write_primitive((0u8, 99u32, "barrier", ())).consume();
        w.write_all(&bytes).await.unwrap();
        let mut receiver = Receiver::new(TokioRead(r), vec![0; 65536]);
        let Message::Response(response) = receiver.recv_async().await.unwrap() else {
            panic!("expected application method-error response");
        };
        assert_eq!(response.msgid.0, 99);
        assert!(response.result.is_err());
        ready.send(()).unwrap();
        receiver.recv_async().await.ok();
    });
    timeout(Duration::from_secs(2), processed)
        .await
        .unwrap()
        .unwrap();
    let frame = connection.frames.recv().await.unwrap().unwrap();
    assert_eq!(frame.sequence, 3);
    assert!(frame.damage.full);
    let titles = frame
        .events
        .iter()
        .filter_map(|event| match event {
            Event::SetTitle(title) => Some(title.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(titles, ["first", "second", "third"]);
    assert_eq!(&*frame.state.title, "third");
    assert!(
        timeout(Duration::from_millis(10), connection.frames.recv())
            .await
            .is_err()
    );
    connection.close_and_wait().await.unwrap();
    peer.abort();
}

#[tokio::test]
async fn incremental_coalescing_preserves_burst_damage_and_event_order() {
    use nvim_rpc_ui::{Coalescing, GridDamage};

    timeout(Duration::from_secs(2), async {
        let (local, remote) = duplex(65536);
        let (r, w) = split(local);
        let mut connection = connect(r, w, ConnectionOptions::default()).unwrap();
        connection.frames.set_coalescing(Coalescing::Incremental);
        let (r, mut w) = split(remote);
        let mut bytes = Vec::new();
        bytes
            .write_primitive((2u8, "redraw", (("grid_resize", (1, 8, 4)), ("flush", ()))))
            .consume();
        w.write_all(&bytes).await.unwrap();
        let initial = connection.frames.recv().await.unwrap().unwrap();
        assert_eq!(initial.sequence, 1);

        bytes.clear();
        bytes
            .write_primitive((
                2u8,
                "redraw",
                (
                    ("grid_cursor_goto", (1, 0, 1)),
                    ("flush", ()),
                    ("grid_line", (1, 1, 2, (("x", 0),), false)),
                    ("bell", ()),
                    ("flush", ()),
                    ("grid_line", (1, 3, 6, (("y", 0),), false)),
                    ("grid_cursor_goto", (1, 3, 7)),
                    ("visual_bell", ()),
                    ("flush", ()),
                ),
            ))
            .consume();
        // The response proves all three flushes reached the pending slot before
        // receiving it, independently of RPC task scheduling.
        bytes.write_primitive((0u8, 99u32, "barrier", ())).consume();
        w.write_all(&bytes).await.unwrap();
        let mut receiver = Receiver::new(TokioRead(r), vec![0; 65536]);
        let Message::Response(response) = receiver.recv_async().await.unwrap() else {
            panic!("expected application method-error response");
        };
        assert_eq!(response.msgid.0, 99);
        assert!(response.result.is_err());

        let latest = connection.frames.recv().await.unwrap().unwrap();
        assert_eq!(latest.sequence, 4);
        assert!(!latest.damage.full);
        assert!(!latest.damage.layout);
        assert!(latest.damage.cursor);
        assert!(!latest.damage.widgets);
        assert_eq!(latest.damage.removed_grids, []);
        assert_eq!(
            latest.damage.grids,
            [GridDamage {
                grid: GridId(1),
                top: 1,
                bottom: 4,
                left: 2,
                right: 7,
            }]
        );
        assert!(matches!(
            latest.events.as_slice(),
            [Event::Bell, Event::VisualBell]
        ));
        let cursor = latest.state.cursor.unwrap();
        assert_eq!((cursor.grid, cursor.row, cursor.col), (GridId(1), 3, 7));
        let grid = &latest.state.grids[&GridId(1)];
        assert_eq!(grid.cell(1, 2).unwrap().text.as_ref(), "x");
        assert_eq!(grid.cell(3, 6).unwrap().text.as_ref(), "y");
        assert_eq!(
            initial.state.grids[&GridId(1)]
                .cell(1, 2)
                .unwrap()
                .text
                .as_ref(),
            " "
        );
        connection.close_and_wait().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn coalescing_overflow_delivers_pending_events_before_the_ui_error() {
    timeout(Duration::from_secs(2), async {
        let (local, mut remote) = duplex(65536);
        let (r, w) = split(local);
        let mut connection = connect(
            r,
            w,
            ConnectionOptions {
                limits: nvim_rpc_ui::Limits {
                    max_pending_events: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        let mut bytes = Vec::new();
        bytes
            .write_primitive((
                2u8,
                "redraw",
                (
                    ("bell", ()),
                    ("flush", ()),
                    ("visual_bell", ()),
                    ("flush", ()),
                    ("bell", ()),
                    ("flush", ()),
                ),
            ))
            .consume();
        remote.write_all(&bytes).await.unwrap();
        // UI processing closes RPC on overflow before the frame is consumed.
        assert_eq!(remote.read(&mut [0]).await.unwrap(), 0);
        let pending = connection.frames.recv().await.unwrap().unwrap();
        assert_eq!(pending.sequence, 1);
        assert!(matches!(pending.events.as_slice(), [Event::Bell]));
        assert!(matches!(
            connection.frames.recv().await.unwrap(),
            Err(nvim_rpc_ui::FrameError::Protocol(
                nvim_rpc_ui::ProtocolError::Limit("pending events")
            ))
        ));
        assert!(connection.frames.recv().await.is_none());
        assert!(matches!(
            connection.wait().await,
            Err(ConnectionError::Closed)
        ));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn responses_progress_while_presentation_is_idle() {
    let (local, remote) = duplex(65536);
    let (r, w) = split(local);
    let mut connection = connect(r, w, ConnectionOptions::default()).unwrap();
    let (r, mut w) = split(remote);
    let (ready, processed) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut bytes = Vec::new();
        for _ in 0..6 {
            bytes
                .write_primitive((2u8, "redraw", (("grid_resize", (1, 8, 4)), ("flush", ()))))
                .consume();
        }
        bytes.write_primitive((0u8, 99u32, "barrier", ())).consume();
        w.write_all(&bytes).await.unwrap();
        let mut receiver = Receiver::new(TokioRead(r), vec![0; 65536]);
        for _ in 0..2 {
            match receiver.recv_async().await.unwrap() {
                Message::Request(request) => {
                    let mut bytes = Vec::new();
                    bytes
                        .write_primitive((
                            1u8,
                            request.msgid.0,
                            nvim_rpc::mp::encode::primitive::Nil,
                            42i64,
                        ))
                        .consume();
                    w.write_all(&bytes).await.unwrap();
                }
                Message::Response(response) => {
                    assert_eq!(response.msgid.0, 99);
                    assert!(response.result.is_err());
                }
                _ => panic!("unexpected message"),
            }
        }
        ready.send(()).unwrap();
        receiver.recv_async().await.ok();
    });
    let result = timeout(
        Duration::from_secs(2),
        connection
            .client
            .nvim()
            .request::<api::NvimEval, i64>(("6 * 7",)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result, 42);
    timeout(Duration::from_secs(2), processed)
        .await
        .unwrap()
        .unwrap();
    let frame = connection.frames.recv().await.unwrap().unwrap();
    assert_eq!(frame.sequence, 6);
    assert!(frame.damage.full);
    assert_eq!(frame.state.grids[&GridId(1)].width, 8);
    connection.close_and_wait().await.unwrap();
    peer.abort();
}

#[tokio::test]
async fn cancelled_waiter_consumes_late_reply_and_connection_remains_usable() {
    let (local, remote) = duplex(65536);
    let (r, w) = split(local);
    let mut connection = connect(r, w, ConnectionOptions::default()).unwrap();
    let (r, w) = split(remote);
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut receiver = Receiver::new(TokioRead(r), vec![0; 65536]);
        let mut sender = Neovim::new(TokioWrite::new(w));
        let Message::Request(request) = receiver.recv_async().await.unwrap() else {
            panic!()
        };
        let id = request.msgid;
        seen_tx.send(()).unwrap();
        reply_rx.await.unwrap();
        sender
            .send_async(
                nvim_rpc::mp::encode::rpc::Response {
                    msgid: id,
                    result: Ok::<_, &str>(1i64),
                }
                .to_primitive(),
            )
            .await
            .unwrap();
        sender.flush_async().await.unwrap();
        let Message::Request(request) = receiver.recv_async().await.unwrap() else {
            panic!()
        };
        sender
            .send_async(
                nvim_rpc::mp::encode::rpc::Response {
                    msgid: request.msgid,
                    result: Ok::<_, &str>(2i64),
                }
                .to_primitive(),
            )
            .await
            .unwrap();
        sender.flush_async().await.unwrap();
        receiver.recv_async().await.ok();
    });
    let client = connection.client.clone();
    let waiting =
        tokio::spawn(async move { client.nvim().request::<api::NvimEval, i64>(("1",)).await });
    seen_rx.await.unwrap();
    waiting.abort();
    let _ = waiting.await;
    reply_tx.send(()).unwrap();
    assert_eq!(
        timeout(
            Duration::from_secs(2),
            connection
                .client
                .nvim()
                .request::<api::NvimEval, i64>(("2",))
        )
        .await
        .unwrap()
        .unwrap(),
        2
    );
    connection.close();
    peer.abort();
}

#[tokio::test]
async fn truncated_and_oversized_frames_report_errors() {
    for (bytes, max_rpc_bytes) in [
        (&b"\xda\x00"[..], 65536),
        (&b"\xdb\xff\xff\xff\xffxxxx"[..], 8),
    ] {
        let (local, mut remote) = duplex(65536);
        let (r, w) = split(local);
        let mut connection = connect(
            r,
            w,
            ConnectionOptions {
                rpc: nvim_rpc_plugin::ConnectionOptions {
                    max_rpc_bytes,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        remote.write_all(bytes).await.unwrap();
        remote.shutdown().await.unwrap();
        let error = timeout(Duration::from_secs(2), connection.wait())
            .await
            .unwrap()
            .unwrap_err();
        match bytes {
            b"\xda\x00" => assert!(matches!(error, ConnectionError::Eof { buffered: 2 })),
            _ => assert!(matches!(error, ConnectionError::Limit("RPC frame bytes"))),
        }
        assert!(connection.frames.recv().await.is_none());
    }
}

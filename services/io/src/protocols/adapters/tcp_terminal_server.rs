//! Shared accept-loop plumbing for terminal dial-in TCP server adapters.
//!
//! GB/T 32960 and JT/T 808 both listen for connections that vehicles open
//! themselves, so both face the same three hazards: an accept loop that dies
//! without telling the channel supervisor, a peer that opens connections or
//! holds them silent without bound, and a rebind that collides with the
//! channel's own still-listening socket. The plumbing lives here so the two
//! servers cannot drift apart on any of them.

use std::future::Future;
use std::io::ErrorKind;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use bytes::BytesMut;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::protocols::core::diagnostics::AtomicDiagnostics;
use crate::protocols::core::error::{GatewayError, Result};
use crate::protocols::core::traits::{ConnectionState, DataEvent, DataEventSender};

/// Concurrent terminal connections one server channel will hold open.
pub(super) const MAX_CONNECTIONS: usize = 64;

/// How long a terminal that has proven its identity may stay silent.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a terminal that has not yet proven its identity may stay silent.
const UNIDENTIFIED_TIMEOUT: Duration = Duration::from_secs(15);

/// Read bounds applied to a single terminal connection.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReadDeadlines {
    unidentified: Duration,
    idle: Duration,
}

impl ReadDeadlines {
    /// Bounds used by a running channel.
    pub(super) const DEFAULT: Self = Self {
        unidentified: UNIDENTIFIED_TIMEOUT,
        idle: IDLE_TIMEOUT,
    };

    #[cfg(test)]
    pub(super) const fn new(unidentified: Duration, idle: Duration) -> Self {
        Self { unidentified, idle }
    }

    /// An unproven peer gets the shorter bound: its connection is the only one
    /// an attacker can open without holding a provisioned credential.
    const fn for_connection(self, identified: bool) -> Duration {
        if identified {
            self.idle
        } else {
            self.unidentified
        }
    }
}

/// What the accept loop needs in order to report its own death.
pub(super) struct ServerContext {
    pub(super) state: Arc<AtomicU8>,
    pub(super) event_tx: DataEventSender,
    pub(super) diagnostics: Arc<AtomicDiagnostics>,
    pub(super) max_connections: usize,
}

/// Accepts terminal connections until cancelled or until accept fails fatally.
///
/// `handle` owns everything one connection needs; the loop only bounds how
/// many of them may run at once.
pub(super) async fn run_accept_loop<F, Fut>(
    listener: TcpListener,
    context: ServerContext,
    cancellation: CancellationToken,
    handle: F,
) where
    F: Fn(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let slots = Arc::new(Semaphore::new(context.max_connections));
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => {
                    // Refusing at the door is what keeps one hostile peer from
                    // spending the whole process's descriptors and task budget.
                    let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
                        context
                            .diagnostics
                            .record_error("terminal refused: connection limit reached");
                        continue;
                    };
                    let connection = handle(stream);
                    tokio::spawn(async move {
                        connection.await;
                        drop(slot);
                    });
                },
                Err(error) if !accept_error_is_fatal(error.kind()) => {
                    // A peer that resets between SYN and accept must not take
                    // the listener down for every other terminal in the fleet.
                    context.diagnostics.record_error(error.to_string());
                },
                Err(error) => {
                    report_server_failure(&context, &error.to_string());
                    return;
                },
            },
        }
    }
}

/// Classifies an `accept` failure as fatal to the listener or local to a peer.
const fn accept_error_is_fatal(kind: ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::Interrupted
    )
}

/// Publishes the accept loop's death so the channel supervisor can rebuild it.
fn report_server_failure(context: &ServerContext, error: &str) {
    context.diagnostics.record_error(error.to_owned());
    let _ = context
        .event_tx
        .try_send(DataEvent::Error(error.to_owned()));
    // The supervisor decides to reconnect from `connection_state()`. A dead
    // accept loop that left the state on Connected produced a channel that
    // reported healthy forever and never accepted another terminal again.
    context
        .state
        .store(ConnectionState::Error.into(), Ordering::SeqCst);
    let _ = context
        .event_tx
        .try_send(DataEvent::ConnectionChanged(ConnectionState::Error));
}

/// Why a bounded read stopped.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReadOutcome {
    /// The peer delivered bytes into the buffer.
    Bytes,
    /// The peer closed, or the channel is shutting down.
    Closed,
    /// The peer held the connection open past its deadline.
    TimedOut,
}

/// Reads into `buffer` under the deadline this connection has earned.
pub(super) async fn read_bounded(
    stream: &mut TcpStream,
    buffer: &mut BytesMut,
    deadlines: ReadDeadlines,
    identified: bool,
    cancellation: &CancellationToken,
) -> Result<ReadOutcome> {
    tokio::select! {
        _ = cancellation.cancelled() => Ok(ReadOutcome::Closed),
        read = tokio::time::timeout(
            deadlines.for_connection(identified),
            stream.read_buf(buffer),
        ) => match read {
            Ok(Ok(0)) => Ok(ReadOutcome::Closed),
            Ok(Ok(_)) => Ok(ReadOutcome::Bytes),
            Ok(Err(error)) => Err(GatewayError::Io(error)),
            Err(_) => Ok(ReadOutcome::TimedOut),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::core::traits::{DataEventReceiver, data_event_channel};
    use tokio::io::AsyncWriteExt;

    fn server_context(max_connections: usize) -> (ServerContext, DataEventReceiver) {
        let (event_tx, event_rx) = data_event_channel();
        (
            ServerContext {
                state: Arc::new(AtomicU8::new(ConnectionState::Connected.into())),
                event_tx,
                diagnostics: Arc::new(AtomicDiagnostics::new()),
                max_connections,
            },
            event_rx,
        )
    }

    #[test]
    fn a_peer_that_dies_before_accept_does_not_kill_the_listener() {
        // ECONNABORTED is produced by a remote peer resetting between SYN and
        // accept, so treating it as fatal would hand any host on the network a
        // one-packet shutdown of the whole channel.
        assert!(!accept_error_is_fatal(ErrorKind::ConnectionAborted));
        assert!(!accept_error_is_fatal(ErrorKind::ConnectionReset));
        assert!(!accept_error_is_fatal(ErrorKind::ConnectionRefused));
        assert!(!accept_error_is_fatal(ErrorKind::Interrupted));
        assert!(accept_error_is_fatal(ErrorKind::Other));
        assert!(accept_error_is_fatal(ErrorKind::InvalidInput));
    }

    #[tokio::test]
    async fn a_fatal_accept_failure_leaves_a_state_the_supervisor_will_reconnect() {
        let (context, mut event_rx) = server_context(MAX_CONNECTIONS);
        let state = Arc::clone(&context.state);

        report_server_failure(&context, "listener is gone");

        assert_eq!(
            ConnectionState::from(state.load(Ordering::SeqCst)),
            ConnectionState::Error,
            "a state that stays Connected is never rebuilt"
        );
        assert!(!ConnectionState::from(state.load(Ordering::SeqCst)).is_connected());
        assert_eq!(context.diagnostics.error_count(), 1);
        assert!(matches!(event_rx.recv().await, Some(DataEvent::Error(_))));
        assert!(matches!(
            event_rx.recv().await,
            Some(DataEvent::ConnectionChanged(ConnectionState::Error))
        ));
    }

    #[tokio::test]
    async fn the_accept_loop_refuses_peers_past_the_connection_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let (context, _event_rx) = server_context(2);
        let diagnostics = Arc::clone(&context.diagnostics);
        let cancellation = CancellationToken::new();
        let loop_cancellation = cancellation.clone();
        let server = tokio::spawn(run_accept_loop(
            listener,
            context,
            loop_cancellation,
            move |mut stream| async move {
                // Hold the slot for as long as the peer holds the connection.
                let mut scratch = [0_u8; 1];
                while stream.read(&mut scratch).await.unwrap_or(0) > 0 {}
            },
        ));

        let mut held = Vec::new();
        for _ in 0..2 {
            let mut client = TcpStream::connect(address).await.expect("client");
            client.write_all(b"x").await.expect("write");
            held.push(client);
        }
        // Both slots are taken only once both handlers have been spawned; the
        // refusal counter is the observable proof the third never got one.
        let mut refused = TcpStream::connect(address).await.expect("third client");
        let mut scratch = [0_u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(5), refused.read(&mut scratch))
            .await
            .expect("refused peer must be closed, not parked");
        assert_eq!(read.expect("read"), 0, "refused peer must see EOF");
        assert!(
            diagnostics
                .last_error()
                .is_some_and(|error| error.contains("connection limit")),
            "the refusal must be visible to an operator"
        );

        cancellation.cancel();
        server.await.expect("server task");
        drop(held);
    }

    #[tokio::test]
    async fn a_silent_peer_is_dropped_at_its_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let deadlines = ReadDeadlines::new(Duration::from_millis(50), Duration::from_secs(600));
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buffer = BytesMut::new();
            read_bounded(
                &mut stream,
                &mut buffer,
                deadlines,
                false,
                &CancellationToken::new(),
            )
            .await
        });

        let _client = TcpStream::connect(address).await.expect("client");
        let outcome = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("deadline must fire without the peer doing anything")
            .expect("server task")
            .expect("read");
        assert_eq!(outcome, ReadOutcome::TimedOut);
    }

    #[tokio::test]
    async fn an_identified_peer_keeps_the_longer_deadline() {
        let deadlines = ReadDeadlines::new(Duration::from_millis(50), Duration::from_secs(600));
        assert_eq!(
            deadlines.for_connection(false),
            Duration::from_millis(50),
            "an unproven peer must not hold a slot on the identified bound"
        );
        assert_eq!(deadlines.for_connection(true), Duration::from_secs(600));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buffer = BytesMut::new();
            read_bounded(
                &mut stream,
                &mut buffer,
                deadlines,
                true,
                &CancellationToken::new(),
            )
            .await
        });

        let mut client = TcpStream::connect(address).await.expect("client");
        tokio::time::sleep(Duration::from_millis(150)).await;
        client.write_all(b"late").await.expect("write");
        let outcome = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("identified peer must survive the unidentified bound")
            .expect("server task")
            .expect("read");
        assert_eq!(outcome, ReadOutcome::Bytes);
    }

    #[tokio::test]
    async fn a_closed_peer_reports_a_clean_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buffer = BytesMut::new();
            read_bounded(
                &mut stream,
                &mut buffer,
                ReadDeadlines::DEFAULT,
                true,
                &CancellationToken::new(),
            )
            .await
        });

        let client = TcpStream::connect(address).await.expect("client");
        drop(client);
        let outcome = server.await.expect("server task").expect("read");
        assert_eq!(outcome, ReadOutcome::Closed);
    }
}

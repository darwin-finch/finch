//! Task list writes over the remote WebSocket transport (`watch_named_brain`).
//!
//! These drive the real socket handler: a tungstenite client connected to the
//! production remote-Brain router, sending the same binary command envelopes
//! `RemoteBrainClient` sends.

use super::handler_tests::connect_test_brain_socket;
use super::*;
use crate::brain::{
    AttachmentRole, BrainEventKind, BrainRemoteCommand, BrainRemoteCommandKind,
    BrainRemoteEnvelope, BrainRemoteMutation, BrainRemoteReply, BrainTask, BrainTaskPriority,
    BrainTaskStatus,
};
use crate::server::BrainLifecycleService;
use futures::{SinkExt, StreamExt};

type TestSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn socket_task(id: &str) -> BrainTask {
    BrainTask {
        id: id.into(),
        content: format!("task {id}"),
        status: BrainTaskStatus::InProgress,
        priority: BrainTaskPriority::High,
    }
}

/// A remote-Brain server on a loopback port with one registered test runner
/// whose turns the test parks.
struct SocketBrain {
    server: Arc<crate::server::AgentServer>,
    lifecycle: BrainLifecycleService,
    address: std::net::SocketAddr,
    runner_rx: tokio::sync::mpsc::UnboundedReceiver<crate::server::RunnerRequest>,
    http: tokio::task::JoinHandle<()>,
}

impl SocketBrain {
    async fn start(root: &std::path::Path) -> Self {
        let server = Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                crate::brain::BrainStore::with_root("box.local", Some(root.into())),
                crate::brain::BrainCredentialAuthority::ephemeral([64; 32]),
                "test-password".into(),
                root,
            )
            .unwrap(),
        );
        let lifecycle = BrainLifecycleService::from_server(&server);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let http_agent = server.clone();
        let http = tokio::spawn(async move {
            axum::serve(
                listener,
                create_remote_brain_router(http_agent)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let snapshot = lifecycle.snapshot("shared").unwrap();
        let lease = lifecycle
            .acquire_runner("shared", "runner", &snapshot.environment, None, 60_000)
            .unwrap();
        let (runner_tx, runner_rx) = tokio::sync::mpsc::unbounded_channel();
        lifecycle.register_test_runner("shared", lease.lease_id, runner_tx);
        Self {
            server,
            lifecycle,
            address,
            runner_rx,
            http,
        }
    }

    /// The mutation precondition `RemoteBrainClient::prepare_mutation` builds:
    /// the revision of a snapshot taken just before the submit.
    fn mutation_at_current_revision(&self) -> BrainRemoteMutation {
        let current = self.lifecycle.snapshot("shared").unwrap();
        BrainRemoteMutation {
            brain_id: current.brain_id,
            expected_revision: current.revision,
            environment_generation: current.environment.generation,
            idempotency_key: uuid::Uuid::new_v4(),
        }
    }
}

impl Drop for SocketBrain {
    fn drop(&mut self) {
        self.http.abort();
    }
}

async fn send_command(socket: &mut TestSocket, command: BrainRemoteCommand) {
    socket
        .send(tokio_tungstenite::tungstenite::Message::Binary(
            crate::brain::encode_brain_remote_envelope(&BrainRemoteEnvelope::Command(command))
                .unwrap(),
        ))
        .await
        .unwrap();
}

/// Read socket frames until the reply to `request_id`, skipping projections.
/// Also returns the request ids of every other reply seen on the way, so a
/// test can assert that a command which must still be waiting was not
/// answered first.
async fn reply_to(socket: &mut TestSocket, request_id: u64) -> (BrainRemoteReply, Vec<u64>) {
    let mut earlier = Vec::new();
    loop {
        let frame = socket
            .next()
            .await
            .expect("socket must stay open until the reply")
            .expect("socket frame must be readable");
        let tokio_tungstenite::tungstenite::Message::Binary(bytes) = frame else {
            continue;
        };
        if let Ok(BrainRemoteEnvelope::Reply(reply)) =
            crate::brain::decode_brain_remote_envelope(&bytes)
        {
            if reply.request_id() == request_id {
                return (reply, earlier);
            }
            earlier.push(reply.request_id());
        }
    }
}

/// Liveness only, far inside `todo_write`'s 30-second tool timeout: expiry
/// means the command is queued behind the socket's suspended prompt.
async fn reply_during_turn(
    brain: &SocketBrain,
    socket: &mut TestSocket,
    request_id: u64,
) -> (BrainRemoteReply, Vec<u64>) {
    let reply = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reply_to(socket, request_id),
    )
    .await;
    reply.unwrap_or_else(|_| {
        let snapshot = brain.lifecycle.snapshot("shared").unwrap();
        panic!(
            "a task list replacement sent on the socket during an active turn must commit \
             without waiting for that turn, but request {request_id} hung behind the socket's \
             suspended prompt command (todo_write would time out); durable tasks: {:?}, runs: \
             {:?}",
            snapshot.tasks, snapshot.runs
        )
    })
}

/// A driver's socket whose `Prompt` command (request 1) is suspended on a
/// dispatched turn. The returned turn request must stay alive: dropping it
/// would end the turn and release the socket's command worker.
async fn socket_with_a_parked_turn(
    brain: &mut SocketBrain,
) -> (TestSocket, crate::server::RunnerTurnRequest) {
    let driver = brain
        .lifecycle
        .attach("shared", "alice", AttachmentRole::Driver, None)
        .unwrap();
    let mut socket =
        connect_test_brain_socket(&brain.server, brain.address, "shared", &driver).await;
    send_command(
        &mut socket,
        BrainRemoteCommand {
            request_id: 1,
            mutation: Some(brain.mutation_at_current_revision()),
            kind: BrainRemoteCommandKind::Submit(BrainEventKind::Prompt {
                text: "plan the work".into(),
                attached_mentions: Vec::new(),
            }),
        },
    )
    .await;
    let turn =
        match tokio::time::timeout(std::time::Duration::from_secs(10), brain.runner_rx.recv())
            .await
            .expect("the socket's prompt hung before reaching its runner")
            .expect("runner channel must stay open")
        {
            crate::server::RunnerRequest::Turn(turn) => turn,
            other => panic!("a prompt must dispatch a turn, got {other:?}"),
        };
    (socket, turn)
}

fn replacement(brain: &SocketBrain, request_id: u64, tasks: Vec<BrainTask>) -> BrainRemoteCommand {
    BrainRemoteCommand {
        request_id,
        mutation: Some(brain.mutation_at_current_revision()),
        kind: BrainRemoteCommandKind::Submit(BrainEventKind::TaskListReplaced { tasks }),
    }
}

/// `(seq, first task id)` of every journaled task list, in journal order.
fn journaled_task_lists(snapshot: &crate::brain::BrainSnapshot) -> Vec<(u64, Option<String>)> {
    snapshot
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            BrainEventKind::TaskListReplaced { tasks } => {
                Some((event.seq, tasks.first().map(|task| task.id.clone())))
            }
            _ => None,
        })
        .collect()
}

fn turn_is_still_running(
    snapshot: &crate::brain::BrainSnapshot,
    turn: &crate::server::RunnerTurnRequest,
) -> bool {
    snapshot
        .runs
        .iter()
        .any(|run| run.run_id == turn.run_id && !run.status.is_terminal())
}

/// Issue #1646 (on the WebSocket transport a task list write during a turn
/// still waits for the turn): the socket's `Prompt` command is suspended on
/// its turn, and the turn's own `todo_write` arrives on the same socket.
#[tokio::test(flavor = "current_thread")]
async fn test_task_list_replacement_on_the_socket_during_a_turn_does_not_wait_for_the_turn() {
    let temp = tempfile::tempdir().unwrap();
    let mut brain = SocketBrain::start(temp.path()).await;
    let (mut socket, turn) = socket_with_a_parked_turn(&mut brain).await;

    let tasks = vec![socket_task("1")];
    send_command(&mut socket, replacement(&brain, 2, tasks.clone())).await;
    let (reply, _) = reply_during_turn(&brain, &mut socket, 2).await;

    let snapshot = brain.lifecycle.snapshot("shared").unwrap();
    assert!(
        matches!(reply, BrainRemoteReply::Submitted { run: None, .. }),
        "the replacement must be accepted and must not start a run: {reply:?}"
    );
    assert_eq!(
        tasks, snapshot.tasks,
        "the replacement must be durable while run {:?} is still active; runs: {:?}",
        turn.run_id, snapshot.runs
    );
    assert!(
        turn_is_still_running(&snapshot, &turn),
        "the turn that issued the replacement must still be running, so the write did not \
         simply wait for it; runs: {:?}",
        snapshot.runs
    );
}

/// Two `todo_write` calls in one turn over the socket, then a retry of the
/// second with the same mutation receipt (a client that lost the reply):
/// each list is journaled once, in the order sent, and the retry replays the
/// original event instead of appending a third.
#[tokio::test(flavor = "current_thread")]
async fn test_socket_task_list_replacements_during_a_turn_commit_once_each_in_order_and_a_retry_replays(
) {
    let temp = tempfile::tempdir().unwrap();
    let mut brain = SocketBrain::start(temp.path()).await;
    let (mut socket, turn) = socket_with_a_parked_turn(&mut brain).await;

    send_command(
        &mut socket,
        replacement(&brain, 2, vec![socket_task("first")]),
    )
    .await;
    let (first, _) = reply_during_turn(&brain, &mut socket, 2).await;
    let second_command = replacement(&brain, 3, vec![socket_task("second")]);
    send_command(&mut socket, second_command.clone()).await;
    let (second, _) = reply_during_turn(&brain, &mut socket, 3).await;
    let retry = BrainRemoteCommand {
        request_id: 4,
        ..second_command
    };
    send_command(&mut socket, retry).await;
    let (retried, _) = reply_during_turn(&brain, &mut socket, 4).await;

    let snapshot = brain.lifecycle.snapshot("shared").unwrap();
    let journaled = journaled_task_lists(&snapshot);
    assert_eq!(
        vec![Some("first".to_string()), Some("second".to_string())],
        journaled
            .iter()
            .map(|(_, id)| id.clone())
            .collect::<Vec<_>>(),
        "each replacement must be journaled exactly once, in the order sent, and a retry with \
         the same receipt must not append again; replies: {first:?} / {second:?} / {retried:?}"
    );
    let accepted_seq = |reply: &BrainRemoteReply| match reply {
        BrainRemoteReply::Submitted { accepted, .. } => Some(accepted.seq),
        _ => None,
    };
    assert_eq!(
        accepted_seq(&second),
        accepted_seq(&retried),
        "the retry must replay the event its first attempt appended: second {second:?}, retry \
         {retried:?}"
    );
    assert_eq!(
        Some(journaled[1].0),
        accepted_seq(&retried),
        "the replayed event must be the journaled second replacement: {journaled:?}"
    );
    assert_eq!(
        vec![socket_task("second")],
        snapshot.tasks,
        "the later replacement must be the durable task list: {journaled:?}"
    );
    assert!(
        turn_is_still_running(&snapshot, &turn),
        "the turn must still be running; runs: {:?}",
        snapshot.runs
    );
}

/// Only the task list (and approval decisions) leave the socket's serial
/// command worker. A participant message sent before the replacement is
/// still waiting for the turn when the replacement has already committed.
#[tokio::test(flavor = "current_thread")]
async fn test_participant_message_on_the_socket_during_a_turn_still_waits_for_the_turn() {
    let temp = tempfile::tempdir().unwrap();
    let mut brain = SocketBrain::start(temp.path()).await;
    let (mut socket, turn) = socket_with_a_parked_turn(&mut brain).await;

    send_command(
        &mut socket,
        BrainRemoteCommand {
            request_id: 2,
            mutation: Some(brain.mutation_at_current_revision()),
            kind: BrainRemoteCommandKind::Submit(BrainEventKind::ParticipantMessage {
                text: "a note for after the turn".into(),
            }),
        },
    )
    .await;
    send_command(&mut socket, replacement(&brain, 3, vec![socket_task("1")])).await;
    let (_, answered_first) = reply_during_turn(&brain, &mut socket, 3).await;

    let snapshot = brain.lifecycle.snapshot("shared").unwrap();
    assert!(
        !answered_first.contains(&2) && !answered_first.contains(&1),
        "a participant message and the prompt itself must still be waiting for the running turn \
         when a later task list replacement is answered; replies seen first: {answered_first:?}"
    );
    assert!(
        !snapshot
            .events
            .iter()
            .any(|event| matches!(event.kind, BrainEventKind::ParticipantMessage { .. })),
        "a participant message sent during a turn must not be journaled until the turn ends; \
         events: {:?}",
        snapshot
            .events
            .iter()
            .map(|event| (event.seq, &event.kind))
            .collect::<Vec<_>>()
    );
    assert!(
        turn_is_still_running(&snapshot, &turn),
        "the turn must still be running; runs: {:?}",
        snapshot.runs
    );
}

/// The role check is unchanged by the routing: an observer's socket cannot
/// replace the task list during someone else's turn, and the refusal comes
/// back promptly instead of waiting for the turn.
#[tokio::test(flavor = "current_thread")]
async fn test_observer_socket_task_list_replacement_during_a_turn_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let mut brain = SocketBrain::start(temp.path()).await;
    let (_driver_socket, turn) = socket_with_a_parked_turn(&mut brain).await;

    let observer = brain
        .lifecycle
        .attach("shared", "olive", AttachmentRole::Observer, None)
        .unwrap();
    let mut observer_socket =
        connect_test_brain_socket(&brain.server, brain.address, "shared", &observer).await;
    send_command(
        &mut observer_socket,
        replacement(&brain, 9, vec![socket_task("intruder")]),
    )
    .await;
    let (reply, _) = reply_during_turn(&brain, &mut observer_socket, 9).await;

    let snapshot = brain.lifecycle.snapshot("shared").unwrap();
    assert!(
        matches!(reply, BrainRemoteReply::Error { .. }),
        "an observer must not be able to replace the task list: {reply:?}"
    );
    assert!(
        snapshot.tasks.is_empty() && journaled_task_lists(&snapshot).is_empty(),
        "a refused replacement must leave no task list behind: tasks {:?}",
        snapshot.tasks
    );
    assert!(
        turn_is_still_running(&snapshot, &turn),
        "the turn must still be running; runs: {:?}",
        snapshot.runs
    );
}

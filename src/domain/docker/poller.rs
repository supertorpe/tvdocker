//! Background pollers (adapted from oh-my-docker's `runtime/tasks.rs`):
//! the containers poller classifies connection state purely (3
//! consecutive failures ⇒ Down); the four `spawn_list` pollers are
//! silent, and `spawn_events` only streams (— connection
//! state has exactly ONE emitter). The FIVE POLLERS — containers plus
//! the four `spawn_list` — each hold a `broadcast::Receiver` and
//! consume [`ActionReq::PollNow`] from the shared broadcast channel
//! (one menu press re-polls every list). `spawn_events`
//! takes no receiver: Docker pushes the stream, there is nothing to
//! poll on request. All pollers apply the bounded-channel drop-new
//! policy via [`offer`] (events bypass it with [`flush_events`] — they
//! are not superseded).
use super::client;
use super::containers;
use super::events::{self, DockerEvent};
use crate::core::{ActionReq, AppMsg, Connection};
use bollard::system::EventsOptions;
use futures_util::{FutureExt, StreamExt};
use std::future::Future;
use std::time::Duration;
use tokio::sync::broadcast::Receiver as ActionReceiver;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::{Sender, error::TrySendError};
use tokio::time::MissedTickBehavior;

const FAILURE_THRESHOLD: u32 = 3;

/// Pure classification of consecutive poll outcomes (unit-tested; the only
/// part of the poller that carries logic worth testing without a daemon).
#[derive(Debug, Default)]
pub struct PollState {
    consecutive_failures: u32,
    last_emitted: Option<Connection>,
}

impl PollState {
    /// A successful list: reset the failure count; emit `Up` on recovery
    /// (silent while already `Up`).
    pub fn on_success(&mut self) -> Option<AppMsg> {
        self.consecutive_failures = 0;
        self.emit(Connection::Up)
    }

    /// A failed list: emit `Down` on the THIRD consecutive failure, then
    /// stay silent (same variant — the status line keeps the first error).
    pub fn on_failure(&mut self, error: String) -> Option<AppMsg> {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= FAILURE_THRESHOLD {
            self.emit(Connection::Down { error })
        } else {
            None
        }
    }

    /// Initial connect failure: `Down` immediately, no threshold wait.
    pub fn on_connect_error(&mut self, error: String) -> Option<AppMsg> {
        self.consecutive_failures = FAILURE_THRESHOLD;
        self.emit(Connection::Down { error })
    }

    fn emit(&mut self, next: Connection) -> Option<AppMsg> {
        let same_variant = matches!(
            (&self.last_emitted, &next),
            (Some(Connection::Up), Connection::Up)
                | (Some(Connection::Down { .. }), Connection::Down { .. })
                | (Some(Connection::Checking), Connection::Checking)
        );
        if same_variant {
            None
        } else {
            self.last_emitted = Some(next.clone());
            Some(AppMsg::Connection(next))
        }
    }
}

/// Bounded-channel send: when the
/// channel is full the NEW message is dropped (the next poll supersedes
/// it); `false` means the receiver is gone and the task must stop.
pub fn offer(tx: &Sender<AppMsg>, msg: AppMsg) -> bool {
    match tx.try_send(msg) {
        Ok(()) | Err(TrySendError::Full(_)) => true,
        Err(TrySendError::Closed(_)) => false,
    }
}

/// Spawn the containers poller . Fires on every interval tick and
/// on every [`ActionReq::PollNow`]; reconnects lazily after an initial
/// connect failure. Interval comes from `config.polling.containers_ms`.
pub fn spawn_containers(
    tx: Sender<AppMsg>,
    mut actions: ActionReceiver<ActionReq>,
    interval_ms: u64,
) {
    tokio::spawn(async move {
        let mut state = PollState::default();
        let mut docker: Option<bollard::Docker> = match client::connect() {
            Ok(d) => Some(d),
            Err(e) => {
                if let Some(msg) = state.on_connect_error(format!("Docker unreachable: {e}"))
                    && !offer(&tx, msg)
                {
                    return;
                }
                None
            }
        };
        let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
        let mut actions_open = true;
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                // Disabled once the UI drops its sender: never spin on a
                // closed channel. Broadcast: Ok = any request (one
                // variant today), Lagged = missed polls are fine (the
                // tick still runs), Closed = no more subscribers.
                req = actions.recv(), if actions_open => {
                    match req {
                        Ok(_) => {}
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => actions_open = false,
                    }
                }
            }
            // (Re)connect lazily: covers both the initial failure and a
            // handle that can no longer reach the daemon.
            let conn = match &docker {
                Some(d) => d.clone(),
                None => match client::connect() {
                    Ok(d) => {
                        docker = Some(d.clone());
                        d
                    }
                    Err(e) => {
                        if let Some(msg) =
                            state.on_connect_error(format!("Docker unreachable: {e}"))
                            && !offer(&tx, msg)
                        {
                            return;
                        }
                        continue;
                    }
                },
            };
            match containers::list_containers(&conn).await {
                Ok(list) => {
                    if let Some(msg) = state.on_success()
                        && !offer(&tx, msg)
                    {
                        return;
                    }
                    if !offer(&tx, AppMsg::Containers(list)) {
                        return;
                    }
                }
                Err(e) => {
                    if let Some(msg) = state.on_failure(format!("Docker: {e}"))
                        && !offer(&tx, msg)
                    {
                        return;
                    }
                }
            }
        }
    });
}

/// Spawn a generic list poller : every interval tick or
/// [`ActionReq::PollNow`], `list(docker)` runs and the wrapped rows
/// are offered to the bridge. Failures are SILENT — connection state
/// is the containers poller's exclusive job (a second
/// emitter would ping-pong the status line). `list` is any
/// `Fn(Docker) -> Future<Result<Vec<T>>>` so each call site passes its
/// domain function (`|d| async move { images::list_images(&d).await }`).
pub fn spawn_list<T, Fut>(
    tx: Sender<AppMsg>,
    mut actions: ActionReceiver<ActionReq>,
    interval_ms: u64,
    list: impl Fn(bollard::Docker) -> Fut + Send + 'static,
    wrap: fn(Vec<T>) -> AppMsg,
) where
    Fut: Future<Output = anyhow::Result<Vec<T>>> + Send + 'static,
    T: Send + 'static,
{
    tokio::spawn(async move {
        // Silent connect: retry on every tick until the daemon shows up.
        let mut docker: Option<bollard::Docker> = client::connect().ok();
        let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
        // Serial round trips (statistics) must not burst-catch-up
        // after a slow batch.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut actions_open = true;
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                req = actions.recv(), if actions_open => {
                    match req {
                        Ok(_) => {}
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => actions_open = false,
                    }
                }
            }
            let conn = match &docker {
                Some(d) => d.clone(),
                None => match client::connect() {
                    Ok(d) => {
                        docker = Some(d.clone());
                        d
                    }
                    Err(_) => continue, // silent retry next tick
                },
            };
            match list(conn).await {
                Ok(rows) => {
                    if !offer(&tx, wrap(rows)) {
                        return; // bridge is gone
                    }
                }
                Err(_) => { /* silent — next tick retries */ }
            }
        }
    });
}

/// Send one batch WITHOUT the drop-new policy: events
/// are not superseded by a later poll, so the producer waits for room
/// (`reserve().await`) instead of dropping. `false` = the receiver is
/// gone and the task must stop. An empty batch is a no-op (`true`):
/// an empty `Events` message would bump the revision for nothing.
pub async fn flush_events(tx: &Sender<AppMsg>, batch: Vec<DockerEvent>) -> bool {
    if batch.is_empty() {
        return true;
    }
    match tx.reserve().await {
        Ok(permit) => {
            permit.send(AppMsg::Events(batch));
            true
        }
        Err(_) => false,
    }
}

/// Spawn the Docker event stream : connect → stream → batch
/// → bridge, reconnecting forever (connect failure,
/// stream end and stream error all fall through to a 5 s sleep).
/// `since` is "now" at stream start — the live stream is the feature
/// (deviation from oh-my-docker's one-hour backlog: no
/// history replay on reconnect). Per event: push, drain whatever else
/// is already ready, then flush the batch (no loss — `flush_events`
/// waits for room).
pub fn spawn_events(tx: Sender<AppMsg>) {
    tokio::spawn(async move {
        let mut batch: Vec<DockerEvent> = Vec::new();
        loop {
            if tx.is_closed() {
                return;
            }
            if let Ok(conn) = client::connect() {
                let options = EventsOptions::<String> {
                    since: Some(chrono::Local::now().timestamp().to_string()),
                    until: None,
                    filters: std::collections::HashMap::new(),
                };
                let mut stream = conn.events(Some(options));
                // Stream error or end ⇒ the loop exits → reconnect after
                // backoff (clippy `while_let_loop`: no `loop`+`match`+`break`).
                while let Some(Ok(ev)) = stream.next().await {
                    batch.push(to_entry_now(ev));
                    // Ready-drain: one bridge slot per burst.
                    while let Some(Some(Ok(ev))) = stream.next().now_or_never() {
                        batch.push(to_entry_now(ev));
                    }
                    if !flush_events(&tx, std::mem::take(&mut batch)).await {
                        return; // bridge is gone
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

/// One event → our row, stamped with the local wall clock (chrono —
/// the timestamp is passed INTO the pure `to_entry`).
fn to_entry_now(ev: bollard::models::EventMessage) -> DockerEvent {
    events::to_entry(chrono::Local::now().format("%H:%M:%S").to_string(), ev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::docker::events::DockerEvent;

    fn event(action: &str) -> DockerEvent {
        DockerEvent {
            timestamp: "12:00:00".into(),
            kind: "container".into(),
            action: action.into(),
            actor: "abc".into(),
        }
    }

    #[tokio::test]
    async fn flush_events_waits_for_room_instead_of_dropping_and_stops_when_closed() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AppMsg>(1);
        // Fill the cap-1 channel.
        tx.send(AppMsg::Connection(Connection::Up)).await.unwrap();

        let tx2 = tx.clone();
        let waiter = tokio::spawn(async move {
            // The batch and the sender both move INTO the future — a bare
            // `spawn(flush_events(&tx2, …))` would borrow a local and fail
            // the 'static bound.
            flush_events(&tx2, vec![event("start")]).await
        });
        for _ in 0..4 {
            tokio::task::yield_now().await; // let the task reach reserve().await
        }
        assert!(
            !waiter.is_finished(),
            "full ⇒ the producer WAITS; events are never superseded"
        );

        // Make room: the waiting batch must then arrive intact.
        let got = rx.recv().await.expect("first message (the filler)");
        assert_eq!(got, AppMsg::Connection(Connection::Up));
        assert!(
            waiter.await.expect("flush task"),
            "delivered once there was room"
        );
        let second = rx.recv().await.expect("the batch");
        let AppMsg::Events(batch) = second else {
            panic!("flush_events must send an Events batch");
        };
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].action, "start");

        // Closed receiver ⇒ false (the task stops instead of spinning).
        drop(rx);
        assert!(
            !flush_events(&tx, vec![event("stop")]).await,
            "closed ⇒ false ⇒ spawn_events returns"
        );
    }

    #[tokio::test]
    async fn flush_events_never_emits_an_empty_batch() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AppMsg>(1);
        assert!(flush_events(&tx, vec![]).await, "nothing to send ⇒ true");
        assert!(
            rx.try_recv().is_err(),
            "no empty Events message ⇒ no phantom revision bump"
        );
    }

    #[test]
    fn first_two_failures_stay_silent_then_third_emits_down_once() {
        let mut st = PollState::default();
        assert_eq!(st.on_failure("e1".into()), None);
        assert_eq!(st.on_failure("e2".into()), None);
        let msg = st.on_failure("e3".into()).expect("third failure emits");
        assert!(matches!(msg, AppMsg::Connection(Connection::Down { .. })));
        // Further failures keep the Down state silent (same variant) — the
        // status line keeps showing the FIRST error, no message churn.
        assert_eq!(st.on_failure("e4-different-error".into()), None);
    }

    #[test]
    fn success_resets_and_emits_up() {
        let mut st = PollState::default();
        st.on_failure("e1".into());
        st.on_failure("e2".into());
        let msg = st.on_success().expect("recovery emits Up");
        assert_eq!(msg, AppMsg::Connection(Connection::Up));
        assert_eq!(st.on_success(), None, "steady-state Up is silent");
    }

    #[test]
    fn connect_error_emits_down_immediately() {
        let mut st = PollState::default();
        let msg = st
            .on_connect_error("no socket".into())
            .expect("emits at once");
        assert!(matches!(
            msg,
            AppMsg::Connection(Connection::Down { ref error }) if error == "no socket"
        ));
        assert_eq!(st.on_failure("again".into()), None, "already Down");
    }

    #[test]
    fn offer_drops_the_new_message_when_the_channel_is_full() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AppMsg>(1);
        assert!(offer(&tx, AppMsg::Connection(Connection::Up)));
        assert!(
            offer(
                &tx,
                AppMsg::Connection(Connection::Down { error: "x".into() })
            ),
            "full is not an error: the NEW message is dropped by policy"
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            AppMsg::Connection(Connection::Up),
            "the older message is kept — the next poll supersedes the dropped one"
        );
        drop(rx);
        assert!(
            !offer(&tx, AppMsg::Connection(Connection::Up)),
            "closed receiver reports stop"
        );
    }
}

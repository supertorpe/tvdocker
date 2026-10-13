//! Docker event mapping (adapted from oh-my-docker's `docker/events.rs`,
//!): a pure `to_entry` here; the streaming loop itself lives in
//! `poller::spawn_events`.

use bollard::models::EventMessage;

/// One row of the Events window (field-for-field oh-my-docker's
/// `DockerEvent`); the timestamp arrives pre-formatted so `to_entry`
/// stays pure.
#[derive(Clone, Debug, PartialEq)]
pub struct DockerEvent {
    pub timestamp: String,
    pub kind: String,
    pub action: String,
    pub actor: String,
}

/// Map one streamed event (pure): `timestamp` is `"%H:%M:%S"` local time
/// supplied by the caller (chrono lives in the poller);
/// the rest is the vendored extraction (`typ` → lowercase name,
/// `actor.id`).
pub fn to_entry(timestamp: String, event: EventMessage) -> DockerEvent {
    let kind = event
        .typ
        .as_ref()
        .map(|t| t.to_string())
        .unwrap_or_default();
    let action = event.action.unwrap_or_default();
    let actor = event.actor.and_then(|a| a.id).unwrap_or_default();
    DockerEvent {
        timestamp,
        kind,
        action,
        actor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{EventActor, EventMessageTypeEnum};

    #[test]
    fn extracts_kind_action_and_actor_id() {
        let ev = EventMessage {
            typ: Some(EventMessageTypeEnum::CONTAINER),
            action: Some("start".into()),
            actor: Some(EventActor {
                id: Some("abc123".into()),
                attributes: None,
            }),
            scope: None,
            time: None,
            time_nano: None,
        };
        let e = to_entry("12:34:56".into(), ev);
        assert_eq!(e.timestamp, "12:34:56");
        assert_eq!(e.kind, "container", "Display prints the serde name");
        assert_eq!(e.action, "start");
        assert_eq!(e.actor, "abc123");
    }

    #[test]
    fn missing_fields_become_empty_strings_not_panics() {
        let e = to_entry("00:00:00".into(), EventMessage::default());
        assert_eq!(e.kind, "");
        assert_eq!(e.action, "");
        assert_eq!(e.actor, "");
    }
}

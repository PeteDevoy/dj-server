use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Playback rate is clamped to +/-6% of normal speed.
pub const MIN_PLAYBACK_RATE: f64 = 0.94;
pub const MAX_PLAYBACK_RATE: f64 = 1.06;

/// Which deck a message targets. Two fully independent decks (own
/// transport, cue point, loop, nudge/bass-cut/pitch-lock settings, own
/// revision counter) share one room/connection - every message that isn't
/// deck-agnostic (ClockRequest, StateRequest, Welcome, Error) carries one of
/// these to say which deck it's about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeckId {
    A,
    B,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportAction {
    Play,
    Pause,
    /// Resets the playhead to the beginning of the track without changing
    /// whether it's playing or paused.
    Restart,
    /// Jumps the playhead to an arbitrary position (e.g. clicking/dragging a
    /// waveform) without changing whether it's playing or paused. Generalizes
    /// `Restart` (seeking to position 0) to any target position. The target
    /// itself travels via `ClientMessage::SeekRequest`, not this action -
    /// `TransportAction` alone carries no data - so this variant only ever
    /// appears as the tag on the resulting broadcast `TransportEvent`.
    Seek,
    /// Changes the canonical playback rate without changing whether it's
    /// playing or paused.
    SetTempo,
    /// Releases a cue-point preview hold: pauses the transport at whatever
    /// `DeckState.cue_point_us` currently holds. Carries no data of its own
    /// (unlike `Seek`) since the server is already the sole source of truth
    /// for the cue point's position - the client never needs to (and
    /// can't) tell it what position to release to, which is exactly what
    /// keeps "only one cue point at a time" trivially true. Dispatched
    /// through the plain `TransportRequest` path like `Restart`, not a
    /// dedicated message like `Seek`/`SetTempo`.
    CueRelease,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransportStateDto {
    pub playing: bool,
    pub anchor_position_us: u64,
    pub anchor_server_time_us: u64,
    pub playback_rate: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LoopRegionDto {
    pub start_us: u64,
    pub end_us: u64,
    pub active: bool,
}

/// One deck's full state, as sent in `ServerMessage::StateSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeckStateDto {
    pub revision: u64,
    pub transport: TransportStateDto,
    pub nudge_enabled: bool,
    pub bass_cut_enabled: bool,
    pub pitch_lock_enabled: bool,
    pub pfl_enabled: bool,
    pub cue_point_us: Option<u64>,
    pub loop_region: Option<LoopRegionDto>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    ClockRequest {
        request_id: String,
        client_send_time_ms: f64,
    },
    TransportRequest {
        request_id: String,
        deck: DeckId,
        action: TransportAction,
    },
    StateRequest {
        request_id: String,
    },
    /// Toggles a deck's client-side tempo-nudge drift correction on or off,
    /// synced room-wide like any other setting - not part of the
    /// authoritative transport timeline, but shares that deck's revision
    /// counter.
    SetNudgeEnabled {
        request_id: String,
        deck: DeckId,
        enabled: bool,
    },
    /// Sets a deck's canonical playback rate, synced room-wide like play/pause.
    SetTempoRequest {
        request_id: String,
        deck: DeckId,
        playback_rate: f64,
    },
    /// Toggles a deck's room-wide bass-cut effect (a highpass filter
    /// client-side), synced the same way as the nudge-enabled setting.
    SetBassCutEnabled {
        request_id: String,
        deck: DeckId,
        enabled: bool,
    },
    /// Toggles whether a deck's tempo changes should be pitch-corrected
    /// client-side, synced the same way as the nudge-enabled and bass-cut
    /// settings.
    SetPitchLockEnabled {
        request_id: String,
        deck: DeckId,
        enabled: bool,
    },
    /// Toggles whether a deck is included in a headphone-role client's
    /// local mix (pre-fader listen / "cue", in traditional mixer terms) -
    /// a master-role client always hears every playing deck regardless of
    /// this setting. Synced the same way as the other boolean settings;
    /// which *hardware role* a given client plays is a separate, local-only
    /// choice each client makes for itself (see the client's own "Hardware
    /// role" selection), never sent to the server at all.
    SetPflEnabled {
        request_id: String,
        deck: DeckId,
        enabled: bool,
    },
    /// Seeks a deck to an arbitrary position, synced room-wide like
    /// play/pause - e.g. clicking/dragging a waveform. A dedicated message
    /// (like `SetTempoRequest`) rather than a `TransportRequest` action,
    /// since it carries a position `TransportAction` alone has no room for.
    SeekRequest {
        request_id: String,
        deck: DeckId,
        position_us: u64,
    },
    /// Sets (or overwrites) a deck's single cue point at an arbitrary
    /// position, synced room-wide like play/pause/seek - e.g. pressing Cue
    /// while paused. A dedicated message like `SeekRequest`, since it
    /// carries a position `TransportAction` has no room for.
    SetCuePoint {
        request_id: String,
        deck: DeckId,
        position_us: u64,
    },
    /// Clears a deck's cue point entirely (back to "never set") - e.g. when
    /// a fresh track is loaded onto that deck, so a leftover cue point from
    /// the previous track can't linger. A no-op if that deck has no cue
    /// point.
    RemoveCuePoint {
        request_id: String,
        deck: DeckId,
    },
    /// Inserts/overwrites a deck's single loop region, always active, at an
    /// arbitrary [start_us, end_us) - e.g. pressing Loop with no active loop
    /// at the playhead. Synced room-wide like `SetCuePoint`.
    SetLoop {
        request_id: String,
        deck: DeckId,
        start_us: u64,
        end_us: u64,
    },
    /// Toggles a deck's existing loop's active flag without changing its
    /// bounds (e.g. pressing Loop on an already-active loop to deactivate
    /// it, or Reloop/exit in either direction). A no-op if that deck has no
    /// loop yet.
    SetLoopActive {
        request_id: String,
        deck: DeckId,
        active: bool,
    },
    /// Removes a deck's loop region entirely (not just deactivating it) -
    /// e.g. pressing Cue while paused with the playhead inside the loop. A
    /// no-op if that deck has no loop.
    RemoveLoop {
        request_id: String,
        deck: DeckId,
    },
    /// Sets the room's crossfader position (0.0 = fully Deck A, 1.0 = fully
    /// Deck B), synced room-wide - not tied to either deck individually, so
    /// unlike every other message above this one carries no `deck` field.
    SetCrossfaderPosition {
        request_id: String,
        position: f64,
    },
    /// Sets the room's crossfader curve shape (0.0 = equal-power/smooth,
    /// 1.0 = plateau/fast-cut), synced room-wide like the position itself.
    SetCrossfaderCurve {
        request_id: String,
        shape: f64,
    },
}

impl ClientMessage {
    pub fn request_id(&self) -> &str {
        match self {
            ClientMessage::ClockRequest { request_id, .. } => request_id,
            ClientMessage::TransportRequest { request_id, .. } => request_id,
            ClientMessage::StateRequest { request_id } => request_id,
            ClientMessage::SetNudgeEnabled { request_id, .. } => request_id,
            ClientMessage::SetTempoRequest { request_id, .. } => request_id,
            ClientMessage::SetBassCutEnabled { request_id, .. } => request_id,
            ClientMessage::SetPitchLockEnabled { request_id, .. } => request_id,
            ClientMessage::SetPflEnabled { request_id, .. } => request_id,
            ClientMessage::SeekRequest { request_id, .. } => request_id,
            ClientMessage::SetCuePoint { request_id, .. } => request_id,
            ClientMessage::RemoveCuePoint { request_id, .. } => request_id,
            ClientMessage::SetLoop { request_id, .. } => request_id,
            ClientMessage::SetLoopActive { request_id, .. } => request_id,
            ClientMessage::RemoveLoop { request_id, .. } => request_id,
            ClientMessage::SetCrossfaderPosition { request_id, .. } => request_id,
            ClientMessage::SetCrossfaderCurve { request_id, .. } => request_id,
        }
    }

    /// Basic field validation beyond what serde already enforces via types.
    pub fn validate(&self) -> Result<(), String> {
        let request_id = self.request_id();
        if request_id.trim().is_empty() {
            return Err("request_id must not be empty".to_string());
        }

        if let ClientMessage::SetTempoRequest { playback_rate, .. } = self {
            // A small epsilon guards against a slider's floating-point steps
            // landing a hair outside the nominal bound.
            if *playback_rate < MIN_PLAYBACK_RATE - 1e-9 || *playback_rate > MAX_PLAYBACK_RATE + 1e-9 {
                return Err(format!(
                    "playback_rate must be between {MIN_PLAYBACK_RATE} and {MAX_PLAYBACK_RATE}"
                ));
            }
        }

        if let ClientMessage::SetLoop { start_us, end_us, .. } = self {
            if end_us <= start_us {
                return Err("end_us must be greater than start_us".to_string());
            }
        }

        if let ClientMessage::SetCrossfaderPosition { position, .. } = self {
            if !(0.0..=1.0).contains(position) {
                return Err("position must be between 0.0 and 1.0".to_string());
            }
        }

        if let ClientMessage::SetCrossfaderCurve { shape, .. } = self {
            if !(0.0..=1.0).contains(shape) {
                return Err("shape must be between 0.0 and 1.0".to_string());
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        connection_id: Uuid,
        server_time_us: u64,
        schedule_lead_time_us: u64,
    },
    StateSnapshot {
        server_time_us: u64,
        crossfader_revision: u64,
        crossfader_position: f64,
        crossfader_curve_shape: f64,
        deck_a: DeckStateDto,
        deck_b: DeckStateDto,
    },
    ClockResponse {
        request_id: String,
        client_send_time_ms: f64,
        server_receive_time_us: u64,
        server_send_time_us: u64,
    },
    TransportEvent {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        deck: DeckId,
        revision: u64,
        action: TransportAction,
        effective_server_time_us: u64,
        position_us: u64,
        playback_rate: f64,
    },
    NudgeSettingChanged {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        deck: DeckId,
        revision: u64,
        enabled: bool,
    },
    BassCutSettingChanged {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        deck: DeckId,
        revision: u64,
        enabled: bool,
    },
    PitchLockSettingChanged {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        deck: DeckId,
        revision: u64,
        enabled: bool,
    },
    PflSettingChanged {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        deck: DeckId,
        revision: u64,
        enabled: bool,
    },
    /// Broadcast whenever a deck's single cue point is set or overwritten
    /// (see `ClientMessage::SetCuePoint`). Shares that deck's revision
    /// counter, like the other *SettingChanged events.
    CuePointChanged {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        deck: DeckId,
        revision: u64,
        position_us: u64,
    },
    /// Broadcast whenever a deck's cue point is cleared entirely (see
    /// `ClientMessage::RemoveCuePoint`) - unlike `CuePointChanged`, this
    /// describes an absence, so it carries no position_us field. Shares
    /// that deck's revision counter, like `CuePointChanged`.
    CuePointRemoved {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        deck: DeckId,
        revision: u64,
    },
    /// Broadcast whenever a deck's single loop region is inserted,
    /// overwritten, or toggled active/inactive (see `ClientMessage::SetLoop`
    /// / `SetLoopActive`). Shares that deck's revision counter, like
    /// `CuePointChanged`. Always describes a concrete loop (never absent) -
    /// this only ever fires when one exists to describe.
    LoopChanged {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        deck: DeckId,
        revision: u64,
        start_us: u64,
        end_us: u64,
        active: bool,
    },
    /// Broadcast whenever a deck's loop region is removed entirely (see
    /// `ClientMessage::RemoveLoop`) - unlike `LoopChanged`, this describes an
    /// absence, so it carries no start/end/active fields. Shares that deck's
    /// revision counter, like `LoopChanged`.
    LoopRemoved {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        deck: DeckId,
        revision: u64,
    },
    /// Broadcast whenever the room's crossfader position changes (see
    /// `ClientMessage::SetCrossfaderPosition`). Shares a revision counter
    /// with `CrossfaderCurveChanged` - not tied to either deck.
    CrossfaderPositionChanged {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        revision: u64,
        position: f64,
    },
    /// Broadcast whenever the room's crossfader curve shape changes (see
    /// `ClientMessage::SetCrossfaderCurve`). Shares its revision counter
    /// with `CrossfaderPositionChanged`.
    CrossfaderCurveChanged {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        revision: u64,
        shape: f64,
    },
    Error {
        request_id: Option<String>,
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_clock_request() {
        let json = r#"{"type":"clock_request","request_id":"clock-42","client_send_time_ms":9382.45}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::ClockRequest {
                request_id,
                client_send_time_ms,
            } => {
                assert_eq!(request_id, "clock-42");
                assert_eq!(client_send_time_ms, 9382.45);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserializes_transport_request() {
        let json = r#"{"type":"transport_request","request_id":"request-91","deck":"a","action":"play"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::TransportRequest { request_id, deck, action } => {
                assert_eq!(request_id, "request-91");
                assert_eq!(deck, DeckId::A);
                assert_eq!(action, TransportAction::Play);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserializes_transport_request_for_deck_b() {
        let json = r#"{"type":"transport_request","request_id":"request-91b","deck":"b","action":"play"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::TransportRequest { deck, .. } => assert_eq!(deck, DeckId::B),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserializes_transport_request_restart() {
        let json = r#"{"type":"transport_request","request_id":"request-94","deck":"a","action":"restart"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::TransportRequest { request_id, action, .. } => {
                assert_eq!(request_id, "request-94");
                assert_eq!(action, TransportAction::Restart);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserializes_seek_request() {
        let json = r#"{"type":"seek_request","request_id":"request-101","deck":"a","position_us":4500000}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SeekRequest { request_id, position_us, .. } => {
                assert_eq!(request_id, "request-101");
                assert_eq!(position_us, 4_500_000);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserializes_set_cue_point() {
        let json = r#"{"type":"set_cue_point","request_id":"request-104","deck":"a","position_us":6000000}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetCuePoint { request_id, position_us, .. } => {
                assert_eq!(request_id, "request-104");
                assert_eq!(position_us, 6_000_000);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn serializes_cue_point_changed() {
        let msg = ServerMessage::CuePointChanged {
            event_id: Uuid::nil(),
            request_id: "request-104".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 3,
            position_us: 6_000_000,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "cue_point_changed");
        assert_eq!(json["deck"], "a");
        assert_eq!(json["position_us"], 6_000_000);
        assert_eq!(json["revision"], 3);
    }

    #[test]
    fn deserializes_set_loop() {
        let json =
            r#"{"type":"set_loop","request_id":"request-106","deck":"a","start_us":6000000,"end_us":13500000}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetLoop { request_id, start_us, end_us, .. } => {
                assert_eq!(request_id, "request-106");
                assert_eq!(start_us, 6_000_000);
                assert_eq!(end_us, 13_500_000);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn rejects_set_loop_with_end_before_start() {
        let msg = ClientMessage::SetLoop {
            request_id: "req".to_string(),
            deck: DeckId::A,
            start_us: 10_000_000,
            end_us: 5_000_000,
        };
        assert!(msg.validate().is_err());
    }

    #[test]
    fn rejects_set_loop_with_end_equal_to_start() {
        let msg = ClientMessage::SetLoop {
            request_id: "req".to_string(),
            deck: DeckId::A,
            start_us: 5_000_000,
            end_us: 5_000_000,
        };
        assert!(msg.validate().is_err());
    }

    #[test]
    fn deserializes_set_loop_active() {
        let json = r#"{"type":"set_loop_active","request_id":"request-107","deck":"a","active":false}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetLoopActive { request_id, active, .. } => {
                assert_eq!(request_id, "request-107");
                assert!(!active);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn serializes_loop_changed() {
        let msg = ServerMessage::LoopChanged {
            event_id: Uuid::nil(),
            request_id: "request-106".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 4,
            start_us: 6_000_000,
            end_us: 13_500_000,
            active: true,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "loop_changed");
        assert_eq!(json["deck"], "a");
        assert_eq!(json["start_us"], 6_000_000);
        assert_eq!(json["end_us"], 13_500_000);
        assert_eq!(json["active"], true);
        assert_eq!(json["revision"], 4);
    }

    #[test]
    fn deserializes_remove_cue_point() {
        let json = r#"{"type":"remove_cue_point","request_id":"request-109","deck":"a"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::RemoveCuePoint { request_id, deck } => {
                assert_eq!(request_id, "request-109");
                assert_eq!(deck, DeckId::A);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn serializes_cue_point_removed() {
        let msg = ServerMessage::CuePointRemoved {
            event_id: Uuid::nil(),
            request_id: "request-109".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 8,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "cue_point_removed");
        assert_eq!(json["deck"], "a");
        assert_eq!(json["revision"], 8);
    }

    #[test]
    fn deserializes_remove_loop() {
        let json = r#"{"type":"remove_loop","request_id":"request-108","deck":"a"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::RemoveLoop { request_id, deck } => {
                assert_eq!(request_id, "request-108");
                assert_eq!(deck, DeckId::A);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn serializes_loop_removed() {
        let msg = ServerMessage::LoopRemoved {
            event_id: Uuid::nil(),
            request_id: "request-108".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 7,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "loop_removed");
        assert_eq!(json["deck"], "a");
        assert_eq!(json["revision"], 7);
    }

    #[test]
    fn deserializes_transport_request_cue_release() {
        let json = r#"{"type":"transport_request","request_id":"request-105","deck":"a","action":"cue_release"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::TransportRequest { request_id, action, .. } => {
                assert_eq!(request_id, "request-105");
                assert_eq!(action, TransportAction::CueRelease);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn serializes_transport_event_seek() {
        let msg = ServerMessage::TransportEvent {
            event_id: Uuid::nil(),
            request_id: "request-101".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 5,
            action: TransportAction::Seek,
            effective_server_time_us: 48_375_000,
            position_us: 4_500_000,
            playback_rate: 1.0,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "transport_event");
        assert_eq!(json["action"], "seek");
        assert_eq!(json["position_us"], 4_500_000);
    }

    #[test]
    fn deserializes_state_request() {
        let json = r#"{"type":"state_request","request_id":"state-12"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        matches!(msg, ClientMessage::StateRequest { .. });
    }

    #[test]
    fn rejects_empty_request_id() {
        let json = r#"{"type":"state_request","request_id":""}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_err());
    }

    #[test]
    fn serializes_welcome() {
        let msg = ServerMessage::Welcome {
            connection_id: Uuid::nil(),
            server_time_us: 48_100_211,
            schedule_lead_time_us: 150_000,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "welcome");
        assert_eq!(json["server_time_us"], 48_100_211);
    }

    #[test]
    fn serializes_transport_event() {
        let msg = ServerMessage::TransportEvent {
            event_id: Uuid::nil(),
            request_id: "request-91".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 8,
            action: TransportAction::Play,
            effective_server_time_us: 48_375_000,
            position_us: 13_200_000,
            playback_rate: 1.0,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "transport_event");
        assert_eq!(json["deck"], "a");
        assert_eq!(json["action"], "play");
        assert_eq!(json["revision"], 8);
    }

    #[test]
    fn serializes_transport_event_for_deck_b() {
        let msg = ServerMessage::TransportEvent {
            event_id: Uuid::nil(),
            request_id: "request-91b".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::B,
            revision: 1,
            action: TransportAction::Play,
            effective_server_time_us: 48_375_000,
            position_us: 0,
            playback_rate: 1.0,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["deck"], "b");
    }

    #[test]
    fn serializes_transport_event_set_tempo() {
        let msg = ServerMessage::TransportEvent {
            event_id: Uuid::nil(),
            request_id: "request-96".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 4,
            action: TransportAction::SetTempo,
            effective_server_time_us: 60_000_000,
            position_us: 5_000_000,
            playback_rate: 1.03,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "transport_event");
        assert_eq!(json["action"], "set_tempo");
        assert_eq!(json["playback_rate"], 1.03);
    }

    #[test]
    fn serializes_state_snapshot() {
        let deck_dto = DeckStateDto {
            revision: 7,
            transport: TransportStateDto {
                playing: true,
                anchor_position_us: 12_000_000,
                anchor_server_time_us: 47_000_000,
                playback_rate: 1.0,
            },
            nudge_enabled: true,
            bass_cut_enabled: false,
            pitch_lock_enabled: true,
            pfl_enabled: false,
            cue_point_us: Some(6_000_000),
            loop_region: Some(LoopRegionDto { start_us: 6_000_000, end_us: 10_000_000, active: true }),
        };
        let msg = ServerMessage::StateSnapshot {
            server_time_us: 48_111_492,
            crossfader_revision: 3,
            crossfader_position: 0.5,
            crossfader_curve_shape: 0.5,
            deck_a: deck_dto.clone(),
            deck_b: DeckStateDto {
                revision: 0,
                transport: TransportStateDto {
                    playing: false,
                    anchor_position_us: 0,
                    anchor_server_time_us: 0,
                    playback_rate: 1.0,
                },
                nudge_enabled: true,
                bass_cut_enabled: false,
                pitch_lock_enabled: true,
                pfl_enabled: false,
                cue_point_us: None,
                loop_region: None,
            },
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "state_snapshot");
        assert_eq!(json["crossfader_revision"], 3);
        assert_eq!(json["crossfader_position"], 0.5);
        assert_eq!(json["crossfader_curve_shape"], 0.5);
        assert_eq!(json["deck_a"]["transport"]["playing"], true);
        assert_eq!(json["deck_a"]["nudge_enabled"], true);
        assert_eq!(json["deck_a"]["bass_cut_enabled"], false);
        assert_eq!(json["deck_a"]["pitch_lock_enabled"], true);
        assert_eq!(json["deck_a"]["cue_point_us"], 6_000_000);
        assert_eq!(json["deck_a"]["loop_region"]["start_us"], 6_000_000);
        assert_eq!(json["deck_a"]["loop_region"]["end_us"], 10_000_000);
        assert_eq!(json["deck_a"]["loop_region"]["active"], true);
        assert_eq!(json["deck_b"]["transport"]["playing"], false);
        assert!(json["deck_b"]["cue_point_us"].is_null());
    }

    #[test]
    fn deserializes_set_crossfader_position() {
        let json = r#"{"type":"set_crossfader_position","request_id":"request-110","position":0.25}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetCrossfaderPosition { request_id, position } => {
                assert_eq!(request_id, "request-110");
                assert_eq!(position, 0.25);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn rejects_crossfader_position_outside_bounds() {
        let json = r#"{"type":"set_crossfader_position","request_id":"r","position":1.5}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_err());

        let json = r#"{"type":"set_crossfader_position","request_id":"r","position":-0.1}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_err());
    }

    #[test]
    fn accepts_crossfader_position_at_the_bounds() {
        let json = r#"{"type":"set_crossfader_position","request_id":"r","position":0.0}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_ok());

        let json = r#"{"type":"set_crossfader_position","request_id":"r","position":1.0}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_ok());
    }

    #[test]
    fn serializes_crossfader_position_changed() {
        let msg = ServerMessage::CrossfaderPositionChanged {
            event_id: Uuid::nil(),
            request_id: "request-110".to_string(),
            origin_connection_id: Uuid::nil(),
            revision: 1,
            position: 0.25,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "crossfader_position_changed");
        assert_eq!(json["position"], 0.25);
        assert_eq!(json["revision"], 1);
    }

    #[test]
    fn deserializes_set_crossfader_curve() {
        let json = r#"{"type":"set_crossfader_curve","request_id":"request-111","shape":0.8}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetCrossfaderCurve { request_id, shape } => {
                assert_eq!(request_id, "request-111");
                assert_eq!(shape, 0.8);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn rejects_crossfader_curve_outside_bounds() {
        let json = r#"{"type":"set_crossfader_curve","request_id":"r","shape":1.1}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_err());
    }

    #[test]
    fn serializes_crossfader_curve_changed() {
        let msg = ServerMessage::CrossfaderCurveChanged {
            event_id: Uuid::nil(),
            request_id: "request-111".to_string(),
            origin_connection_id: Uuid::nil(),
            revision: 2,
            shape: 0.8,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "crossfader_curve_changed");
        assert_eq!(json["shape"], 0.8);
        assert_eq!(json["revision"], 2);
    }

    #[test]
    fn deserializes_set_nudge_enabled() {
        let json = r#"{"type":"set_nudge_enabled","request_id":"request-95","deck":"a","enabled":false}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetNudgeEnabled { request_id, enabled, .. } => {
                assert_eq!(request_id, "request-95");
                assert!(!enabled);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserializes_set_tempo_request() {
        let json = r#"{"type":"set_tempo_request","request_id":"request-96","deck":"a","playback_rate":1.03}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetTempoRequest { request_id, playback_rate, .. } => {
                assert_eq!(request_id, "request-96");
                assert_eq!(playback_rate, 1.03);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn accepts_playback_rate_at_the_bounds() {
        let json = format!(
            r#"{{"type":"set_tempo_request","request_id":"r","deck":"a","playback_rate":{MIN_PLAYBACK_RATE}}}"#
        );
        let msg: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(msg.validate().is_ok());

        let json = format!(
            r#"{{"type":"set_tempo_request","request_id":"r","deck":"a","playback_rate":{MAX_PLAYBACK_RATE}}}"#
        );
        let msg: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(msg.validate().is_ok());
    }

    #[test]
    fn rejects_playback_rate_outside_bounds() {
        let json = r#"{"type":"set_tempo_request","request_id":"r","deck":"a","playback_rate":1.5}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_err());

        let json = r#"{"type":"set_tempo_request","request_id":"r","deck":"a","playback_rate":0.5}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_err());
    }

    #[test]
    fn deserializes_set_bass_cut_enabled() {
        let json = r#"{"type":"set_bass_cut_enabled","request_id":"request-97","deck":"a","enabled":true}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetBassCutEnabled { request_id, enabled, .. } => {
                assert_eq!(request_id, "request-97");
                assert!(enabled);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn serializes_bass_cut_setting_changed() {
        let msg = ServerMessage::BassCutSettingChanged {
            event_id: Uuid::nil(),
            request_id: "request-97".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 5,
            enabled: true,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "bass_cut_setting_changed");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["revision"], 5);
    }

    #[test]
    fn deserializes_set_pitch_lock_enabled() {
        let json = r#"{"type":"set_pitch_lock_enabled","request_id":"request-98","deck":"a","enabled":false}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetPitchLockEnabled { request_id, enabled, .. } => {
                assert_eq!(request_id, "request-98");
                assert!(!enabled);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn serializes_pitch_lock_setting_changed() {
        let msg = ServerMessage::PitchLockSettingChanged {
            event_id: Uuid::nil(),
            request_id: "request-98".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 6,
            enabled: false,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "pitch_lock_setting_changed");
        assert_eq!(json["enabled"], false);
        assert_eq!(json["revision"], 6);
    }

    #[test]
    fn deserializes_set_pfl_enabled() {
        let json = r#"{"type":"set_pfl_enabled","request_id":"request-99","deck":"a","enabled":true}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetPflEnabled { request_id, enabled, .. } => {
                assert_eq!(request_id, "request-99");
                assert!(enabled);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn serializes_pfl_setting_changed() {
        let msg = ServerMessage::PflSettingChanged {
            event_id: Uuid::nil(),
            request_id: "request-99".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 9,
            enabled: true,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "pfl_setting_changed");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["revision"], 9);
    }

    #[test]
    fn serializes_nudge_setting_changed() {
        let msg = ServerMessage::NudgeSettingChanged {
            event_id: Uuid::nil(),
            request_id: "request-95".to_string(),
            origin_connection_id: Uuid::nil(),
            deck: DeckId::A,
            revision: 3,
            enabled: false,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "nudge_setting_changed");
        assert_eq!(json["enabled"], false);
        assert_eq!(json["revision"], 3);
    }

    #[test]
    fn serializes_error() {
        let msg = ServerMessage::Error {
            request_id: Some("request-93".to_string()),
            code: "invalid_message".to_string(),
            message: "Missing action field".to_string(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["code"], "invalid_message");
    }

    #[test]
    fn round_trips_clock_response() {
        let msg = ServerMessage::ClockResponse {
            request_id: "clock-42".to_string(),
            client_send_time_ms: 9382.450,
            server_receive_time_us: 48_200_110,
            server_send_time_us: 48_200_142,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "clock_response");
        assert_eq!(value["request_id"], "clock-42");
    }
}

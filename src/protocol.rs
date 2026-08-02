use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Playback rate is clamped to +/-6% of normal speed.
pub const MIN_PLAYBACK_RATE: f64 = 0.94;
pub const MAX_PLAYBACK_RATE: f64 = 1.06;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportAction {
    Play,
    Pause,
    /// Resets the playhead to the beginning of the track without changing
    /// whether it's playing or paused.
    Restart,
    /// Changes the canonical playback rate without changing whether it's
    /// playing or paused.
    SetTempo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransportStateDto {
    pub playing: bool,
    pub anchor_position_us: u64,
    pub anchor_server_time_us: u64,
    pub playback_rate: f64,
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
        action: TransportAction,
    },
    StateRequest {
        request_id: String,
    },
    /// Toggles the client-side tempo-nudge drift correction on or off,
    /// synced room-wide like any other setting - not part of the
    /// authoritative transport timeline, but shares its revision counter.
    SetNudgeEnabled {
        request_id: String,
        enabled: bool,
    },
    /// Sets the canonical playback rate, synced room-wide like play/pause.
    SetTempoRequest {
        request_id: String,
        playback_rate: f64,
    },
    /// Toggles a room-wide bass-cut effect (a highpass filter client-side),
    /// synced the same way as the nudge-enabled setting.
    SetBassCutEnabled {
        request_id: String,
        enabled: bool,
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
        revision: u64,
        transport: TransportStateDto,
        nudge_enabled: bool,
        bass_cut_enabled: bool,
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
        revision: u64,
        enabled: bool,
    },
    BassCutSettingChanged {
        event_id: Uuid,
        request_id: String,
        origin_connection_id: Uuid,
        revision: u64,
        enabled: bool,
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
        let json = r#"{"type":"transport_request","request_id":"request-91","action":"play"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::TransportRequest { request_id, action } => {
                assert_eq!(request_id, "request-91");
                assert_eq!(action, TransportAction::Play);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserializes_transport_request_restart() {
        let json = r#"{"type":"transport_request","request_id":"request-94","action":"restart"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::TransportRequest { request_id, action } => {
                assert_eq!(request_id, "request-94");
                assert_eq!(action, TransportAction::Restart);
            }
            _ => panic!("wrong variant"),
        }
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
            revision: 8,
            action: TransportAction::Play,
            effective_server_time_us: 48_375_000,
            position_us: 13_200_000,
            playback_rate: 1.0,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "transport_event");
        assert_eq!(json["action"], "play");
        assert_eq!(json["revision"], 8);
    }

    #[test]
    fn serializes_transport_event_set_tempo() {
        let msg = ServerMessage::TransportEvent {
            event_id: Uuid::nil(),
            request_id: "request-96".to_string(),
            origin_connection_id: Uuid::nil(),
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
        let msg = ServerMessage::StateSnapshot {
            server_time_us: 48_111_492,
            revision: 7,
            transport: TransportStateDto {
                playing: true,
                anchor_position_us: 12_000_000,
                anchor_server_time_us: 47_000_000,
                playback_rate: 1.0,
            },
            nudge_enabled: true,
            bass_cut_enabled: false,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "state_snapshot");
        assert_eq!(json["transport"]["playing"], true);
        assert_eq!(json["nudge_enabled"], true);
        assert_eq!(json["bass_cut_enabled"], false);
    }

    #[test]
    fn deserializes_set_nudge_enabled() {
        let json = r#"{"type":"set_nudge_enabled","request_id":"request-95","enabled":false}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetNudgeEnabled { request_id, enabled } => {
                assert_eq!(request_id, "request-95");
                assert!(!enabled);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn deserializes_set_tempo_request() {
        let json = r#"{"type":"set_tempo_request","request_id":"request-96","playback_rate":1.03}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetTempoRequest { request_id, playback_rate } => {
                assert_eq!(request_id, "request-96");
                assert_eq!(playback_rate, 1.03);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn accepts_playback_rate_at_the_bounds() {
        let json = format!(
            r#"{{"type":"set_tempo_request","request_id":"r","playback_rate":{MIN_PLAYBACK_RATE}}}"#
        );
        let msg: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(msg.validate().is_ok());

        let json = format!(
            r#"{{"type":"set_tempo_request","request_id":"r","playback_rate":{MAX_PLAYBACK_RATE}}}"#
        );
        let msg: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(msg.validate().is_ok());
    }

    #[test]
    fn rejects_playback_rate_outside_bounds() {
        let json = r#"{"type":"set_tempo_request","request_id":"r","playback_rate":1.5}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_err());

        let json = r#"{"type":"set_tempo_request","request_id":"r","playback_rate":0.5}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        assert!(msg.validate().is_err());
    }

    #[test]
    fn deserializes_set_bass_cut_enabled() {
        let json = r#"{"type":"set_bass_cut_enabled","request_id":"request-97","enabled":true}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::SetBassCutEnabled { request_id, enabled } => {
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
            revision: 5,
            enabled: true,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "bass_cut_setting_changed");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["revision"], 5);
    }

    #[test]
    fn serializes_nudge_setting_changed() {
        let msg = ServerMessage::NudgeSettingChanged {
            event_id: Uuid::nil(),
            request_id: "request-95".to_string(),
            origin_connection_id: Uuid::nil(),
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

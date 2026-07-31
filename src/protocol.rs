use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportAction {
    Play,
    Pause,
    /// Resets the playhead to the beginning of the track without changing
    /// whether it's playing or paused.
    Restart,
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
}

impl ClientMessage {
    /// Basic field validation beyond what serde already enforces via types.
    pub fn validate(&self) -> Result<(), String> {
        let request_id = match self {
            ClientMessage::ClockRequest { request_id, .. } => request_id,
            ClientMessage::TransportRequest { request_id, .. } => request_id,
            ClientMessage::StateRequest { request_id } => request_id,
        };
        if request_id.trim().is_empty() {
            return Err("request_id must not be empty".to_string());
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
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "state_snapshot");
        assert_eq!(json["transport"]["playing"], true);
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

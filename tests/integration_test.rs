use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use shared_audio_clock::{router, AppState};

async fn spawn_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = AppState::new(Duration::from_millis(150));
    let app = router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}/ws")
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn recv_json(ws: &mut WsStream) -> Value {
    loop {
        let msg = timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("timed out waiting for message")
            .expect("stream ended")
            .expect("websocket error");
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).expect("invalid json from server");
        }
    }
}

/// Reads messages until one with the given `type` field arrives, ignoring
/// any others (e.g. skipping a snapshot while waiting for a transport event).
async fn recv_until(ws: &mut WsStream, message_type: &str) -> Value {
    loop {
        let value = recv_json(ws).await;
        if value["type"] == message_type {
            return value;
        }
    }
}

async fn send_json(ws: &mut WsStream, value: Value) {
    ws.send(Message::Text(value.to_string())).await.unwrap();
}

#[tokio::test]
async fn two_clients_observe_the_same_canonical_transport_events() {
    let url = spawn_server().await;

    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Both clients get identified and see the initial (paused) state.
    let welcome_a = recv_until(&mut client_a, "welcome").await;
    assert!(welcome_a["connection_id"].is_string());
    assert_eq!(welcome_a["schedule_lead_time_us"], 150_000);

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_a["revision"], 0);
    assert_eq!(snapshot_a["transport"]["playing"], false);
    assert_eq!(snapshot_a["nudge_enabled"], true);

    let _welcome_b = recv_until(&mut client_b, "welcome").await;
    let snapshot_b = recv_until(&mut client_b, "state_snapshot").await;
    assert_eq!(snapshot_b["revision"], 0);

    // Client A requests play; both clients must receive the identical
    // canonical event (same event_id, effective time, and revision).
    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "request-91", "action": "play"}),
    )
    .await;

    let play_a = recv_until(&mut client_a, "transport_event").await;
    let play_b = recv_until(&mut client_b, "transport_event").await;

    assert_eq!(play_a["event_id"], play_b["event_id"]);
    assert_eq!(play_a["effective_server_time_us"], play_b["effective_server_time_us"]);
    assert_eq!(play_a["revision"], play_b["revision"]);
    assert_eq!(play_a["revision"], 1);
    assert_eq!(play_a["action"], "play");
    assert_eq!(play_a["position_us"], 0);
    assert_eq!(play_a["request_id"], "request-91");

    // Client B requests pause; both clients again converge on one event.
    send_json(
        &mut client_b,
        json!({"type": "transport_request", "request_id": "request-92", "action": "pause"}),
    )
    .await;

    let pause_a = recv_until(&mut client_a, "transport_event").await;
    let pause_b = recv_until(&mut client_b, "transport_event").await;

    assert_eq!(pause_a["event_id"], pause_b["event_id"]);
    assert_eq!(pause_a["position_us"], pause_b["position_us"]);
    assert_eq!(pause_a["action"], "pause");
    assert_eq!(pause_a["revision"], 2);
    assert!(pause_a["revision"].as_u64().unwrap() > play_a["revision"].as_u64().unwrap());

    // A fresh state request must reflect the final, paused, canonical state.
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-12"}),
    )
    .await;
    let final_snapshot = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(final_snapshot["revision"], 2);
    assert_eq!(final_snapshot["transport"]["playing"], false);
    assert_eq!(
        final_snapshot["transport"]["anchor_position_us"],
        pause_a["position_us"]
    );
    assert_eq!(
        final_snapshot["transport"]["anchor_server_time_us"],
        pause_a["effective_server_time_us"]
    );
}

#[tokio::test]
async fn clock_request_round_trips_the_opaque_client_timestamp() {
    let url = spawn_server().await;
    let (mut client, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let _welcome = recv_until(&mut client, "welcome").await;
    let _snapshot = recv_until(&mut client, "state_snapshot").await;

    send_json(
        &mut client,
        json!({"type": "clock_request", "request_id": "clock-42", "client_send_time_ms": 9382.45}),
    )
    .await;

    let response = recv_until(&mut client, "clock_response").await;
    assert_eq!(response["request_id"], "clock-42");
    assert_eq!(response["client_send_time_ms"], 9382.45);
    assert!(response["server_receive_time_us"].as_u64().unwrap() <= response["server_send_time_us"].as_u64().unwrap());
}

#[tokio::test]
async fn duplicate_request_id_is_not_applied_twice() {
    let url = spawn_server().await;
    let (mut client, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let _welcome = recv_until(&mut client, "welcome").await;
    let _snapshot = recv_until(&mut client, "state_snapshot").await;

    let play_request = json!({"type": "transport_request", "request_id": "dup-1", "action": "play"});
    send_json(&mut client, play_request.clone()).await;
    let first = recv_until(&mut client, "transport_event").await;
    assert_eq!(first["revision"], 1);

    send_json(&mut client, play_request).await;

    // No second transport_event should arrive for the duplicate; a
    // subsequent state_request should still report revision 1.
    send_json(
        &mut client,
        json!({"type": "state_request", "request_id": "state-1"}),
    )
    .await;
    let snapshot = recv_until(&mut client, "state_snapshot").await;
    assert_eq!(snapshot["revision"], 1);
}

#[tokio::test]
async fn restart_resets_position_but_keeps_playing_and_broadcasts_to_both_clients() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let _ = recv_until(&mut client_a, "state_snapshot").await;
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-play", "action": "play"}),
    )
    .await;
    let play_a = recv_until(&mut client_a, "transport_event").await;
    let _ = recv_until(&mut client_b, "transport_event").await;
    assert_eq!(play_a["revision"], 1);

    send_json(
        &mut client_b,
        json!({"type": "transport_request", "request_id": "req-restart", "action": "restart"}),
    )
    .await;

    let restart_a = recv_until(&mut client_a, "transport_event").await;
    let restart_b = recv_until(&mut client_b, "transport_event").await;

    assert_eq!(restart_a["event_id"], restart_b["event_id"]);
    assert_eq!(restart_a["action"], "restart");
    assert_eq!(restart_a["position_us"], 0);
    assert_eq!(restart_a["revision"], 2);

    // Restart must not have paused playback: a state request afterwards
    // should still report playing=true, just anchored back at position 0.
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-restart"}),
    )
    .await;
    let snapshot = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot["transport"]["playing"], true);
    assert_eq!(snapshot["transport"]["anchor_position_us"], 0);
}

#[tokio::test]
async fn nudge_setting_syncs_to_both_clients_and_is_idempotent() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_a["nudge_enabled"], true);
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    // Client A disables it; both clients must converge on the same event.
    send_json(
        &mut client_a,
        json!({"type": "set_nudge_enabled", "request_id": "req-nudge-off", "enabled": false}),
    )
    .await;

    let event_a = recv_until(&mut client_a, "nudge_setting_changed").await;
    let event_b = recv_until(&mut client_b, "nudge_setting_changed").await;

    assert_eq!(event_a["event_id"], event_b["event_id"]);
    assert_eq!(event_a["enabled"], false);
    assert_eq!(event_a["revision"], 1);
    assert_eq!(event_a["request_id"], "req-nudge-off");

    // A fresh snapshot must reflect the change.
    send_json(
        &mut client_b,
        json!({"type": "state_request", "request_id": "state-after-nudge-off"}),
    )
    .await;
    let snapshot = recv_until(&mut client_b, "state_snapshot").await;
    assert_eq!(snapshot["nudge_enabled"], false);
    assert_eq!(snapshot["revision"], 1);

    // Repeating the same value is idempotent: no new event, no revision bump.
    send_json(
        &mut client_a,
        json!({"type": "set_nudge_enabled", "request_id": "req-nudge-off-again", "enabled": false}),
    )
    .await;
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-idempotent-toggle"}),
    )
    .await;
    let snapshot_after = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_after["revision"], 1);
}

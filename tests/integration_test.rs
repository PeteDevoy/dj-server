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
    assert_eq!(snapshot_a["deck_a"]["revision"], 0);
    assert_eq!(snapshot_a["deck_a"]["transport"]["playing"], false);
    assert_eq!(snapshot_a["deck_a"]["nudge_enabled"], true);

    let _welcome_b = recv_until(&mut client_b, "welcome").await;
    let snapshot_b = recv_until(&mut client_b, "state_snapshot").await;
    assert_eq!(snapshot_b["deck_a"]["revision"], 0);

    // Client A requests play; both clients must receive the identical
    // canonical event (same event_id, effective time, and revision).
    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "request-91", "deck": "a", "action": "play"}),
    )
    .await;

    let play_a = recv_until(&mut client_a, "transport_event").await;
    let play_b = recv_until(&mut client_b, "transport_event").await;

    assert_eq!(play_a["event_id"], play_b["event_id"]);
    assert_eq!(play_a["deck"], "a");
    assert_eq!(play_a["effective_server_time_us"], play_b["effective_server_time_us"]);
    assert_eq!(play_a["revision"], play_b["revision"]);
    assert_eq!(play_a["revision"], 1);
    assert_eq!(play_a["action"], "play");
    assert_eq!(play_a["position_us"], 0);
    assert_eq!(play_a["request_id"], "request-91");

    // Client B requests pause; both clients again converge on one event.
    send_json(
        &mut client_b,
        json!({"type": "transport_request", "request_id": "request-92", "deck": "a", "action": "pause"}),
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
    assert_eq!(final_snapshot["deck_a"]["revision"], 2);
    assert_eq!(final_snapshot["deck_a"]["transport"]["playing"], false);
    assert_eq!(
        final_snapshot["deck_a"]["transport"]["anchor_position_us"],
        pause_a["position_us"]
    );
    assert_eq!(
        final_snapshot["deck_a"]["transport"]["anchor_server_time_us"],
        pause_a["effective_server_time_us"]
    );
}

#[tokio::test]
async fn deck_a_and_deck_b_are_fully_independent() {
    // The core claim of the two-deck refactor: an action on one deck must
    // never be visible on the other, and each has its own revision counter.
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _ = recv_until(&mut client_a, "state_snapshot").await;

    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-play-a", "deck": "a", "action": "play"}),
    )
    .await;
    let play_a = recv_until(&mut client_a, "transport_event").await;
    assert_eq!(play_a["deck"], "a");
    assert_eq!(play_a["revision"], 1);

    send_json(
        &mut client_a,
        json!({"type": "seek_request", "request_id": "req-seek-a", "deck": "a", "position_us": 4_500_000}),
    )
    .await;
    let seek_a = recv_until(&mut client_a, "transport_event").await;
    assert_eq!(seek_a["deck"], "a");
    assert_eq!(seek_a["revision"], 2);

    // Deck B must be completely untouched: still paused, position 0,
    // revision 0 - despite deck A having reached revision 2.
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-check"}),
    )
    .await;
    let snapshot = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["revision"], 2);
    assert_eq!(snapshot["deck_a"]["transport"]["playing"], true);
    assert_eq!(snapshot["deck_a"]["transport"]["anchor_position_us"], 4_500_000);
    assert_eq!(snapshot["deck_b"]["revision"], 0);
    assert_eq!(snapshot["deck_b"]["transport"]["playing"], false);
    assert_eq!(snapshot["deck_b"]["transport"]["anchor_position_us"], 0);

    // Now drive deck B independently and confirm deck A stays exactly where it was.
    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-play-b", "deck": "b", "action": "play"}),
    )
    .await;
    let play_b = recv_until(&mut client_a, "transport_event").await;
    assert_eq!(play_b["deck"], "b");
    assert_eq!(play_b["revision"], 1); // deck B's OWN revision counter, independent of deck A's

    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-check-2"}),
    )
    .await;
    let snapshot = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["revision"], 2); // unchanged
    assert_eq!(snapshot["deck_a"]["transport"]["anchor_position_us"], 4_500_000); // unchanged
    assert_eq!(snapshot["deck_b"]["revision"], 1);
    assert_eq!(snapshot["deck_b"]["transport"]["playing"], true);
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

    let play_request = json!({"type": "transport_request", "request_id": "dup-1", "deck": "a", "action": "play"});
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
    assert_eq!(snapshot["deck_a"]["revision"], 1);
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
        json!({"type": "transport_request", "request_id": "req-play", "deck": "a", "action": "play"}),
    )
    .await;
    let play_a = recv_until(&mut client_a, "transport_event").await;
    let _ = recv_until(&mut client_b, "transport_event").await;
    assert_eq!(play_a["revision"], 1);

    send_json(
        &mut client_b,
        json!({"type": "transport_request", "request_id": "req-restart", "deck": "a", "action": "restart"}),
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
    assert_eq!(snapshot["deck_a"]["transport"]["playing"], true);
    assert_eq!(snapshot["deck_a"]["transport"]["anchor_position_us"], 0);
}

#[tokio::test]
async fn seek_jumps_to_target_position_and_broadcasts_to_both_clients() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let _ = recv_until(&mut client_a, "state_snapshot").await;
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-play", "deck": "a", "action": "play"}),
    )
    .await;
    let play_a = recv_until(&mut client_a, "transport_event").await;
    let _ = recv_until(&mut client_b, "transport_event").await;
    assert_eq!(play_a["revision"], 1);

    send_json(
        &mut client_b,
        json!({"type": "seek_request", "request_id": "req-seek", "deck": "a", "position_us": 4_500_000}),
    )
    .await;

    let seek_a = recv_until(&mut client_a, "transport_event").await;
    let seek_b = recv_until(&mut client_b, "transport_event").await;

    assert_eq!(seek_a["event_id"], seek_b["event_id"]);
    assert_eq!(seek_a["action"], "seek");
    assert_eq!(seek_a["position_us"], 4_500_000);
    assert_eq!(seek_a["revision"], 2);

    // Seeking must not have paused playback, same as restart.
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-seek"}),
    )
    .await;
    let snapshot = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["transport"]["playing"], true);
    assert_eq!(snapshot["deck_a"]["transport"]["anchor_position_us"], 4_500_000);
}

#[tokio::test]
async fn cue_point_set_and_released_syncs_to_both_clients() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert!(snapshot_a["deck_a"]["cue_point_us"].is_null());
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-play", "deck": "a", "action": "play"}),
    )
    .await;
    let _ = recv_until(&mut client_a, "transport_event").await;
    let _ = recv_until(&mut client_b, "transport_event").await;

    // B sets a cue point while A never touches anything.
    send_json(
        &mut client_b,
        json!({"type": "set_cue_point", "request_id": "req-cue-set", "deck": "a", "position_us": 6_000_000}),
    )
    .await;
    let cue_a = recv_until(&mut client_a, "cue_point_changed").await;
    let cue_b = recv_until(&mut client_b, "cue_point_changed").await;
    assert_eq!(cue_a["event_id"], cue_b["event_id"]);
    assert_eq!(cue_a["position_us"], 6_000_000);
    assert_eq!(cue_a["revision"], 2);

    // Both clients agree on the cue point via a fresh snapshot.
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-cue-set"}),
    )
    .await;
    let snapshot = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["cue_point_us"], 6_000_000);
    assert_eq!(snapshot["deck_a"]["transport"]["playing"], true); // setting a cue point must not affect playback

    // A releases the cue (still playing) - transport must pause exactly at
    // the cue point, broadcast to both, without either client having to
    // supply the position themselves.
    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-cue-release", "deck": "a", "action": "cue_release"}),
    )
    .await;
    let release_a = recv_until(&mut client_a, "transport_event").await;
    let release_b = recv_until(&mut client_b, "transport_event").await;
    assert_eq!(release_a["event_id"], release_b["event_id"]);
    assert_eq!(release_a["action"], "cue_release");
    assert_eq!(release_a["position_us"], 6_000_000);
    assert_eq!(release_a["revision"], 3);

    send_json(
        &mut client_b,
        json!({"type": "state_request", "request_id": "state-after-cue-release"}),
    )
    .await;
    let snapshot = recv_until(&mut client_b, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["transport"]["playing"], false);
    assert_eq!(snapshot["deck_a"]["transport"]["anchor_position_us"], 6_000_000);
}

#[tokio::test]
async fn set_loop_syncs_cue_point_and_loop_to_both_clients() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert!(snapshot_a["deck_a"]["loop_region"].is_null());
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    // A inserts a loop; B never touches anything.
    send_json(
        &mut client_a,
        json!({"type": "set_loop", "request_id": "req-loop-set", "deck": "a", "start_us": 1_000_000, "end_us": 3_000_000}),
    )
    .await;

    // Two broadcasts (cue point, then loop), same request, sequential revisions.
    let cue_a = recv_until(&mut client_a, "cue_point_changed").await;
    let cue_b = recv_until(&mut client_b, "cue_point_changed").await;
    assert_eq!(cue_a["event_id"], cue_b["event_id"]);
    assert_eq!(cue_a["position_us"], 1_000_000);
    assert_eq!(cue_a["revision"], 1);

    let loop_a = recv_until(&mut client_a, "loop_changed").await;
    let loop_b = recv_until(&mut client_b, "loop_changed").await;
    assert_eq!(loop_a["event_id"], loop_b["event_id"]);
    assert_eq!(loop_a["start_us"], 1_000_000);
    assert_eq!(loop_a["end_us"], 3_000_000);
    assert_eq!(loop_a["active"], true);
    assert_eq!(loop_a["revision"], 2);

    // B (which never touched anything) sees the same loop via a fresh snapshot.
    send_json(
        &mut client_b,
        json!({"type": "state_request", "request_id": "state-after-loop-set"}),
    )
    .await;
    let snapshot = recv_until(&mut client_b, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["loop_region"]["start_us"], 1_000_000);
    assert_eq!(snapshot["deck_a"]["loop_region"]["end_us"], 3_000_000);
    assert_eq!(snapshot["deck_a"]["loop_region"]["active"], true);
    assert_eq!(snapshot["deck_a"]["cue_point_us"], 1_000_000);

    // B deactivates the loop; A observes it without having touched anything.
    send_json(
        &mut client_b,
        json!({"type": "set_loop_active", "request_id": "req-loop-deactivate", "deck": "a", "active": false}),
    )
    .await;
    let deactivate_a = recv_until(&mut client_a, "loop_changed").await;
    let deactivate_b = recv_until(&mut client_b, "loop_changed").await;
    assert_eq!(deactivate_a["event_id"], deactivate_b["event_id"]);
    assert_eq!(deactivate_a["active"], false);
    assert_eq!(deactivate_a["start_us"], 1_000_000); // bounds unchanged
    assert_eq!(deactivate_a["end_us"], 3_000_000);
    assert_eq!(deactivate_a["revision"], 3);
}

#[tokio::test]
async fn set_loop_active_with_no_loop_is_silently_ignored() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _ = recv_until(&mut client_a, "state_snapshot").await;

    send_json(
        &mut client_a,
        json!({"type": "set_loop_active", "request_id": "req-no-loop", "deck": "a", "active": true}),
    )
    .await;

    // No loop_changed broadcast should follow - confirm by requesting a
    // snapshot instead and seeing it arrive without a loop_changed in between.
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-noop"}),
    )
    .await;
    let snapshot = recv_until(&mut client_a, "state_snapshot").await;
    assert!(snapshot["deck_a"]["loop_region"].is_null());
    assert_eq!(snapshot["deck_a"]["revision"], 0); // untouched
}

#[tokio::test]
async fn nudge_setting_syncs_to_both_clients_and_is_idempotent() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_a["deck_a"]["nudge_enabled"], true);
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    // Client A disables it; both clients must converge on the same event.
    send_json(
        &mut client_a,
        json!({"type": "set_nudge_enabled", "request_id": "req-nudge-off", "deck": "a", "enabled": false}),
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
    assert_eq!(snapshot["deck_a"]["nudge_enabled"], false);
    assert_eq!(snapshot["deck_a"]["revision"], 1);

    // Repeating the same value is idempotent: no new event, no revision bump.
    send_json(
        &mut client_a,
        json!({"type": "set_nudge_enabled", "request_id": "req-nudge-off-again", "deck": "a", "enabled": false}),
    )
    .await;
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-idempotent-toggle"}),
    )
    .await;
    let snapshot_after = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_after["deck_a"]["revision"], 1);
}

#[tokio::test]
async fn bass_cut_setting_syncs_to_both_clients_and_is_idempotent() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_a["deck_a"]["bass_cut_enabled"], false);
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    // Client A enables it; both clients must converge on the same event.
    send_json(
        &mut client_a,
        json!({"type": "set_bass_cut_enabled", "request_id": "req-bass-on", "deck": "a", "enabled": true}),
    )
    .await;

    let event_a = recv_until(&mut client_a, "bass_cut_setting_changed").await;
    let event_b = recv_until(&mut client_b, "bass_cut_setting_changed").await;

    assert_eq!(event_a["event_id"], event_b["event_id"]);
    assert_eq!(event_a["enabled"], true);
    assert_eq!(event_a["revision"], 1);
    assert_eq!(event_a["request_id"], "req-bass-on");

    // A fresh snapshot must reflect the change.
    send_json(
        &mut client_b,
        json!({"type": "state_request", "request_id": "state-after-bass-on"}),
    )
    .await;
    let snapshot = recv_until(&mut client_b, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["bass_cut_enabled"], true);
    assert_eq!(snapshot["deck_a"]["revision"], 1);

    // Repeating the same value is idempotent: no new event, no revision bump.
    send_json(
        &mut client_a,
        json!({"type": "set_bass_cut_enabled", "request_id": "req-bass-on-again", "deck": "a", "enabled": true}),
    )
    .await;
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-idempotent-bass-toggle"}),
    )
    .await;
    let snapshot_after = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_after["deck_a"]["revision"], 1);
}

#[tokio::test]
async fn pitch_lock_setting_syncs_to_both_clients_and_is_idempotent() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_a["deck_a"]["pitch_lock_enabled"], true);
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    // Client A disables it; both clients must converge on the same event.
    send_json(
        &mut client_a,
        json!({"type": "set_pitch_lock_enabled", "request_id": "req-pitch-lock-off", "deck": "a", "enabled": false}),
    )
    .await;

    let event_a = recv_until(&mut client_a, "pitch_lock_setting_changed").await;
    let event_b = recv_until(&mut client_b, "pitch_lock_setting_changed").await;

    assert_eq!(event_a["event_id"], event_b["event_id"]);
    assert_eq!(event_a["enabled"], false);
    assert_eq!(event_a["revision"], 1);
    assert_eq!(event_a["request_id"], "req-pitch-lock-off");

    // A fresh snapshot must reflect the change.
    send_json(
        &mut client_b,
        json!({"type": "state_request", "request_id": "state-after-pitch-lock-off"}),
    )
    .await;
    let snapshot = recv_until(&mut client_b, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["pitch_lock_enabled"], false);
    assert_eq!(snapshot["deck_a"]["revision"], 1);

    // Repeating the same value is idempotent: no new event, no revision bump.
    send_json(
        &mut client_a,
        json!({"type": "set_pitch_lock_enabled", "request_id": "req-pitch-lock-off-again", "deck": "a", "enabled": false}),
    )
    .await;
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-idempotent-pitch-lock-toggle"}),
    )
    .await;
    let snapshot_after = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_after["deck_a"]["revision"], 1);
}

#[tokio::test]
async fn tempo_change_syncs_to_both_clients_and_carries_into_next_play() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let _ = recv_until(&mut client_a, "state_snapshot").await;
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    // Client A raises tempo to +6%; both clients converge on one event.
    send_json(
        &mut client_a,
        json!({"type": "set_tempo_request", "request_id": "req-tempo-fast", "deck": "a", "playback_rate": 1.06}),
    )
    .await;

    let event_a = recv_until(&mut client_a, "transport_event").await;
    let event_b = recv_until(&mut client_b, "transport_event").await;

    assert_eq!(event_a["event_id"], event_b["event_id"]);
    assert_eq!(event_a["action"], "set_tempo");
    assert_eq!(event_a["playback_rate"], 1.06);
    assert_eq!(event_a["revision"], 1);

    // An out-of-range request must be rejected with an error, not applied.
    send_json(
        &mut client_b,
        json!({"type": "set_tempo_request", "request_id": "req-tempo-bad", "deck": "a", "playback_rate": 2.0}),
    )
    .await;
    let error = recv_until(&mut client_b, "error").await;
    assert_eq!(error["request_id"], "req-tempo-bad");

    // The rejected request must not have bumped the revision or changed the rate.
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-bad-tempo"}),
    )
    .await;
    let snapshot = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["revision"], 1);
    assert_eq!(snapshot["deck_a"]["transport"]["playback_rate"], 1.06);

    // A subsequent play carries the new tempo forward.
    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-play-after-tempo", "deck": "a", "action": "play"}),
    )
    .await;
    let play_event = recv_until(&mut client_a, "transport_event").await;
    assert_eq!(play_event["action"], "play");
    assert_eq!(play_event["playback_rate"], 1.06);
}

#[tokio::test]
async fn rapid_tempo_samples_each_get_a_distinct_ordered_broadcast() {
    // Simulates a fader drag: many quick tempo samples in succession. Every
    // one must be individually broadcast (not merged/coalesced) with a
    // strictly increasing revision, and applied immediately (no lead time) -
    // this is the server-side half of the sample+interpolate design; the
    // client is what buffers/smooths these for display.
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _ = recv_until(&mut client_a, "state_snapshot").await;
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    // Starts above 1.0 since the room's default rate already *is* 1.0 -
    // a sample equal to the current rate is treated as idempotent (no new
    // revision), same as schedule_play being a no-op while already playing.
    let samples = [1.01, 1.02, 1.03, 1.04, 1.05];
    for (i, rate) in samples.iter().enumerate() {
        send_json(
            &mut client_a,
            json!({"type": "set_tempo_request", "request_id": format!("drag-{i}"), "deck": "a", "playback_rate": rate}),
        )
        .await;
    }

    let mut last_revision = 0;
    for expected_rate in samples {
        let event_a = recv_until(&mut client_a, "transport_event").await;
        let event_b = recv_until(&mut client_b, "transport_event").await;
        assert_eq!(event_a["event_id"], event_b["event_id"]);
        assert_eq!(event_a["playback_rate"], expected_rate);
        let revision = event_a["revision"].as_u64().unwrap();
        assert!(revision > last_revision, "revisions must strictly increase");
        last_revision = revision;
    }

    // Immediate application: effective time tracks receipt, not +150ms.
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-drag"}),
    )
    .await;
    let snapshot = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot["deck_a"]["transport"]["playback_rate"], 1.05);
}

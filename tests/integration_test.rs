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
async fn seek_jumps_to_target_position_and_broadcasts_to_both_clients() {
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
        json!({"type": "seek_request", "request_id": "req-seek", "position_us": 4_500_000}),
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
    assert_eq!(snapshot["transport"]["playing"], true);
    assert_eq!(snapshot["transport"]["anchor_position_us"], 4_500_000);
}

#[tokio::test]
async fn cue_point_set_and_released_syncs_to_both_clients() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_a["cue_point_us"], Value::Null);
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-play", "action": "play"}),
    )
    .await;
    let _ = recv_until(&mut client_a, "transport_event").await;
    let _ = recv_until(&mut client_b, "transport_event").await;

    // B sets a cue point while A never touches anything.
    send_json(
        &mut client_b,
        json!({"type": "set_cue_point", "request_id": "req-cue-set", "position_us": 6_000_000}),
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
    assert_eq!(snapshot["cue_point_us"], 6_000_000);
    assert_eq!(snapshot["transport"]["playing"], true); // setting a cue point must not affect playback

    // A releases the cue (still playing) - transport must pause exactly at
    // the cue point, broadcast to both, without either client having to
    // supply the position themselves.
    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-cue-release", "action": "cue_release"}),
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
    assert_eq!(snapshot["transport"]["playing"], false);
    assert_eq!(snapshot["transport"]["anchor_position_us"], 6_000_000);
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

#[tokio::test]
async fn bass_cut_setting_syncs_to_both_clients_and_is_idempotent() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_a["bass_cut_enabled"], false);
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    // Client A enables it; both clients must converge on the same event.
    send_json(
        &mut client_a,
        json!({"type": "set_bass_cut_enabled", "request_id": "req-bass-on", "enabled": true}),
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
    assert_eq!(snapshot["bass_cut_enabled"], true);
    assert_eq!(snapshot["revision"], 1);

    // Repeating the same value is idempotent: no new event, no revision bump.
    send_json(
        &mut client_a,
        json!({"type": "set_bass_cut_enabled", "request_id": "req-bass-on-again", "enabled": true}),
    )
    .await;
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-idempotent-bass-toggle"}),
    )
    .await;
    let snapshot_after = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_after["revision"], 1);
}

#[tokio::test]
async fn pitch_lock_setting_syncs_to_both_clients_and_is_idempotent() {
    let url = spawn_server().await;
    let (mut client_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut client_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let snapshot_a = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_a["pitch_lock_enabled"], true);
    let _ = recv_until(&mut client_b, "state_snapshot").await;

    // Client A disables it; both clients must converge on the same event.
    send_json(
        &mut client_a,
        json!({"type": "set_pitch_lock_enabled", "request_id": "req-pitch-lock-off", "enabled": false}),
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
    assert_eq!(snapshot["pitch_lock_enabled"], false);
    assert_eq!(snapshot["revision"], 1);

    // Repeating the same value is idempotent: no new event, no revision bump.
    send_json(
        &mut client_a,
        json!({"type": "set_pitch_lock_enabled", "request_id": "req-pitch-lock-off-again", "enabled": false}),
    )
    .await;
    send_json(
        &mut client_a,
        json!({"type": "state_request", "request_id": "state-after-idempotent-pitch-lock-toggle"}),
    )
    .await;
    let snapshot_after = recv_until(&mut client_a, "state_snapshot").await;
    assert_eq!(snapshot_after["revision"], 1);
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
        json!({"type": "set_tempo_request", "request_id": "req-tempo-fast", "playback_rate": 1.06}),
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
        json!({"type": "set_tempo_request", "request_id": "req-tempo-bad", "playback_rate": 2.0}),
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
    assert_eq!(snapshot["revision"], 1);
    assert_eq!(snapshot["transport"]["playback_rate"], 1.06);

    // A subsequent play carries the new tempo forward.
    send_json(
        &mut client_a,
        json!({"type": "transport_request", "request_id": "req-play-after-tempo", "action": "play"}),
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
            json!({"type": "set_tempo_request", "request_id": format!("drag-{i}"), "playback_rate": rate}),
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
    assert_eq!(snapshot["transport"]["playback_rate"], 1.05);
}

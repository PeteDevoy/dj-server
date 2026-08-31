use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures::{sink::SinkExt, stream::StreamExt};
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::clock::Clock;
use crate::protocol::{ClientMessage, DeckId, DeckStateDto, LoopRegionDto, ServerMessage};
use crate::room::{DeckState, RoomState};

/// Bound on how many recent request IDs are remembered for deduplication.
const DEDUP_CACHE_CAPACITY: usize = 256;

/// Bound on the per-connection outbound channel. Generous enough to absorb
/// bursts without ever blocking the room lock on a slow socket.
const OUTBOUND_CHANNEL_CAPACITY: usize = 64;

/// Tempo requests apply with no lead time, unlike play/pause/restart.
/// The schedule-ahead convention exists to let clients prepare for a rare,
/// discrete, precisely-synchronized moment; tempo is now a continuous stream
/// of frequent samples (client throttles sends to ~25-50ms while the fader
/// moves, plus a forced final send on release), so hiding latency is instead
/// the receiving client's job via a buffered/interpolated render (see
/// `applyTempoSample` and the render loop in public/index.html). Scheduling
/// each sample 150ms out would just make the audio perpetually chase a
/// stale target.
const TEMPO_SAMPLE_LEAD_TIME_US: u64 = 0;

#[derive(Default)]
struct SeenRequests {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl SeenRequests {
    /// Returns true if `request_id` had already been recorded (i.e. this is
    /// a duplicate that should not be applied again).
    fn check_and_record(&mut self, request_id: &str) -> bool {
        if self.set.contains(request_id) {
            return true;
        }
        if self.order.len() >= DEDUP_CACHE_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
        self.order.push_back(request_id.to_string());
        self.set.insert(request_id.to_string());
        false
    }
}

pub struct AppState {
    pub clock: Clock,
    pub room: Arc<Mutex<RoomState>>,
    pub events: broadcast::Sender<ServerMessage>,
    pub schedule_lead_time_us: u64,
    pub connection_count: Arc<AtomicUsize>,
    seen_requests: Arc<Mutex<SeenRequests>>,
}

impl AppState {
    pub fn new(schedule_lead_time: Duration) -> Arc<Self> {
        let (events, _rx) = broadcast::channel(128);
        Arc::new(Self {
            clock: Clock::new(),
            room: Arc::new(Mutex::new(RoomState::new())),
            events,
            schedule_lead_time_us: schedule_lead_time.as_micros() as u64,
            connection_count: Arc::new(AtomicUsize::new(0)),
            seen_requests: Arc::new(Mutex::new(SeenRequests::default())),
        })
    }
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let connection_id = Uuid::new_v4();
    let connected = state.connection_count.fetch_add(1, Ordering::SeqCst) + 1;
    info!(%connection_id, connected_client_count = connected, "client connected");

    let (mut ws_sink, mut ws_stream) = socket.split();
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<ServerMessage>(OUTBOUND_CHANNEL_CAPACITY);
    let mut broadcast_rx = state.events.subscribe();

    // Writer task: owns the sink exclusively, forwarding both direct
    // responses and broadcast events. Never awaits while holding the room
    // lock - it doesn't touch the lock at all.
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                direct = outbound_rx.recv() => {
                    match direct {
                        Some(msg) => {
                            if send(&mut ws_sink, &msg).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                broadcast_msg = broadcast_rx.recv() => {
                    match broadcast_msg {
                        Ok(msg) => {
                            if send(&mut ws_sink, &msg).await.is_err() {
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(%connection_id, skipped, "connection lagged behind broadcast events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    });

    send_welcome_and_snapshot(&outbound_tx, &state, connection_id).await;

    while let Some(Ok(msg)) = ws_stream.next().await {
        let Message::Text(text) = msg else {
            continue;
        };
        handle_client_message(&text, &state, connection_id, &outbound_tx).await;
    }

    writer.abort();
    let connected = state.connection_count.fetch_sub(1, Ordering::SeqCst) - 1;
    info!(%connection_id, connected_client_count = connected, "client disconnected");
}

async fn send(sink: &mut (impl futures::Sink<Message> + Unpin), msg: &ServerMessage) -> Result<(), ()> {
    let text = match serde_json::to_string(msg) {
        Ok(text) => text,
        Err(err) => {
            warn!(%err, "failed to serialize outgoing message");
            return Err(());
        }
    };
    sink.send(Message::Text(text)).await.map_err(|_| ())
}

async fn send_welcome_and_snapshot(
    outbound_tx: &mpsc::Sender<ServerMessage>,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    let welcome = ServerMessage::Welcome {
        connection_id,
        server_time_us: state.clock.now_us(),
        schedule_lead_time_us: state.schedule_lead_time_us,
    };
    let _ = outbound_tx.send(welcome).await;

    let snapshot = build_snapshot(state).await;
    let _ = outbound_tx.send(snapshot).await;
}

fn deck_state_dto(deck: &DeckState) -> DeckStateDto {
    DeckStateDto {
        revision: deck.revision,
        transport: deck.transport.to_dto(),
        nudge_enabled: deck.nudge_enabled,
        bass_cut_enabled: deck.bass_cut_enabled,
        pitch_lock_enabled: deck.pitch_lock_enabled,
        pfl_enabled: deck.pfl_enabled,
        cue_point_us: deck.cue_point_us,
        loop_region: deck
            .loop_region
            .map(|l| LoopRegionDto { start_us: l.start_us, end_us: l.end_us, active: l.active }),
    }
}

async fn build_snapshot(state: &Arc<AppState>) -> ServerMessage {
    let room = state.room.lock().await;
    ServerMessage::StateSnapshot {
        server_time_us: state.clock.now_us(),
        crossfader_revision: room.crossfader_revision,
        crossfader_position: room.crossfader_position,
        crossfader_curve_shape: room.crossfader_curve_shape,
        deck_a: deck_state_dto(&room.deck_a),
        deck_b: deck_state_dto(&room.deck_b),
    }
}

async fn handle_client_message(
    text: &str,
    state: &Arc<AppState>,
    connection_id: Uuid,
    outbound_tx: &mpsc::Sender<ServerMessage>,
) {
    let parsed: Result<ClientMessage, _> = serde_json::from_str(text);
    let message = match parsed {
        Ok(message) => message,
        Err(err) => {
            let _ = outbound_tx
                .send(ServerMessage::Error {
                    request_id: None,
                    code: "invalid_message".to_string(),
                    message: format!("Could not parse message: {err}"),
                })
                .await;
            return;
        }
    };

    if let Err(reason) = message.validate() {
        let _ = outbound_tx
            .send(ServerMessage::Error {
                request_id: Some(message.request_id().to_string()),
                code: "invalid_message".to_string(),
                message: reason,
            })
            .await;
        return;
    }

    match message {
        ClientMessage::ClockRequest {
            request_id,
            client_send_time_ms,
        } => {
            let server_receive_time_us = state.clock.now_us();
            let server_send_time_us = state.clock.now_us();
            debug!(%request_id, %connection_id, server_receive_time_us, server_send_time_us, "clock request");
            let _ = outbound_tx
                .send(ServerMessage::ClockResponse {
                    request_id,
                    client_send_time_ms,
                    server_receive_time_us,
                    server_send_time_us,
                })
                .await;
        }
        ClientMessage::StateRequest { .. } => {
            let snapshot = build_snapshot(state).await;
            let _ = outbound_tx.send(snapshot).await;
        }
        ClientMessage::TransportRequest { request_id, deck, action } => {
            handle_transport_request(request_id, deck, action, state, connection_id, outbound_tx).await;
        }
        ClientMessage::SetNudgeEnabled { request_id, deck, enabled } => {
            handle_set_nudge_enabled(request_id, deck, enabled, state, connection_id).await;
        }
        ClientMessage::SetTempoRequest { request_id, deck, playback_rate } => {
            handle_set_tempo_request(request_id, deck, playback_rate, state, connection_id).await;
        }
        ClientMessage::SetBassCutEnabled { request_id, deck, enabled } => {
            handle_set_bass_cut_enabled(request_id, deck, enabled, state, connection_id).await;
        }
        ClientMessage::SetPitchLockEnabled { request_id, deck, enabled } => {
            handle_set_pitch_lock_enabled(request_id, deck, enabled, state, connection_id).await;
        }
        ClientMessage::SetPflEnabled { request_id, deck, enabled } => {
            handle_set_pfl_enabled(request_id, deck, enabled, state, connection_id).await;
        }
        ClientMessage::SeekRequest { request_id, deck, position_us } => {
            handle_seek_request(request_id, deck, position_us, state, connection_id).await;
        }
        ClientMessage::SetCuePoint { request_id, deck, position_us } => {
            handle_set_cue_point(request_id, deck, position_us, state, connection_id).await;
        }
        ClientMessage::RemoveCuePoint { request_id, deck } => {
            handle_remove_cue_point(request_id, deck, state, connection_id).await;
        }
        ClientMessage::SetLoop { request_id, deck, start_us, end_us } => {
            handle_set_loop(request_id, deck, start_us, end_us, state, connection_id).await;
        }
        ClientMessage::SetLoopActive { request_id, deck, active } => {
            handle_set_loop_active(request_id, deck, active, state, connection_id).await;
        }
        ClientMessage::RemoveLoop { request_id, deck } => {
            handle_remove_loop(request_id, deck, state, connection_id).await;
        }
        ClientMessage::SetCrossfaderPosition { request_id, position } => {
            handle_set_crossfader_position(request_id, position, state, connection_id).await;
        }
        ClientMessage::SetCrossfaderCurve { request_id, shape } => {
            handle_set_crossfader_curve(request_id, shape, state, connection_id).await;
        }
    }
}

async fn handle_transport_request(
    request_id: String,
    deck: DeckId,
    action: crate::protocol::TransportAction,
    state: &Arc<AppState>,
    connection_id: Uuid,
    outbound_tx: &mpsc::Sender<ServerMessage>,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate transport request ignored");
            return;
        }
    }

    let received_server_time_us = state.clock.now_us();
    let event_data = {
        let mut room = state.room.lock().await;
        let deck_state = room.deck_mut(deck);
        match action {
            crate::protocol::TransportAction::Play => {
                deck_state.schedule_play(received_server_time_us, state.schedule_lead_time_us)
            }
            crate::protocol::TransportAction::Pause => {
                deck_state.schedule_pause(received_server_time_us, state.schedule_lead_time_us)
            }
            crate::protocol::TransportAction::Restart => {
                deck_state.schedule_restart(received_server_time_us, state.schedule_lead_time_us)
            }
            crate::protocol::TransportAction::CueRelease => {
                deck_state.schedule_cue_release(received_server_time_us, state.schedule_lead_time_us)
            }
            crate::protocol::TransportAction::SetTempo => {
                drop(room);
                let _ = outbound_tx
                    .send(ServerMessage::Error {
                        request_id: Some(request_id.clone()),
                        code: "invalid_message".to_string(),
                        message: "set_tempo is not valid for transport_request; use set_tempo_request instead"
                            .to_string(),
                    })
                    .await;
                return;
            }
            crate::protocol::TransportAction::Seek => {
                drop(room);
                let _ = outbound_tx
                    .send(ServerMessage::Error {
                        request_id: Some(request_id.clone()),
                        code: "invalid_message".to_string(),
                        message: "seek is not valid for transport_request; use seek_request instead".to_string(),
                    })
                    .await;
                return;
            }
        }
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        action = ?action,
        received_server_time_us,
        effective_server_time_us = event_data.effective_server_time_us,
        revision = event_data.revision,
        position_us = event_data.position_us,
        connected_client_count,
        "transport request accepted"
    );

    let event = ServerMessage::TransportEvent {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: event_data.revision,
        action,
        effective_server_time_us: event_data.effective_server_time_us,
        position_us: event_data.position_us,
        playback_rate: event_data.playback_rate,
    };

    // Broadcasting can fail only when there are no subscribers left, which
    // is harmless here since the sender's own writer task is dropping too.
    let _ = state.events.send(event);
}

async fn handle_set_nudge_enabled(
    request_id: String,
    deck: DeckId,
    enabled: bool,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_nudge_enabled request ignored");
            return;
        }
    }

    let event_data = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck).set_nudge_enabled(enabled)
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        enabled,
        revision = event_data.revision,
        connected_client_count,
        "nudge setting change accepted"
    );

    let event = ServerMessage::NudgeSettingChanged {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: event_data.revision,
        enabled: event_data.enabled,
    };

    let _ = state.events.send(event);
}

async fn handle_set_tempo_request(
    request_id: String,
    deck: DeckId,
    playback_rate: f64,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_tempo request ignored");
            return;
        }
    }

    let received_server_time_us = state.clock.now_us();
    let event_data = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck)
            .schedule_playback_rate(received_server_time_us, TEMPO_SAMPLE_LEAD_TIME_US, playback_rate)
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        playback_rate,
        received_server_time_us,
        effective_server_time_us = event_data.effective_server_time_us,
        revision = event_data.revision,
        position_us = event_data.position_us,
        connected_client_count,
        "tempo change accepted"
    );

    let event = ServerMessage::TransportEvent {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: event_data.revision,
        action: event_data.action,
        effective_server_time_us: event_data.effective_server_time_us,
        position_us: event_data.position_us,
        playback_rate: event_data.playback_rate,
    };

    let _ = state.events.send(event);
}

async fn handle_seek_request(
    request_id: String,
    deck: DeckId,
    position_us: u64,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate seek request ignored");
            return;
        }
    }

    let received_server_time_us = state.clock.now_us();
    let event_data = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck)
            .schedule_seek(received_server_time_us, state.schedule_lead_time_us, position_us)
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        position_us,
        received_server_time_us,
        effective_server_time_us = event_data.effective_server_time_us,
        revision = event_data.revision,
        connected_client_count,
        "seek request accepted"
    );

    let event = ServerMessage::TransportEvent {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: event_data.revision,
        action: event_data.action,
        effective_server_time_us: event_data.effective_server_time_us,
        position_us: event_data.position_us,
        playback_rate: event_data.playback_rate,
    };

    let _ = state.events.send(event);
}

async fn handle_set_cue_point(
    request_id: String,
    deck: DeckId,
    position_us: u64,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_cue_point request ignored");
            return;
        }
    }

    let event_data = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck).set_cue_point(position_us)
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        position_us,
        revision = event_data.revision,
        connected_client_count,
        "cue point set"
    );

    let event = ServerMessage::CuePointChanged {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: event_data.revision,
        position_us: event_data.position_us,
    };

    let _ = state.events.send(event);
}

async fn handle_remove_cue_point(request_id: String, deck: DeckId, state: &Arc<AppState>, connection_id: Uuid) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate remove_cue_point request ignored");
            return;
        }
    }

    let revision = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck).remove_cue_point()
    };

    let Some(revision) = revision else {
        debug!(%request_id, %connection_id, ?deck, "remove_cue_point ignored: no cue point exists yet");
        return;
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        revision,
        connected_client_count,
        "cue point removed"
    );

    let event = ServerMessage::CuePointRemoved {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision,
    };

    let _ = state.events.send(event);
}

/// Inserts/overwrites a deck's loop, broadcasting two separate events
/// (CuePointChanged, then LoopChanged) since `DeckState::set_loop` bumps
/// the revision twice - one message can't carry two revision bumps without
/// the second failing every client's own staleness check.
async fn handle_set_loop(
    request_id: String,
    deck: DeckId,
    start_us: u64,
    end_us: u64,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_loop request ignored");
            return;
        }
    }

    let (cue_event, loop_event) = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck).set_loop(start_us, end_us)
    };

    let connected_client_count = state.connection_count.load(Ordering::SeqCst);

    let cue_event_id = Uuid::new_v4();
    info!(
        event_id = %cue_event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        position_us = cue_event.position_us,
        revision = cue_event.revision,
        connected_client_count,
        "cue point set (via set_loop)"
    );
    let _ = state.events.send(ServerMessage::CuePointChanged {
        event_id: cue_event_id,
        request_id: request_id.clone(),
        origin_connection_id: connection_id,
        deck,
        revision: cue_event.revision,
        position_us: cue_event.position_us,
    });

    let loop_event_id = Uuid::new_v4();
    info!(
        event_id = %loop_event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        start_us = loop_event.start_us,
        end_us = loop_event.end_us,
        active = loop_event.active,
        revision = loop_event.revision,
        connected_client_count,
        "loop inserted"
    );
    let _ = state.events.send(ServerMessage::LoopChanged {
        event_id: loop_event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: loop_event.revision,
        start_us: loop_event.start_us,
        end_us: loop_event.end_us,
        active: loop_event.active,
    });
}

async fn handle_set_loop_active(
    request_id: String,
    deck: DeckId,
    active: bool,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_loop_active request ignored");
            return;
        }
    }

    let event_data = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck).set_loop_active(active)
    };

    let Some(event_data) = event_data else {
        debug!(%request_id, %connection_id, ?deck, "set_loop_active ignored: no loop exists yet");
        return;
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        active,
        revision = event_data.revision,
        connected_client_count,
        "loop active toggled"
    );

    let event = ServerMessage::LoopChanged {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: event_data.revision,
        start_us: event_data.start_us,
        end_us: event_data.end_us,
        active: event_data.active,
    };

    let _ = state.events.send(event);
}

async fn handle_remove_loop(
    request_id: String,
    deck: DeckId,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate remove_loop request ignored");
            return;
        }
    }

    let revision = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck).remove_loop()
    };

    let Some(revision) = revision else {
        debug!(%request_id, %connection_id, ?deck, "remove_loop ignored: no loop exists yet");
        return;
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        revision,
        connected_client_count,
        "loop removed"
    );

    let event = ServerMessage::LoopRemoved {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision,
    };

    let _ = state.events.send(event);
}

async fn handle_set_crossfader_position(request_id: String, position: f64, state: &Arc<AppState>, connection_id: Uuid) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_crossfader_position request ignored");
            return;
        }
    }

    let event_data = {
        let mut room = state.room.lock().await;
        room.set_crossfader_position(position)
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        position,
        revision = event_data.revision,
        connected_client_count,
        "crossfader position change accepted"
    );

    let event = ServerMessage::CrossfaderPositionChanged {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        revision: event_data.revision,
        position: event_data.value,
    };

    let _ = state.events.send(event);
}

async fn handle_set_crossfader_curve(request_id: String, shape: f64, state: &Arc<AppState>, connection_id: Uuid) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_crossfader_curve request ignored");
            return;
        }
    }

    let event_data = {
        let mut room = state.room.lock().await;
        room.set_crossfader_curve_shape(shape)
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        shape,
        revision = event_data.revision,
        connected_client_count,
        "crossfader curve change accepted"
    );

    let event = ServerMessage::CrossfaderCurveChanged {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        revision: event_data.revision,
        shape: event_data.value,
    };

    let _ = state.events.send(event);
}

async fn handle_set_bass_cut_enabled(
    request_id: String,
    deck: DeckId,
    enabled: bool,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_bass_cut_enabled request ignored");
            return;
        }
    }

    let event_data = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck).set_bass_cut_enabled(enabled)
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        enabled,
        revision = event_data.revision,
        connected_client_count,
        "bass-cut setting change accepted"
    );

    let event = ServerMessage::BassCutSettingChanged {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: event_data.revision,
        enabled: event_data.enabled,
    };

    let _ = state.events.send(event);
}

async fn handle_set_pitch_lock_enabled(
    request_id: String,
    deck: DeckId,
    enabled: bool,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_pitch_lock_enabled request ignored");
            return;
        }
    }

    let event_data = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck).set_pitch_lock_enabled(enabled)
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        enabled,
        revision = event_data.revision,
        connected_client_count,
        "pitch-lock setting change accepted"
    );

    let event = ServerMessage::PitchLockSettingChanged {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: event_data.revision,
        enabled: event_data.enabled,
    };

    let _ = state.events.send(event);
}

async fn handle_set_pfl_enabled(
    request_id: String,
    deck: DeckId,
    enabled: bool,
    state: &Arc<AppState>,
    connection_id: Uuid,
) {
    {
        let mut seen = state.seen_requests.lock().await;
        if seen.check_and_record(&request_id) {
            debug!(%request_id, %connection_id, "duplicate set_pfl_enabled request ignored");
            return;
        }
    }

    let event_data = {
        let mut room = state.room.lock().await;
        room.deck_mut(deck).set_pfl_enabled(enabled)
    };

    let event_id = Uuid::new_v4();
    let connected_client_count = state.connection_count.load(Ordering::SeqCst);
    info!(
        %event_id,
        request_id = %request_id,
        %connection_id,
        ?deck,
        enabled,
        revision = event_data.revision,
        connected_client_count,
        "pfl setting change accepted"
    );

    let event = ServerMessage::PflSettingChanged {
        event_id,
        request_id,
        origin_connection_id: connection_id,
        deck,
        revision: event_data.revision,
        enabled: event_data.enabled,
    };

    let _ = state.events.send(event);
}

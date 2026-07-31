use uuid::Uuid;

use crate::clock::ServerTimeUs;
use crate::protocol::{TransportAction, TransportStateDto};

#[derive(Debug, Clone, PartialEq)]
pub struct TransportState {
    pub playing: bool,
    pub anchor_position_us: u64,
    pub anchor_server_time_us: u64,
    pub playback_rate: f64,
}

impl TransportState {
    pub fn initial() -> Self {
        Self {
            playing: false,
            anchor_position_us: 0,
            anchor_server_time_us: 0,
            playback_rate: 1.0,
        }
    }

    /// Derives the track position at `now_us` from the anchor. No running
    /// counter is kept - position is always computed on demand.
    pub fn position_at(&self, now_us: ServerTimeUs) -> u64 {
        if !self.playing {
            return self.anchor_position_us;
        }
        let elapsed_us = now_us.saturating_sub(self.anchor_server_time_us);
        let advanced_us = (elapsed_us as f64 * self.playback_rate) as u64;
        self.anchor_position_us + advanced_us
    }

    pub fn to_dto(&self) -> TransportStateDto {
        TransportStateDto {
            playing: self.playing,
            anchor_position_us: self.anchor_position_us,
            anchor_server_time_us: self.anchor_server_time_us,
            playback_rate: self.playback_rate,
        }
    }
}

/// The outcome of applying a scheduled transport transition, ready to be
/// turned into a canonical `ServerMessage::TransportEvent`.
#[derive(Debug, Clone, PartialEq)]
pub struct TransportEventData {
    pub action: TransportAction,
    pub effective_server_time_us: u64,
    pub position_us: u64,
    pub playback_rate: f64,
    pub revision: u64,
    /// True when this event reflects an already-applied state rather than a
    /// fresh transition (e.g. a duplicate play request while already playing).
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone)]
pub struct RoomState {
    pub transport: TransportState,
    pub revision: u64,
}

impl RoomState {
    pub fn new() -> Self {
        Self {
            transport: TransportState::initial(),
            revision: 0,
        }
    }

    pub fn current_position(&self, now_us: ServerTimeUs) -> u64 {
        self.transport.position_at(now_us)
    }

    /// Schedules a play transition `lead_time_us` in the future. If playback
    /// is already active, treats the request as idempotent and returns the
    /// existing anchor instead of restarting playback.
    pub fn schedule_play(&mut self, now_us: ServerTimeUs, lead_time_us: u64) -> TransportEventData {
        if self.transport.playing {
            return TransportEventData {
                action: TransportAction::Play,
                effective_server_time_us: self.transport.anchor_server_time_us,
                position_us: self.transport.anchor_position_us,
                playback_rate: self.transport.playback_rate,
                revision: self.revision,
                idempotent_replay: true,
            };
        }

        let effective_time_us = now_us + lead_time_us;
        let position_us = self.transport.position_at(now_us);

        self.transport.playing = true;
        self.transport.anchor_position_us = position_us;
        self.transport.anchor_server_time_us = effective_time_us;
        self.revision += 1;

        TransportEventData {
            action: TransportAction::Play,
            effective_server_time_us: effective_time_us,
            position_us,
            playback_rate: self.transport.playback_rate,
            revision: self.revision,
            idempotent_replay: false,
        }
    }

    /// Schedules a pause transition `lead_time_us` in the future. Playback
    /// continues until the effective time, then the anchor freezes there.
    pub fn schedule_pause(&mut self, now_us: ServerTimeUs, lead_time_us: u64) -> TransportEventData {
        let effective_time_us = now_us + lead_time_us;
        let position_us = self.transport.position_at(effective_time_us);

        self.transport.playing = false;
        self.transport.anchor_position_us = position_us;
        self.transport.anchor_server_time_us = effective_time_us;
        self.revision += 1;

        TransportEventData {
            action: TransportAction::Pause,
            effective_server_time_us: effective_time_us,
            position_us,
            playback_rate: self.transport.playback_rate,
            revision: self.revision,
            idempotent_replay: false,
        }
    }

    /// Schedules a restart `lead_time_us` in the future: resets the playhead
    /// to the beginning of the track without changing whether it's playing
    /// or paused. Mirrors `schedule_pause`'s convention of committing the
    /// post-transition anchor immediately rather than tracking a pending one.
    pub fn schedule_restart(&mut self, now_us: ServerTimeUs, lead_time_us: u64) -> TransportEventData {
        let effective_time_us = now_us + lead_time_us;

        self.transport.anchor_position_us = 0;
        self.transport.anchor_server_time_us = effective_time_us;
        self.revision += 1;

        TransportEventData {
            action: TransportAction::Restart,
            effective_server_time_us: effective_time_us,
            position_us: 0,
            playback_rate: self.transport.playback_rate,
            revision: self.revision,
            idempotent_replay: false,
        }
    }
}

impl Default for RoomState {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
pub fn new_event_id() -> Uuid {
    Uuid::new_v4()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_while_paused_is_the_anchor() {
        let state = TransportState {
            playing: false,
            anchor_position_us: 12_000_000,
            anchor_server_time_us: 47_000_000,
            playback_rate: 1.0,
        };
        assert_eq!(state.position_at(90_000_000), 12_000_000);
    }

    #[test]
    fn position_while_playing_advances_with_elapsed_time() {
        let state = TransportState {
            playing: true,
            anchor_position_us: 12_000_000,
            anchor_server_time_us: 47_000_000,
            playback_rate: 1.0,
        };
        assert_eq!(state.position_at(48_000_000), 13_000_000);
    }

    #[test]
    fn position_while_playing_at_anchor_time_is_unchanged() {
        let state = TransportState {
            playing: true,
            anchor_position_us: 12_000_000,
            anchor_server_time_us: 47_000_000,
            playback_rate: 1.0,
        };
        assert_eq!(state.position_at(47_000_000), 12_000_000);
    }

    #[test]
    fn schedule_play_from_paused_sets_future_anchor() {
        let mut room = RoomState::new();
        let event = room.schedule_play(48_225_000, 150_000);

        assert_eq!(event.effective_server_time_us, 48_375_000);
        assert_eq!(event.position_us, 0);
        assert_eq!(event.revision, 1);
        assert!(!event.idempotent_replay);
        assert!(room.transport.playing);
        assert_eq!(room.transport.anchor_server_time_us, 48_375_000);
        assert_eq!(room.revision, 1);
    }

    #[test]
    fn schedule_play_preserves_position_already_in_progress() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        // Advance time, then pause partway through, then play again.
        let pause_event = room.schedule_pause(1_150_000, 150_000);
        assert_eq!(pause_event.position_us, 1_150_000);

        let play_event = room.schedule_play(2_000_000, 150_000);
        assert_eq!(play_event.position_us, 1_150_000);
        assert_eq!(play_event.effective_server_time_us, 2_150_000);
    }

    #[test]
    fn schedule_play_is_idempotent_while_already_playing() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        let revision_after_first_play = room.revision;

        let replay = room.schedule_play(1_000_000, 150_000);

        assert!(replay.idempotent_replay);
        assert_eq!(room.revision, revision_after_first_play);
    }

    #[test]
    fn schedule_pause_freezes_position_at_effective_time() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);

        let event = room.schedule_pause(48_100_000, 150_000);

        assert_eq!(event.effective_server_time_us, 48_250_000);
        // Playing since anchor_server_time_us = 150_000.
        assert_eq!(event.position_us, 48_250_000 - 150_000);
        assert!(!room.transport.playing);
        assert_eq!(room.transport.anchor_position_us, event.position_us);
        assert_eq!(room.transport.anchor_server_time_us, 48_250_000);
    }

    #[test]
    fn revisions_increase_monotonically_across_transitions() {
        let mut room = RoomState::new();
        assert_eq!(room.revision, 0);
        room.schedule_play(0, 150_000);
        assert_eq!(room.revision, 1);
        room.schedule_pause(1_000_000, 150_000);
        assert_eq!(room.revision, 2);
        room.schedule_play(2_000_000, 150_000);
        assert_eq!(room.revision, 3);
    }

    #[test]
    fn current_position_matches_transport_position() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        assert_eq!(room.current_position(1_150_000), room.transport.position_at(1_150_000));
    }

    #[test]
    fn schedule_restart_zeroes_position_and_keeps_playing() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);

        let event = room.schedule_restart(1_150_000, 150_000);

        assert_eq!(event.action, TransportAction::Restart);
        assert_eq!(event.position_us, 0);
        assert_eq!(event.effective_server_time_us, 1_300_000);
        assert!(room.transport.playing);
        assert_eq!(room.transport.anchor_position_us, 0);
        assert_eq!(room.transport.anchor_server_time_us, 1_300_000);
    }

    #[test]
    fn schedule_restart_zeroes_position_and_keeps_paused() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        room.schedule_pause(1_150_000, 150_000);

        let event = room.schedule_restart(2_000_000, 150_000);

        assert_eq!(event.position_us, 0);
        assert!(!room.transport.playing);
        assert_eq!(room.transport.anchor_position_us, 0);
    }

    #[test]
    fn schedule_restart_increments_revision() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        let revision_after_play = room.revision;

        room.schedule_restart(1_000_000, 150_000);

        assert_eq!(room.revision, revision_after_play + 1);
    }
}

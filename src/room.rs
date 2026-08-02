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

/// The outcome of toggling a room-wide boolean setting (tempo-nudge,
/// bass-cut, ...), ready to be turned into that setting's canonical
/// `ServerMessage::*SettingChanged` variant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoolSettingEventData {
    pub enabled: bool,
    pub revision: u64,
    pub idempotent_replay: bool,
}

/// The outcome of setting/overwriting the room's single cue point, ready to
/// be turned into `ServerMessage::CuePointChanged`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CuePointEventData {
    pub position_us: u64,
    pub revision: u64,
}

/// The room's single loop region. `active` means "primed to wrap back to
/// `start_us` once playback reaches `end_us`" - not "currently playing";
/// an inactive loop still exists (visible, re-activatable) but is inert.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopRegion {
    pub start_us: u64,
    pub end_us: u64,
    pub active: bool,
}

/// The outcome of inserting, overwriting, or toggling the room's single
/// loop region, ready to be turned into `ServerMessage::LoopChanged`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoopEventData {
    pub start_us: u64,
    pub end_us: u64,
    pub active: bool,
    pub revision: u64,
}

#[derive(Debug, Clone)]
pub struct RoomState {
    pub transport: TransportState,
    /// Whether clients should apply local tempo-nudge drift correction.
    /// Not part of the transport timeline, but shares its revision counter
    /// so clients can apply both kinds of event in a single ordered stream.
    pub nudge_enabled: bool,
    /// Whether clients should apply the bass-cut (highpass) effect. Same
    /// shared-revision convention as nudge_enabled.
    pub bass_cut_enabled: bool,
    /// Whether clients should pitch-correct tempo changes. Same
    /// shared-revision convention as nudge_enabled.
    pub pitch_lock_enabled: bool,
    /// The room's single cue point, or `None` if nothing's been set yet.
    /// Same shared-revision convention as the settings above. There is
    /// deliberately no way to clear it back to `None` once set - only to
    /// overwrite it with a new position (see `set_cue_point`).
    pub cue_point_us: Option<u64>,
    /// The room's single loop region, or `None` if none has been inserted
    /// yet. Same shared-revision convention as the settings above.
    pub loop_region: Option<LoopRegion>,
    pub revision: u64,
}

impl RoomState {
    pub fn new() -> Self {
        Self {
            transport: TransportState::initial(),
            nudge_enabled: true,
            bass_cut_enabled: false,
            pitch_lock_enabled: true,
            cue_point_us: None,
            loop_region: None,
            revision: 0,
        }
    }

    pub fn current_position(&self, now_us: ServerTimeUs) -> u64 {
        self.position_at_with_loop(now_us)
    }

    /// The position at `now_us`, wrapped into the active loop's bounds if
    /// one exists. Looping is deliberately a property of this formula
    /// rather than a discrete "seek back" event fired by whichever client
    /// notices first: every observer (server and every client alike)
    /// derives the identical wrapped position from the same
    /// anchor/rate/loop state, with no network round-trip and no race to
    /// coordinate.
    ///
    /// Only wraps when the transport's own anchor sits at or before the
    /// loop's end - i.e. we reached `end_us` via continuous playback
    /// through the loop, not because an explicit seek/restart/cue-release
    /// placed the anchor somewhere past it. Without this guard, seeking to
    /// a position after a loop would immediately "snap back" into it, which
    /// is not what seeking there means.
    fn position_at_with_loop(&self, now_us: ServerTimeUs) -> u64 {
        let raw = self.transport.position_at(now_us);
        let Some(loop_region) = &self.loop_region else {
            return raw;
        };
        if !loop_region.active || loop_region.end_us <= loop_region.start_us {
            return raw;
        }
        if self.transport.anchor_position_us >= loop_region.end_us || raw < loop_region.end_us {
            return raw;
        }
        let loop_len = loop_region.end_us - loop_region.start_us;
        let offset = (raw - loop_region.start_us) % loop_len;
        loop_region.start_us + offset
    }

    /// Toggles the room-wide tempo-nudge setting. Idempotent: setting it to
    /// its current value doesn't bump the revision or count as a fresh
    /// change, mirroring `schedule_play`'s idempotent-replay convention.
    pub fn set_nudge_enabled(&mut self, enabled: bool) -> BoolSettingEventData {
        if self.nudge_enabled == enabled {
            return BoolSettingEventData {
                enabled,
                revision: self.revision,
                idempotent_replay: true,
            };
        }
        self.nudge_enabled = enabled;
        self.revision += 1;
        BoolSettingEventData {
            enabled,
            revision: self.revision,
            idempotent_replay: false,
        }
    }

    /// Toggles the room-wide bass-cut setting. Same idempotent convention
    /// as `set_nudge_enabled`.
    pub fn set_bass_cut_enabled(&mut self, enabled: bool) -> BoolSettingEventData {
        if self.bass_cut_enabled == enabled {
            return BoolSettingEventData {
                enabled,
                revision: self.revision,
                idempotent_replay: true,
            };
        }
        self.bass_cut_enabled = enabled;
        self.revision += 1;
        BoolSettingEventData {
            enabled,
            revision: self.revision,
            idempotent_replay: false,
        }
    }

    /// Toggles the room-wide pitch-lock setting. Same idempotent convention
    /// as `set_nudge_enabled`.
    pub fn set_pitch_lock_enabled(&mut self, enabled: bool) -> BoolSettingEventData {
        if self.pitch_lock_enabled == enabled {
            return BoolSettingEventData {
                enabled,
                revision: self.revision,
                idempotent_replay: true,
            };
        }
        self.pitch_lock_enabled = enabled;
        self.revision += 1;
        BoolSettingEventData {
            enabled,
            revision: self.revision,
            idempotent_replay: false,
        }
    }

    /// Sets (or overwrites) the room's single cue point. Unlike the boolean
    /// settings above, this is not idempotent-by-value: setting it to the
    /// same position it already holds still counts as a fresh action and
    /// bumps the revision, mirroring `schedule_restart`/`schedule_seek`
    /// (this is a discrete user action - "set a cue point here" - not a
    /// settings toggle where redundant no-op requests should be absorbed).
    pub fn set_cue_point(&mut self, position_us: u64) -> CuePointEventData {
        self.cue_point_us = Some(position_us);
        self.revision += 1;
        CuePointEventData {
            position_us,
            revision: self.revision,
        }
    }

    /// Inserts/overwrites the room's single loop region, always active, and
    /// moves the cue point to its start (so the cue marker matches where
    /// the loop begins). Two fresh revisions - one for the cue point, one
    /// for the loop - broadcast as two separate events by the caller (see
    /// websocket.rs): a single event can't carry two revision bumps without
    /// the second silently failing every client's staleness check, since
    /// revision is how clients tell "already applied" from "new".
    pub fn set_loop(&mut self, start_us: u64, end_us: u64) -> (CuePointEventData, LoopEventData) {
        let cue_event = self.set_cue_point(start_us);
        self.loop_region = Some(LoopRegion { start_us, end_us, active: true });
        self.revision += 1;
        let loop_event = LoopEventData { start_us, end_us, active: true, revision: self.revision };
        (cue_event, loop_event)
    }

    /// Toggles the existing loop's active flag without changing its bounds.
    /// Returns `None` if no loop has been inserted yet - nothing to toggle.
    pub fn set_loop_active(&mut self, active: bool) -> Option<LoopEventData> {
        let loop_region = self.loop_region.as_mut()?;
        loop_region.active = active;
        let (start_us, end_us) = (loop_region.start_us, loop_region.end_us);
        self.revision += 1;
        Some(LoopEventData { start_us, end_us, active, revision: self.revision })
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
        let position_us = self.position_at_with_loop(now_us);

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
        let position_us = self.position_at_with_loop(effective_time_us);

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

    /// Schedules a seek to an arbitrary position `lead_time_us` in the
    /// future, without changing whether it's playing or paused. Generalizes
    /// `schedule_restart` (seeking to position 0) to any target position -
    /// e.g. clicking/dragging a waveform. Like `schedule_restart`, always a
    /// fresh transition (no idempotent-by-value check): landing on the same
    /// position twice is still the intended effect, not a no-op.
    pub fn schedule_seek(&mut self, now_us: ServerTimeUs, lead_time_us: u64, position_us: u64) -> TransportEventData {
        let effective_time_us = now_us + lead_time_us;

        self.transport.anchor_position_us = position_us;
        self.transport.anchor_server_time_us = effective_time_us;
        self.revision += 1;

        TransportEventData {
            action: TransportAction::Seek,
            effective_server_time_us: effective_time_us,
            position_us,
            playback_rate: self.transport.playback_rate,
            revision: self.revision,
            idempotent_replay: false,
        }
    }

    /// Releases a cue-point preview hold `lead_time_us` in the future:
    /// pauses the transport at whatever `cue_point_us` currently holds. The
    /// server is the sole source of truth for that position - unlike
    /// `schedule_seek`, callers never supply one. If no cue point has been
    /// set yet, this is a no-op (idempotent_replay) rather than a panic,
    /// since there's nothing sensible to release to.
    pub fn schedule_cue_release(&mut self, now_us: ServerTimeUs, lead_time_us: u64) -> TransportEventData {
        let Some(cue_position_us) = self.cue_point_us else {
            return TransportEventData {
                action: TransportAction::CueRelease,
                effective_server_time_us: self.transport.anchor_server_time_us,
                position_us: self.transport.anchor_position_us,
                playback_rate: self.transport.playback_rate,
                revision: self.revision,
                idempotent_replay: true,
            };
        };

        let effective_time_us = now_us + lead_time_us;

        self.transport.playing = false;
        self.transport.anchor_position_us = cue_position_us;
        self.transport.anchor_server_time_us = effective_time_us;
        self.revision += 1;

        TransportEventData {
            action: TransportAction::CueRelease,
            effective_server_time_us: effective_time_us,
            position_us: cue_position_us,
            playback_rate: self.transport.playback_rate,
            revision: self.revision,
            idempotent_replay: false,
        }
    }

    /// Schedules a playback-rate change `lead_time_us` in the future, without
    /// changing whether it's playing or paused. Mirrors `schedule_pause`'s
    /// convention: position is re-anchored at the effective time under the
    /// OLD rate, then the anchor switches over to the new rate from there.
    /// Idempotent if the rate is unchanged, like `schedule_play`.
    pub fn schedule_playback_rate(
        &mut self,
        now_us: ServerTimeUs,
        lead_time_us: u64,
        new_rate: f64,
    ) -> TransportEventData {
        if (self.transport.playback_rate - new_rate).abs() < f64::EPSILON {
            return TransportEventData {
                action: TransportAction::SetTempo,
                effective_server_time_us: self.transport.anchor_server_time_us,
                position_us: self.transport.anchor_position_us,
                playback_rate: self.transport.playback_rate,
                revision: self.revision,
                idempotent_replay: true,
            };
        }

        let effective_time_us = now_us + lead_time_us;
        let position_us = self.position_at_with_loop(effective_time_us);

        self.transport.anchor_position_us = position_us;
        self.transport.anchor_server_time_us = effective_time_us;
        self.transport.playback_rate = new_rate;
        self.revision += 1;

        TransportEventData {
            action: TransportAction::SetTempo,
            effective_server_time_us: effective_time_us,
            position_us,
            playback_rate: new_rate,
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

    #[test]
    fn schedule_seek_jumps_to_target_position_and_keeps_playing() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);

        let event = room.schedule_seek(1_150_000, 150_000, 4_500_000);

        assert_eq!(event.action, TransportAction::Seek);
        assert_eq!(event.position_us, 4_500_000);
        assert_eq!(event.effective_server_time_us, 1_300_000);
        assert!(room.transport.playing);
        assert_eq!(room.transport.anchor_position_us, 4_500_000);
        assert_eq!(room.transport.anchor_server_time_us, 1_300_000);
    }

    #[test]
    fn schedule_seek_jumps_to_target_position_and_keeps_paused() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        room.schedule_pause(1_150_000, 150_000);

        let event = room.schedule_seek(2_000_000, 150_000, 7_000_000);

        assert_eq!(event.position_us, 7_000_000);
        assert!(!room.transport.playing);
        assert_eq!(room.transport.anchor_position_us, 7_000_000);
    }

    #[test]
    fn schedule_seek_increments_revision_even_to_the_same_position() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        room.schedule_seek(1_000_000, 150_000, 4_500_000);
        let revision_after_first_seek = room.revision;

        let event = room.schedule_seek(1_500_000, 150_000, 4_500_000);

        assert_eq!(room.revision, revision_after_first_seek + 1);
        assert!(!event.idempotent_replay);
    }

    #[test]
    fn cue_point_defaults_to_none() {
        let room = RoomState::new();
        assert_eq!(room.cue_point_us, None);
    }

    #[test]
    fn set_cue_point_stores_position_and_bumps_revision() {
        let mut room = RoomState::new();
        let revision_before = room.revision;

        let event = room.set_cue_point(6_000_000);

        assert_eq!(event.position_us, 6_000_000);
        assert_eq!(room.cue_point_us, Some(6_000_000));
        assert_eq!(room.revision, revision_before + 1);
    }

    #[test]
    fn set_cue_point_overwrites_and_bumps_revision_even_to_the_same_position() {
        let mut room = RoomState::new();
        room.set_cue_point(6_000_000);
        let revision_after_first_set = room.revision;

        let event = room.set_cue_point(6_000_000);

        assert_eq!(room.revision, revision_after_first_set + 1);
        assert_eq!(event.position_us, 6_000_000);

        room.set_cue_point(9_000_000);
        assert_eq!(room.cue_point_us, Some(9_000_000));
    }

    #[test]
    fn schedule_cue_release_pauses_at_the_cue_point() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        room.set_cue_point(6_000_000);

        let event = room.schedule_cue_release(1_000_000, 150_000);

        assert_eq!(event.action, TransportAction::CueRelease);
        assert_eq!(event.position_us, 6_000_000);
        assert!(!room.transport.playing);
        assert_eq!(room.transport.anchor_position_us, 6_000_000);
        assert!(!event.idempotent_replay);
    }

    #[test]
    fn schedule_cue_release_with_no_cue_point_is_a_no_op() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        let revision_before = room.revision;

        let event = room.schedule_cue_release(1_000_000, 150_000);

        assert!(event.idempotent_replay);
        assert!(room.transport.playing); // untouched
        assert_eq!(room.revision, revision_before);
    }

    #[test]
    fn loop_region_defaults_to_none() {
        let room = RoomState::new();
        assert_eq!(room.loop_region, None);
    }

    #[test]
    fn set_loop_creates_active_loop_and_moves_cue_point() {
        let mut room = RoomState::new();
        let revision_before = room.revision;

        let (cue_event, loop_event) = room.set_loop(1_000_000, 3_000_000);

        assert_eq!(cue_event.position_us, 1_000_000);
        assert_eq!(room.cue_point_us, Some(1_000_000));
        assert_eq!(loop_event.start_us, 1_000_000);
        assert_eq!(loop_event.end_us, 3_000_000);
        assert!(loop_event.active);
        assert_eq!(room.loop_region, Some(LoopRegion { start_us: 1_000_000, end_us: 3_000_000, active: true }));
        // Two fresh revisions: one for the cue point, one for the loop.
        assert_eq!(cue_event.revision, revision_before + 1);
        assert_eq!(loop_event.revision, revision_before + 2);
        assert_eq!(room.revision, revision_before + 2);
    }

    #[test]
    fn set_loop_overwrites_existing_loop() {
        let mut room = RoomState::new();
        room.set_loop(1_000_000, 3_000_000);

        room.set_loop(5_000_000, 9_000_000);

        assert_eq!(room.loop_region, Some(LoopRegion { start_us: 5_000_000, end_us: 9_000_000, active: true }));
        assert_eq!(room.cue_point_us, Some(5_000_000));
    }

    #[test]
    fn set_loop_active_toggles_existing_loop() {
        let mut room = RoomState::new();
        room.set_loop(1_000_000, 3_000_000);
        let revision_before = room.revision;

        let event = room.set_loop_active(false).expect("loop exists");

        assert!(!event.active);
        assert!(!room.loop_region.unwrap().active);
        assert_eq!(room.revision, revision_before + 1);

        let event = room.set_loop_active(true).expect("loop exists");
        assert!(event.active);
        assert!(room.loop_region.unwrap().active);
    }

    #[test]
    fn set_loop_active_with_no_loop_returns_none() {
        let mut room = RoomState::new();
        assert_eq!(room.set_loop_active(true), None);
    }

    #[test]
    fn current_position_wraps_within_active_loop_during_playback() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000); // anchor_position_us=0, anchor_server_time_us=150_000
        room.set_loop(1_000_000, 3_000_000); // 2s loop starting at 1s

        // now_us chosen so raw elapsed position (7.5s) is well past the loop's
        // end (3s) - 6.5s past the loop's start, i.e. 3 full loop lengths
        // (2s each) plus 0.5s: should wrap to 1.5s, not read back 7.5s.
        let position = room.current_position(7_650_000);

        assert_eq!(position, 1_500_000);
    }

    #[test]
    fn current_position_does_not_wrap_when_loop_inactive() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        room.set_loop(1_000_000, 3_000_000);
        room.set_loop_active(false);

        let position = room.current_position(7_650_000);

        assert_eq!(position, 7_500_000); // raw, unwrapped - the loop is inert
    }

    #[test]
    fn current_position_does_not_snap_back_when_seeked_past_loop_end() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        room.set_loop(1_000_000, 3_000_000);

        // Seeking to 5s (past the loop's 3s end) must not immediately
        // "snap back" into the loop - that's not what seeking there means.
        room.schedule_seek(1_000_000, 150_000, 5_000_000);

        let position = room.current_position(1_300_000); // shortly after the seek's effective time
        assert!(position >= 5_000_000); // still past the loop, not wrapped back to ~1-3s
    }

    #[test]
    fn schedule_pause_freezes_at_wrapped_position_during_active_loop() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        room.set_loop(1_000_000, 3_000_000);

        let event = room.schedule_pause(7_500_000, 150_000);

        assert_eq!(event.position_us, 1_500_000);
        assert_eq!(room.transport.anchor_position_us, 1_500_000);
        assert!(!room.transport.playing);
    }

    #[test]
    fn nudge_enabled_defaults_to_true() {
        let room = RoomState::new();
        assert!(room.nudge_enabled);
    }

    #[test]
    fn set_nudge_enabled_toggles_and_bumps_revision() {
        let mut room = RoomState::new();

        let event = room.set_nudge_enabled(false);

        assert!(!event.enabled);
        assert!(!event.idempotent_replay);
        assert_eq!(event.revision, 1);
        assert!(!room.nudge_enabled);
        assert_eq!(room.revision, 1);
    }

    #[test]
    fn set_nudge_enabled_is_idempotent_when_unchanged() {
        let mut room = RoomState::new();
        room.set_nudge_enabled(false);
        let revision_after_first_toggle = room.revision;

        let replay = room.set_nudge_enabled(false);

        assert!(replay.idempotent_replay);
        assert_eq!(room.revision, revision_after_first_toggle);
    }

    #[test]
    fn nudge_setting_shares_revision_counter_with_transport() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        assert_eq!(room.revision, 1);

        room.set_nudge_enabled(false);
        assert_eq!(room.revision, 2);

        room.schedule_pause(1_000_000, 150_000);
        assert_eq!(room.revision, 3);
    }

    #[test]
    fn bass_cut_enabled_defaults_to_false() {
        let room = RoomState::new();
        assert!(!room.bass_cut_enabled);
    }

    #[test]
    fn set_bass_cut_enabled_toggles_and_bumps_revision() {
        let mut room = RoomState::new();

        let event = room.set_bass_cut_enabled(true);

        assert!(event.enabled);
        assert!(!event.idempotent_replay);
        assert_eq!(event.revision, 1);
        assert!(room.bass_cut_enabled);
        assert_eq!(room.revision, 1);
    }

    #[test]
    fn set_bass_cut_enabled_is_idempotent_when_unchanged() {
        let mut room = RoomState::new();
        room.set_bass_cut_enabled(true);
        let revision_after_first_toggle = room.revision;

        let replay = room.set_bass_cut_enabled(true);

        assert!(replay.idempotent_replay);
        assert_eq!(room.revision, revision_after_first_toggle);
    }

    #[test]
    fn bass_cut_setting_shares_revision_counter_with_transport_and_nudge() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        assert_eq!(room.revision, 1);

        room.set_bass_cut_enabled(true);
        assert_eq!(room.revision, 2);

        room.set_nudge_enabled(false);
        assert_eq!(room.revision, 3);
    }

    #[test]
    fn pitch_lock_enabled_defaults_to_true() {
        let room = RoomState::new();
        assert!(room.pitch_lock_enabled);
    }

    #[test]
    fn set_pitch_lock_enabled_toggles_and_bumps_revision() {
        let mut room = RoomState::new();

        let event = room.set_pitch_lock_enabled(false);

        assert!(!event.enabled);
        assert!(!event.idempotent_replay);
        assert_eq!(event.revision, 1);
        assert!(!room.pitch_lock_enabled);
        assert_eq!(room.revision, 1);
    }

    #[test]
    fn set_pitch_lock_enabled_is_idempotent_when_unchanged() {
        let mut room = RoomState::new();
        room.set_pitch_lock_enabled(false);
        let revision_after_first_toggle = room.revision;

        let replay = room.set_pitch_lock_enabled(false);

        assert!(replay.idempotent_replay);
        assert_eq!(room.revision, revision_after_first_toggle);
    }

    #[test]
    fn pitch_lock_setting_shares_revision_counter_with_transport_and_other_settings() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        assert_eq!(room.revision, 1);

        room.set_pitch_lock_enabled(false);
        assert_eq!(room.revision, 2);

        room.set_bass_cut_enabled(true);
        assert_eq!(room.revision, 3);
    }

    #[test]
    fn schedule_playback_rate_reanchors_position_while_playing() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);

        let event = room.schedule_playback_rate(48_100_000, 150_000, 1.03);

        assert_eq!(event.action, TransportAction::SetTempo);
        assert_eq!(event.effective_server_time_us, 48_250_000);
        // Playing at rate 1.0 since anchor_server_time_us = 150_000.
        assert_eq!(event.position_us, 48_250_000 - 150_000);
        assert_eq!(event.playback_rate, 1.03);
        assert_eq!(room.transport.playback_rate, 1.03);
        assert!(room.transport.playing);
        assert_eq!(room.transport.anchor_position_us, event.position_us);
        assert_eq!(room.transport.anchor_server_time_us, 48_250_000);
    }

    #[test]
    fn schedule_playback_rate_while_paused_keeps_position_and_paused_state() {
        let mut room = RoomState::new();

        let event = room.schedule_playback_rate(0, 150_000, 0.94);

        assert_eq!(event.position_us, 0);
        assert_eq!(event.playback_rate, 0.94);
        assert!(!room.transport.playing);
        assert_eq!(room.transport.anchor_position_us, 0);
    }

    #[test]
    fn schedule_playback_rate_is_idempotent_when_unchanged() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        let revision_after_play = room.revision;

        let replay = room.schedule_playback_rate(1_000_000, 150_000, 1.0);

        assert!(replay.idempotent_replay);
        assert_eq!(room.revision, revision_after_play);
    }

    #[test]
    fn schedule_playback_rate_increments_revision() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        let revision_after_play = room.revision;

        room.schedule_playback_rate(1_000_000, 150_000, 1.06);

        assert_eq!(room.revision, revision_after_play + 1);
    }

    #[test]
    fn subsequent_play_after_tempo_change_carries_the_new_rate() {
        let mut room = RoomState::new();
        room.schedule_playback_rate(0, 150_000, 1.06);
        room.schedule_pause(1_000_000, 150_000);

        let event = room.schedule_play(2_000_000, 150_000);

        assert_eq!(event.playback_rate, 1.06);
    }

    #[test]
    fn schedule_playback_rate_with_zero_lead_time_applies_immediately() {
        // The continuous tempo-sample stream applies with no lead time (see
        // TEMPO_SAMPLE_LEAD_TIME_US in websocket.rs) - unlike play/pause/
        // restart, each sample takes effect at `now_us` itself.
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);

        let event = room.schedule_playback_rate(48_100_000, 0, 1.03);

        assert_eq!(event.effective_server_time_us, 48_100_000);
        assert_eq!(event.position_us, 48_100_000 - 150_000);
        assert_eq!(room.transport.anchor_server_time_us, 48_100_000);
    }

    #[test]
    fn successive_tempo_changes_each_get_a_strictly_increasing_revision() {
        let mut room = RoomState::new();
        room.schedule_play(0, 150_000);
        let after_play = room.revision;

        let first = room.schedule_playback_rate(1_000_000, 0, 1.01);
        let second = room.schedule_playback_rate(1_040_000, 0, 1.02);
        let third = room.schedule_playback_rate(1_080_000, 0, 1.03);

        assert_eq!(first.revision, after_play + 1);
        assert_eq!(second.revision, after_play + 2);
        assert_eq!(third.revision, after_play + 3);
        assert_eq!(room.transport.playback_rate, 1.03);
    }
}

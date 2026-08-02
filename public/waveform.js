// Waveform visualization and click/drag-to-seek, layered on top of the main
// script (index.html) rather than merged into it: wavesurfer.js's ESM
// bundle needs `import`, which requires `type="module"`, and module scripts
// run in their own scope. The two scripts talk to each other only through
// explicit `window.x = ...` bridges in both directions (function
// declarations in the classic script do attach to `window` automatically,
// but its top-level `let`s don't - so the classic script exposes what this
// file needs to read, e.g. `window.decks.a.sendSeekRequest`/`getCuePointUs`/
// `getLoopRegion`, and this file exposes what the classic script needs, e.g.
// `window.waveSurfers`/`window.renderRegions` below - deliberately, rather
// than relying on implicit global visibility either way).
//
// One independent WaveSurfer + regions-plugin instance per deck (own
// container div, own region set) - deck A and deck B visualize/seek two
// entirely separate audio files.
import WaveSurfer from "./vendor/wavesurfer/wavesurfer.esm.js";
import RegionsPlugin from "./vendor/wavesurfer/plugins/regions.esm.js";

function createDeckWaveform(deckId) {
  const regions = RegionsPlugin.create();

  const waveSurfer = WaveSurfer.create({
    container: `#waveform-${deckId}`,
    waveColor: "#7b7b7b",
    progressColor: "#3b82f6",
    height: 80,
    // A plain click seeks immediately regardless of this option (see
    // `interact` below) - dragToSeek additionally fires many intermediate
    // seeks while the pointer is held down and moving. Left off deliberately:
    // every seek is a synced room-wide transport event that makes every
    // connected client's audio pipeline restart its source node, so a
    // continuous drag would spam the room with restarts instead of one
    // deliberate jump.
    dragToSeek: false,
    interact: true,
    plugins: [regions],
  });

  // Regions don't survive loadBlob() decoding a new file on this same
  // instance, so whenever a fresh file finishes decoding, re-apply whatever
  // cue point/loop currently exist for this deck (read via
  // window.decks[deckId].getCuePointUs/getLoopRegion, the same
  // explicit-bridge convention as sendSeekRequest, since that state lives in
  // the main classic script, not here). Calling this from the main script's
  // file-input handler directly - instead of relying on "decode" - would
  // race: loadBlob() is async, and this event is the only reliable signal
  // that wavesurfer's own decode-side-effects have finished.
  waveSurfer.on("decode", () => {
    window.renderRegions(deckId, {
      cuePointUs: window.decks?.[deckId]?.getCuePointUs() ?? null,
      loopRegion: window.decks?.[deckId]?.getLoopRegion() ?? null,
    });
  });

  waveSurfer.on("interaction", (newTimeS) => {
    // Only ever fires from a genuine user click/drag on the waveform (per
    // the wavesurfer.js source: the renderer's click/drag handlers are the
    // only callers of `emit("interaction", ...)`; the programmatic
    // `setTime()` the main script's tick() loop uses to keep the playhead in
    // sync never touches it) - so this can't loop back on itself.
    const positionUs = Math.max(0, Math.round(newTimeS * 1e6));
    window.decks?.[deckId]?.sendSeekRequest(positionUs);
  });

  return { waveSurfer, regions };
}

const waveformDecks = { a: createDeckWaveform("a"), b: createDeckWaveform("b") };

window.waveSurfers = { a: waveformDecks.a.waveSurfer, b: waveformDecks.b.waveSurfer };

/// Renders BOTH a deck's cue point marker and its loop region together -
/// they have to be managed as one combined function, not two independent
/// "clear + add" calls, because regions.clearRegions() wipes every region on
/// that deck's waveform, not just the caller's own one. Every call site that
/// changes either piece of state passes the FULL current state (both
/// values, one of which may be unchanged) so neither ever gets silently
/// wiped by an update to the other.
///
/// `loopRegion` (if given) is `{start_us, end_us, active}`; its color
/// reflects `active` - amber while primed to loop, grey/15% while inert.
window.renderRegions = function (deckId, { cuePointUs, loopRegion }) {
  const regions = waveformDecks[deckId].regions;
  regions.clearRegions();
  if (loopRegion) {
    regions.addRegion({
      start: loopRegion.start_us / 1e6,
      end: loopRegion.end_us / 1e6,
      color: loopRegion.active ? "rgba(255, 191, 0, 0.8)" : "rgba(128, 128, 128, 0.15)",
      drag: false,
      resize: false,
    });
  }
  if (cuePointUs !== null && cuePointUs !== undefined) {
    regions.addRegion({
      start: cuePointUs / 1e6,
      color: "rgba(255, 0, 0, 0.8)",
      drag: false,
      resize: false,
    });
  }
};

// Waveform visualization and click/drag-to-seek, layered on top of the main
// script (index.html) rather than merged into it: wavesurfer.js's ESM
// bundle needs `import`, which requires `type="module"`, and module scripts
// run in their own scope. The two scripts talk to each other only through
// explicit `window.x = ...` bridges in both directions (function
// declarations in the classic script do attach to `window` automatically,
// but its top-level `let`s don't - so the classic script exposes what this
// file needs to read, e.g. `window.sendSeekRequest`/`window.getCuePointUs`,
// and this file exposes what the classic script needs, e.g.
// `window.waveSurfer`/`window.updateCueMarker` below - deliberately, rather
// than relying on implicit global visibility either way).
import WaveSurfer from "./vendor/wavesurfer/wavesurfer.esm.js";
import RegionsPlugin from "./vendor/wavesurfer/plugins/regions.esm.js";

// Instantiated and wired into the plugins list now so it's ready for a
// future loop-region feature - the only region currently in use is the cue
// point marker below, no drag-selection is enabled yet ("just seeking" for
// now).
const regions = RegionsPlugin.create();

const waveSurfer = WaveSurfer.create({
  container: "#waveform",
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
window.waveSurfer = waveSurfer;

/// Renders the room's single cue point (see the "Cue" button in index.html)
/// as a red marker, or clears it entirely if `positionUs` is null. Called
/// from the main script whenever cuePointUs changes (a synced
/// cue_point_changed broadcast, a fresh state_snapshot, or a new file
/// finishing decode - regions don't survive loadBlob() re-decoding a new
/// file on this same instance, so the marker needs re-applying each time).
/// clearRegions() first since there's only ever one cue point - a stale
/// marker from a previous position must never linger alongside a new one.
window.updateCueMarker = function (positionUs) {
  regions.clearRegions();
  if (positionUs === null) return;
  regions.addRegion({
    start: positionUs / 1e6,
    color: "rgba(255, 0, 0, 0.8)",
    drag: false,
    resize: false,
  });
};

// Regions don't survive loadBlob() decoding a new file on this same
// instance, so whenever a fresh file finishes decoding, re-apply whatever
// cue point currently exists (read via window.getCuePointUs, the same
// explicit-bridge convention as window.sendSeekRequest, since the cue
// point's state lives in the main classic script, not here). Calling this
// from the main script's file-input handler directly - instead of relying
// on "decode" - would race: loadBlob() is async, and this event is the only
// reliable signal that wavesurfer's own decode-side-effects have finished.
waveSurfer.on("decode", () => {
  window.updateCueMarker(window.getCuePointUs?.() ?? null);
});

waveSurfer.on("interaction", (newTimeS) => {
  // Only ever fires from a genuine user click/drag on the waveform (per the
  // wavesurfer.js source: the renderer's click/drag handlers are the only
  // callers of `emit("interaction", ...)`; the programmatic `setTime()` the
  // main script's tick() loop uses to keep the playhead in sync never
  // touches it) - so this can't loop back on itself.
  const positionUs = Math.max(0, Math.round(newTimeS * 1e6));
  window.sendSeekRequest(positionUs);
});

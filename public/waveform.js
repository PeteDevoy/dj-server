// Waveform visualization and click/drag-to-seek, layered on top of the main
// script (index.html) rather than merged into it: wavesurfer.js's ESM
// bundle needs `import`, which requires `type="module"`, and module scripts
// have their own top-level scope - unlike the main script's classic
// (non-module) top-level `let`/`function` declarations, which attach
// directly to `window` and are already readable from here once this module
// runs (module scripts are always deferred, so the classic script has
// already executed and defined them by the time this file's top-level code
// runs). The one thing THIS file needs to expose back is the WaveSurfer
// instance itself, since it's otherwise only visible in this module's scope
// - see `window.waveSurfer` below, read by the main script's tick() loop.
import WaveSurfer from "./vendor/wavesurfer/wavesurfer.esm.js";
import RegionsPlugin from "./vendor/wavesurfer/plugins/regions.esm.js";

// Instantiated and wired into the plugins list now so it's ready for a
// future loop-region feature, but deliberately unused otherwise - no
// regions are created and no drag-selection is enabled yet ("just seeking"
// for now).
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

waveSurfer.on("interaction", (newTimeS) => {
  // Only ever fires from a genuine user click/drag on the waveform (per the
  // wavesurfer.js source: the renderer's click/drag handlers are the only
  // callers of `emit("interaction", ...)`; the programmatic `setTime()` the
  // main script's tick() loop uses to keep the playhead in sync never
  // touches it) - so this can't loop back on itself.
  const positionUs = Math.max(0, Math.round(newTimeS * 1e6));
  window.sendSeekRequest(positionUs);
});

// BPM detection via web-audio-beat-detector
// (https://github.com/chrisguttandin/web-audio-beat-detector, MIT - see
// public/vendor/web-audio-beat-detector/LICENSE), vendored as a single
// self-contained bundle (fetched via esm.sh's `?bundle` mode, which inlines
// its dependency graph - unlike beat-parser-core, this one has no Node
// built-ins or unrelated audio-codec decoders to drag in, since it operates
// directly on an already-decoded AudioBuffer rather than decoding a file
// itself).
//
// A separate `type="module"` script for the same reason as waveform.js:
// ESM `import` needs a module context, and the main classic script's
// globals are already reachable from here once this runs (module scripts
// are always deferred, so the classic script has already executed).
//
// Unlike the BeatDetect.js path (bpm-worker.js), this library creates and
// manages its own internal Web Worker automatically (its bundle embeds the
// worker's source as a string and spins it up via a Blob URL) - so unlike
// startBpmDetection()'s BeatDetect.js branch, there's no manual
// OfflineAudioContext rendering or postMessage plumbing to do here at all.
import { guess } from "./vendor/web-audio-beat-detector/web-audio-beat-detector.esm.js";

/// Returns the raw (unrounded) detected tempo in BPM. Deliberately uses
/// `tempo` rather than `guess()`'s own `bpm` field, which is pre-rounded to
/// an integer - detectedBaseBpm should carry the same fractional precision
/// as the BeatDetect.js path (which uses `round: false`), so both engines
/// behave identically to whatever calls updateBpmDisplay()/currentBpm()
/// afterwards.
window.detectBpmWithWabd = async function (audioBuffer) {
  const { tempo } = await guess(audioBuffer);
  return tempo;
};

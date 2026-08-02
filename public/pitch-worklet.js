// A real-time pitch shifter: two overlapping Hann-windowed grains reading a
// circular delay line at `pitchRatio` speed while the write head advances at
// a fixed 1 sample per real-time sample. Reading a grain's content faster or
// slower than it was written shifts its pitch without changing how much
// wall-clock time the node consumes per sample - the classic two-grain OLA
// (overlap-add) pitch-shift technique.
//
// Used to cancel out the pitch change that naturally comes from resampling
// (AudioBufferSourceNode.playbackRate) when the tempo control changes speed:
// feed this node `pitchRatio = 1 / tempoRate` and the audible pitch stays
// locked to the original (0%) recording regardless of tempo.
//
// Plain OLA (reset each grain to a fixed offset behind the write head, as an
// earlier version of this file did) has a known artifact: over a grain's
// lifetime its read position drifts away from "real time" by roughly
// `grainSamples * (pitchRatio - 1)`, and the two grains are offset from each
// other by half a period in their own lifecycles - so at any instant they're
// generally reading the underlying waveform at *different relative phases*.
// Summing two out-of-phase copies of the same signal is comb filtering:
// audible as a wobble/warble or "phase smearing", worse on tonal content.
// This is exactly what WSOLA (Waveform-Similarity Overlap-Add) exists to
// fix: instead of resetting a grain to a fixed offset, search a small window
// around that natural position for the offset whose content best correlates
// with what the *other* grain is currently reading, and use that instead.
// That keeps the two grains' relative phase realigned at every reset rather
// than letting it drift for a full grain lifetime unconstrained.
//
// Tunables below (grain length, search radius, template length) trade off
// smearing vs. graininess vs. CPU cost - worth experimenting with by ear if
// the current values don't sound right for a given piece of audio.
class PitchShiftProcessor extends AudioWorkletProcessor {
  static get parameterDescriptors() {
    return [
      {
        name: "pitchRatio",
        defaultValue: 1.0,
        minValue: 0.5,
        maxValue: 2.0,
        automationRate: "a-rate",
      },
    ];
  }

  constructor() {
    super();
    this.grainSamples = Math.round(sampleRate * 0.08); // 80ms grains
    this.bufferSize = this.grainSamples * 4; // generous circular history, see readme above on safe lookback
    // How far to search around the "natural" reset position for a better-
    // aligned splice point, and how long a reference snippet to compare
    // against. Wider search = better odds of finding a good match (helps
    // more on lower/bass content, which has longer periods) but more CPU;
    // this runs only once per grain reset (~25/s total for both grains
    // combined at the default grain size), so it's cheap even fairly wide.
    this.searchRadius = Math.round(this.grainSamples / 8); // ~10ms at 80ms grains
    this.templateLen = Math.min(256, Math.floor(this.grainSamples / 4));
    this.templateScratch = new Float32Array(this.templateLen);

    this.channelBuffers = []; // one circular Float32Array per channel, created lazily once channel count is known
    this.writeIndex = 0;
    // Two grains, offset by half a period, so their Hann windows sum to a
    // constant (w(x) + w(x+0.5 mod 1) === 1 for a raised-cosine window) -
    // continuous output amplitude with no discontinuity at grain boundaries.
    this.grainPhase = [0, this.grainSamples / 2];
    this.grainReadPos = [0, 0];
    this.readPosInitialized = false;
  }

  hannWindow(x) {
    return 0.5 - 0.5 * Math.cos(2 * Math.PI * x);
  }

  readInterpolated(buf, pos) {
    const len = buf.length;
    let p = pos % len;
    if (p < 0) p += len;
    const i0 = Math.floor(p);
    const i1 = (i0 + 1) % len;
    const frac = p - i0;
    return buf[i0] * (1 - frac) + buf[i1] * frac;
  }

  /// Finds the integer offset (within +/-searchRadius of `naturalPos`) whose
  /// content best correlates with a snapshot of what's currently at
  /// `referencePos` - i.e. what the *other*, still-active grain is reading
  /// right now. Using channel 0 only, and applying the same chosen offset to
  /// every channel (see the call site), so stereo channels stay time-aligned
  /// with each other rather than each independently drifting.
  findBestAlignedPos(buf, naturalPos, referencePos) {
    const len = buf.length;
    const templateLen = this.templateLen;
    const template = this.templateScratch;
    const refStart = Math.floor(referencePos);
    for (let i = 0; i < templateLen; i++) {
      template[i] = buf[(refStart + i) % len];
    }

    let bestScore = -Infinity;
    let bestOffset = 0;
    for (let offset = -this.searchRadius; offset <= this.searchRadius; offset++) {
      const candidateStart = naturalPos + offset;
      let score = 0;
      for (let i = 0; i < templateLen; i++) {
        const idx = (((candidateStart + i) % len) + len) % len;
        score += buf[idx] * template[i];
      }
      if (score > bestScore) {
        bestScore = score;
        bestOffset = offset;
      }
    }
    return naturalPos + bestOffset;
  }

  process(inputs, outputs, parameters) {
    const input = inputs[0];
    const output = outputs[0];
    const pitchRatioParam = parameters.pitchRatio;
    const channelCount = output.length;
    const frames = 128; // fixed Web Audio render quantum

    for (let ch = 0; ch < channelCount; ch++) {
      if (!this.channelBuffers[ch]) this.channelBuffers[ch] = new Float32Array(this.bufferSize);
    }

    if (!this.readPosInitialized) {
      // Give both grains real (if silent) history to read from immediately,
      // `grainSamples` behind the write head - see the lookback-safety note
      // in the reset branch below for why that margin is enough. No search
      // needed for this cold start; both start from the same simple rule so
      // they're already mutually aligned.
      this.grainReadPos[0] = (this.writeIndex - this.grainSamples + this.bufferSize) % this.bufferSize;
      this.grainReadPos[1] = (this.writeIndex - this.grainSamples / 2 + this.bufferSize) % this.bufferSize;
      this.readPosInitialized = true;
    }

    for (let i = 0; i < frames; i++) {
      const pitchRatio = pitchRatioParam.length > 1 ? pitchRatioParam[i] : pitchRatioParam[0];
      // No pitch shift is being requested at all (tempo at 0%, or pitch-lock
      // off) - skip the grain synthesis entirely and pass the signal through
      // untouched, rather than running a nominally-transparent-but-not-free
      // OLA pass for no reason. Grain phase/read position still advance
      // normally below (at exactly 1:1 since pitchRatio is ~1.0 here), so
      // there's no stale state to resync when a real shift is requested again.
      const bypassed = Math.abs(pitchRatio - 1.0) < 1e-6;

      for (let ch = 0; ch < channelCount; ch++) {
        const inCh = input[ch] || input[0];
        this.channelBuffers[ch][this.writeIndex] = inCh ? inCh[i] : 0;
      }

      if (bypassed) {
        for (let ch = 0; ch < channelCount; ch++) {
          const inCh = input[ch] || input[0];
          output[ch][i] = inCh ? inCh[i] : 0;
        }
      } else {
        for (let ch = 0; ch < channelCount; ch++) {
          const buf = this.channelBuffers[ch];
          let sample = 0;
          for (let g = 0; g < 2; g++) {
            const windowValue = this.hannWindow(this.grainPhase[g] / this.grainSamples);
            sample += windowValue * this.readInterpolated(buf, this.grainReadPos[g]);
          }
          output[ch][i] = sample;
        }
      }

      for (let g = 0; g < 2; g++) {
        this.grainPhase[g] += 1;
        this.grainReadPos[g] += pitchRatio;
        if (this.grainPhase[g] >= this.grainSamples) {
          this.grainPhase[g] -= this.grainSamples;
          // `grainSamples` behind the write head is the *natural* position -
          // safe (see the margin reasoning below) but not phase-aligned with
          // the other grain, which is what causes the comb-filtering/wobble
          // artifact. Re-synchronize by searching nearby for a better match
          // against what the other grain currently reads, using channel 0 as
          // the reference for all channels (see findBestAlignedPos above).
          //
          // Over one grain lifetime the read pointer drifts from real time
          // by `grainSamples * (pitchRatio - 1)`. This project's ratios stay
          // within about +/-6%, so that drift is a small fraction of
          // `grainSamples` - nowhere near enough for the search below to
          // catch up to (pitchRatio > 1) or fall out of (pitchRatio < 1) the
          // circular buffer's safe lookback margin.
          const naturalPos = (this.writeIndex - this.grainSamples + this.bufferSize) % this.bufferSize;
          const otherGrain = 1 - g;
          this.grainReadPos[g] = this.findBestAlignedPos(
            this.channelBuffers[0],
            naturalPos,
            this.grainReadPos[otherGrain],
          );
        }
      }

      this.writeIndex = (this.writeIndex + 1) % this.bufferSize;
    }

    return true;
  }
}

registerProcessor("pitch-shift-processor", PitchShiftProcessor);

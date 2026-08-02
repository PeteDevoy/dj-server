// BPM detection worker.
//
// This is a port of the analysis algorithm from BeatDetect.js
// (https://github.com/ArthurBeaulieu/BeatDetect.js, GPLv3 - see
// public/vendor/beatdetect/LICENSE), specifically its private
// _getPeaks/_getIntervals/_getOffsets/_getLowestTimeOffset/_floatRound
// methods (public/vendor/beatdetect/BeatDetect.js), adapted into plain
// functions that take their configuration as parameters instead of reading
// `this._x`, so they have no dependency on the class or its constructor.
//
// It's a *port*, not a direct `import`/`new Worker` of the library itself,
// because BeatDetect.js's constructor requires `window.AudioContext` and
// `window.OfflineAudioContext`, and its own track loading uses
// `XMLHttpRequest` - none of which exist inside a Worker's global scope.
// Those steps (decoding the file, rendering it through the low/high-pass
// filter chain via OfflineAudioContext) can only happen on the main thread;
// see startBpmDetection() in index.html. What actually runs here is the
// CPU-bound part - peak-picking and interval counting over the full
// rendered PCM data - which is the part actually worth moving off the main
// thread to satisfy "detection shouldn't block playing".

function getPeaks(dataL, dataR, sampleRate) {
  const partSize = sampleRate / 2;
  const parts = dataL.length / partSize;
  let peaks = [];
  for (let i = 0; i < parts; ++i) {
    let max = 0;
    for (let j = i * partSize; j < (i + 1) * partSize; ++j) {
      const volume = Math.max(Math.abs(dataL[j]), Math.abs(dataR[j]));
      if (!max || volume > max.volume) {
        max = { position: j, volume };
      }
    }
    peaks.push(max);
  }
  peaks.sort((a, b) => b.volume - a.volume);
  peaks = peaks.splice(0, peaks.length * 0.5);
  peaks.sort((a, b) => a.position - b.position);
  return peaks;
}

function floatRound(value, precision) {
  const multiplier = Math.pow(10, precision || 0);
  return Math.round(value * multiplier) / multiplier;
}

function getIntervals(peaks, sampleRate, bpmRange, round, float) {
  const groups = [];
  peaks.forEach((peak, index) => {
    for (let i = 1; index + i < peaks.length && i < 10; ++i) {
      const group = {
        tempo: (60 * sampleRate) / (peaks[index + i].position - peak.position),
        count: 1,
        position: peak.position,
        peaks: [],
      };
      while (group.tempo <= bpmRange[0]) group.tempo *= 2;
      while (group.tempo > bpmRange[1]) group.tempo /= 2;
      group.tempo = round === true ? Math.round(group.tempo) : floatRound(group.tempo, float);

      const exists = groups.some((interval) => {
        if (interval.tempo === group.tempo) {
          interval.peaks.push(peak);
          ++interval.count;
          return true;
        }
        return false;
      });
      if (!exists) groups.push(group);
    }
  });
  return groups;
}

function getLowestTimeOffset(position, bpm, sampleRate, timeSignature) {
  const bpmTime = 60 / bpm;
  const firstBeatTime = position / sampleRate;
  let offset = firstBeatTime;
  while (offset >= bpmTime) offset -= bpmTime * timeSignature;
  if (offset < 0) while (offset < 0) offset += bpmTime;
  return offset;
}

function getOffsets(dataL, bpm, sampleRate, timeSignature) {
  const partSize = sampleRate / 2;
  const parts = dataL.length / partSize;
  let peaks = [];
  for (let i = 0; i < parts; ++i) {
    let max = 0;
    for (let j = i * partSize; j < (i + 1) * partSize; ++j) {
      const volume = dataL[j];
      if (!max || volume > max.volume) {
        max = { position: j - Math.round(((60 / bpm) * 0.05) * sampleRate), volume };
      }
    }
    peaks.push(max);
  }
  const unsortedPeaks = [...peaks];
  peaks.sort((a, b) => b.volume - a.volume);
  const refOffset = getLowestTimeOffset(peaks[0].position, bpm, sampleRate, timeSignature);
  let mean = 0;
  let divider = 0;
  for (let i = 0; i < peaks.length; ++i) {
    const offset = getLowestTimeOffset(peaks[i].position, bpm, sampleRate, timeSignature);
    if (offset - refOffset < 0.05 || refOffset - offset > -0.05) {
      mean += offset;
      ++divider;
    }
  }
  let i = 0;
  while (unsortedPeaks[i].volume < 0.02) ++i;
  let firstBar = unsortedPeaks[i].position / sampleRate;
  if (firstBar > mean / divider && firstBar < 60 / bpm) firstBar = mean / divider;
  return { offset: mean / divider, firstBar };
}

self.onmessage = (e) => {
  const { dataL, dataR, sampleRate, bpmRange, timeSignature, round, float } = e.data;
  try {
    const peaks = getPeaks(dataL, dataR, sampleRate);
    const top = getIntervals(peaks, sampleRate, bpmRange, round, float).sort((a, b) => b.count - a.count).slice(0, 5);
    if (!top.length) throw new Error("no beat intervals found");
    const offsets = getOffsets(dataL, top[0].tempo, sampleRate, timeSignature);
    self.postMessage({
      bpm: top[0].tempo,
      offset: floatRound(offsets.offset, float),
      firstBar: floatRound(offsets.firstBar, float),
    });
  } catch (err) {
    self.postMessage({ error: err.message ?? String(err) });
  }
};

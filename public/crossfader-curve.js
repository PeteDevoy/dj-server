// Crossfader gain curve: shared math + canvas visualization, adapted from a
// standalone reference page into a reusable module. A classic script (not
// type="module" - it needs no imports, and being classic lets the main
// script depend on its `window.crossfaderGain` synchronously without a
// module-timing dance, the same reasoning documented in the main script for
// why waveform.js/bpm-wabd-engine.js are modules but this doesn't need to be).
//
// `crossfaderGain` is exposed globally specifically so the REAL audio mixer
// (see index.html's crossfaderGainForDeck) can call the exact same formula
// the canvas draws - the picture is never just a stylized approximation of
// what's actually applied to the audio.
"use strict";

/// Equal-power sine law: gA² + gB² = 1 - the "smooth" end of the contour
/// (shape = 0).
function smoothGain(x) {
  return Math.sin((x * Math.PI) / 2);
}

/// Reaches unity within 14% of the fader's travel, then holds a plateau -
/// the "fast-cut" end of the contour (shape = 1).
function plateauGain(x) {
  const cutIn = 0.14;
  const normalized = Math.min(1, x / cutIn);
  return normalized * normalized * (3 - 2 * normalized); // smoothstep
}

/// The gain at crossfader position `x` (0..1), blended between the two
/// contour extremes by `shape` (0 = equal-power, 1 = plateau).
function crossfaderGain(x, shape) {
  return (1 - shape) * smoothGain(x) + shape * plateauGain(x);
}
window.crossfaderGain = crossfaderGain;

/// Wires `canvas` to visualize both decks' gain curves (mirrored around the
/// crossfader's centre) against crossfader position, redrawing whenever
/// `contourInput` changes locally. `amountOutput` (optional) is kept in
/// sync with the contour's percentage. Returns `{ redraw }` so the caller
/// can also force a redraw when the shape changes for a reason other than
/// this input firing its own "input" event - e.g. a value arriving from
/// another connected client.
window.setupCrossfaderCurveCanvas = function (canvas, contourInput, amountOutput) {
  const context = canvas.getContext("2d");
  const scale = 2; // canvas is stored at 2x resolution, displayed at half that in CSS pixels
  const width = canvas.width / scale;
  const height = canvas.height / scale;
  const plot = { left: 24, top: 13, right: 10, bottom: 22 };

  function point(x, y) {
    const innerWidth = width - plot.left - plot.right;
    const innerHeight = height - plot.top - plot.bottom;
    return [plot.left + x * innerWidth, plot.top + (1 - y) * innerHeight];
  }

  function drawPath(shape, invert, color) {
    context.beginPath();
    for (let i = 0; i <= 160; i += 1) {
      const x = i / 160;
      const y = crossfaderGain(invert ? 1 - x : x, shape);
      const [px, py] = point(x, y);
      if (i === 0) context.moveTo(px, py);
      else context.lineTo(px, py);
    }
    context.strokeStyle = color;
    context.lineWidth = 2;
    context.lineCap = "round";
    context.lineJoin = "round";
    context.stroke();
  }

  function draw() {
    const shape = Number(contourInput.value);
    if (amountOutput) amountOutput.value = `${Math.round(shape * 100)}%`;

    context.setTransform(scale, 0, 0, scale, 0, 0);
    context.clearRect(0, 0, width, height);

    context.strokeStyle = "#292e38";
    context.lineWidth = 0.5;
    for (const value of [0, 0.5, 1]) {
      const [x] = point(value, 0);
      const [, y] = point(0, value);
      context.beginPath();
      context.moveTo(x, plot.top);
      context.lineTo(x, height - plot.bottom);
      context.moveTo(plot.left, y);
      context.lineTo(width - plot.right, y);
      context.stroke();
    }

    // Deck A (invert=true) is at unity when x=0 (crossfader hard left, A's
    // own side) - Deck B (invert=false) mirrors it, at unity when x=1.
    drawPath(shape, true, "#55d6be");
    drawPath(shape, false, "#a78bfa");

    context.fillStyle = "#8d95a4";
    context.font = "9px ui-sans-serif, system-ui, sans-serif";
    context.textAlign = "left";
    context.fillText("gain", 4, 12);
    context.fillText("A", 5, height - plot.bottom + 3);
    context.textAlign = "right";
    context.fillText("B", width - 3, height - plot.bottom + 3);
    context.textAlign = "center";
    context.fillText("crossfader position", plot.left + (width - plot.left - plot.right) / 2, height - 6);
  }

  contourInput.addEventListener("input", draw);
  draw();
  return { redraw: draw };
};

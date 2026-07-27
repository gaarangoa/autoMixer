import assert from "node:assert/strict";
import test from "node:test";

import {
  barSeconds,
  beatSeconds,
  formatMusicalPosition,
  gridStepSeconds,
  snapSecondsToGrid,
  zoomScrollLeft,
} from "../client/src/timelineGrid.ts";

test("calculates bars and beats for common meters", () => {
  assert.equal(beatSeconds(120, { numerator: 4, denominator: 4 }), 0.5);
  assert.equal(barSeconds(120, { numerator: 4, denominator: 4 }), 2);
  assert.equal(beatSeconds(120, { numerator: 6, denominator: 8 }), 0.25);
  assert.equal(barSeconds(120, { numerator: 6, denominator: 8 }), 1.5);
});

test("formats musical positions with the selected meter and start bar", () => {
  assert.equal(formatMusicalPosition(0, 120, { numerator: 4, denominator: 4 }, 1), "1.1.1");
  assert.equal(formatMusicalPosition(2.75, 120, { numerator: 4, denominator: 4 }, 1), "2.2.3");
  assert.equal(formatMusicalPosition(1.5, 120, { numerator: 6, denominator: 8 }, 5), "6.1.1");
});

test("snaps to bars, beats, and note subdivisions", () => {
  const signature = { numerator: 4, denominator: 4 };
  assert.equal(gridStepSeconds(120, signature, "bar"), 2);
  assert.equal(gridStepSeconds(120, signature, "beat"), 0.5);
  assert.equal(gridStepSeconds(120, signature, "1/16"), 0.125);
  assert.equal(snapSecondsToGrid(1.91, 120, signature, "bar"), 2);
  assert.equal(snapSecondsToGrid(1.06, 120, signature, "1/16"), 1);
});

test("keeps the same timeline point under the zoom anchor", () => {
  const next = zoomScrollLeft(400, 250, 20, 40);
  assert.equal(next, 1050);
  assert.equal((next + 250) / 40, (400 + 250) / 20);
});

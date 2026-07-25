import assert from "node:assert/strict";
import test from "node:test";

import {
  recordingTimelineDuration,
  shouldStopPlaybackAtTimelineEnd,
} from "../client/src/timeline.ts";

test("keeps the initial scratch timeline until recording approaches its end", () => {
  assert.equal(recordingTimelineDuration(180, 149.9), 180);
});

test("extends the recording timeline in one-minute chunks", () => {
  assert.equal(recordingTimelineDuration(180, 150.1), 240);
  assert.equal(recordingTimelineDuration(240, 210.1), 300);
});

test("has no maximum recording horizon", () => {
  const threeHours = 3 * 60 * 60;
  assert.equal(recordingTimelineDuration(180, threeHours), threeHours + 60);
});

test("only normal playback stops at the timeline end", () => {
  assert.equal(shouldStopPlaybackAtTimelineEnd(180, 180, false), true);
  assert.equal(shouldStopPlaybackAtTimelineEnd(181, 180, true), false);
  assert.equal(shouldStopPlaybackAtTimelineEnd(10, 0, false), false);
});

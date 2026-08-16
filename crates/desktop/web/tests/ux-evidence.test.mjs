import assert from "node:assert/strict";
import test from "node:test";

import { evaluateLayout } from "../ux-evidence.mjs";

function passingMetrics() {
  return {
    reason: "results",
    viewport_width: 760,
    viewport_height: 540,
    device_pixel_ratio: 2,
    input_focused: true,
    launcher_fits_viewport: true,
    document_horizontal_overflow: false,
    results_horizontal_overflow: false,
    results_scroll_available: true,
    selected_result_visible: true,
    content_overflow_exercised: true,
    content_overflow_managed: true,
    result_count: 50,
  };
}

test("layout evidence passes only when focus, bounds, overflow, and result limits hold", () => {
  assert.equal(evaluateLayout(passingMetrics()).pass, true);
  assert.equal(
    evaluateLayout({ ...passingMetrics(), document_horizontal_overflow: true }).pass,
    false,
  );
  assert.equal(evaluateLayout({ ...passingMetrics(), selected_result_visible: false }).pass, false);
  assert.equal(evaluateLayout({ ...passingMetrics(), results_scroll_available: false }).pass, false);
  assert.equal(evaluateLayout({ ...passingMetrics(), result_count: 51 }).pass, false);
});

test("managed ellipsis is required for long result content", () => {
  assert.equal(evaluateLayout({ ...passingMetrics(), content_overflow_managed: false }).pass, false);
});

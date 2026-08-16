const MAX_VISIBLE_RESULTS = 50;

function inside(inner, outer) {
  if (!inner || !outer) return true;
  const tolerance = 1;
  return (
    inner.top >= outer.top - tolerance &&
    inner.left >= outer.left - tolerance &&
    inner.bottom <= outer.bottom + tolerance &&
    inner.right <= outer.right + tolerance
  );
}

export function evaluateLayout(metrics) {
  const pass =
    metrics.input_focused &&
    metrics.launcher_fits_viewport &&
    !metrics.document_horizontal_overflow &&
    !metrics.results_horizontal_overflow &&
    metrics.results_scroll_available &&
    metrics.selected_result_visible &&
    metrics.content_overflow_managed &&
    metrics.result_count <= MAX_VISIBLE_RESULTS;
  return { ...metrics, pass };
}

export function collectLayoutSnapshot(document, window, reason) {
  const documentElement = document.documentElement;
  const body = document.body;
  const launcher = document.querySelector(".launcher");
  const input = document.querySelector("#search-input");
  const results = document.querySelector("#results");
  const selected = document.querySelector('.result[aria-selected="true"]');
  const content = [...document.querySelectorAll(".result-name, .result-path")];
  const viewport = {
    top: 0,
    left: 0,
    right: documentElement.clientWidth,
    bottom: documentElement.clientHeight,
  };
  const contentOverflowManaged = content.every((element) => {
    if (element.scrollWidth <= element.clientWidth + 1) return true;
    const style = window.getComputedStyle(element);
    return (
      style.overflowX === "hidden" &&
      style.textOverflow === "ellipsis" &&
      style.whiteSpace === "nowrap"
    );
  });
  const contentOverflowExercised = content.some(
    (element) => element.scrollWidth > element.clientWidth + 1,
  );

  return evaluateLayout({
    reason,
    viewport_width: documentElement.clientWidth,
    viewport_height: documentElement.clientHeight,
    device_pixel_ratio: window.devicePixelRatio,
    input_focused: document.activeElement === input,
    launcher_fits_viewport: inside(launcher?.getBoundingClientRect(), viewport),
    document_horizontal_overflow:
      documentElement.scrollWidth > documentElement.clientWidth + 1 ||
      body.scrollWidth > body.clientWidth + 1,
    results_horizontal_overflow: results.scrollWidth > results.clientWidth + 1,
    results_scroll_available: results.scrollHeight >= results.clientHeight,
    selected_result_visible: inside(selected?.getBoundingClientRect(), results.getBoundingClientRect()),
    content_overflow_exercised: contentOverflowExercised,
    content_overflow_managed: contentOverflowManaged,
    result_count: document.querySelectorAll(".result").length,
  });
}

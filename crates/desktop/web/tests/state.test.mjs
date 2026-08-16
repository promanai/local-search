import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_VISIBLE_RESULTS,
  SearchSession,
  idleStatus,
  kindLabel,
  matchLabel,
  parentPath,
  resultStatus,
  searchModePresentation,
} from "../state.mjs";

function result(requestId, hits) {
  return { request_id: requestId, response: { hits } };
}

test("late response A never replaces accepted response B", () => {
  const session = new SearchSession();
  const a = session.begin("architecture", 1);
  const b = session.begin("architecture v2", 2);
  assert.equal(session.accept(result(b.requestId, [{ name: "B" }])), true);
  assert.equal(session.accept(result(a.requestId, [{ name: "A" }])), false);
  assert.equal(session.hits[0].name, "B");
});

test("result rendering state is hard-bounded to fifty items", () => {
  const session = new SearchSession();
  const request = session.begin("a", 1);
  const hits = Array.from({ length: 75 }, (_, index) => ({ name: String(index) }));
  assert.equal(session.accept(result(request.requestId, hits)), true);
  assert.equal(session.hits.length, MAX_VISIBLE_RESULTS);
});

test("keyboard selection remains within available results", () => {
  const session = new SearchSession();
  const request = session.begin("a", 1);
  session.accept(result(request.requestId, [{ name: "A" }, { name: "B" }]));
  assert.equal(session.moveSelection(10), 1);
  assert.equal(session.selectedHit.name, "B");
  assert.equal(session.moveSelection(-10), 0);
  assert.equal(session.selectedHit.name, "A");
});

test("clearing a query invalidates the active request", () => {
  const session = new SearchSession();
  const request = session.begin("a", 1);
  assert.equal(session.clear(), request.requestId);
  assert.equal(session.isCurrent(request.requestId), false);
  assert.equal(session.selectedHit, null);
});

test("presentation helpers are deterministic and backend-score free", () => {
  assert.equal(parentPath("C:\\Projects\\architecture.md"), "C:\\Projects");
  assert.equal(matchLabel("substring_name"), "Substring");
  assert.equal(kindLabel("file", "md"), "MD");
  assert.equal(kindLabel("directory", null), "Folder");
});

test("content mode stays explicit before and after a search", () => {
  assert.deepEqual(searchModePresentation(false), {
    namePressed: "true",
    contentPressed: "false",
    placeholder: "Search files...",
  });
  assert.deepEqual(searchModePresentation(true), {
    namePressed: "false",
    contentPressed: "true",
    placeholder: "Search document contents...",
  });
  assert.equal(idleStatus(true, true), "Contents mode · type at least 4 characters");
  assert.equal(resultStatus(true, 3, "1.2 ms"), "Found in document contents · 1.2 ms");
  assert.equal(resultStatus(true, 0, "900 µs"), "No matching content");
});

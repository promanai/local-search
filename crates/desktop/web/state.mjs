export const MAX_VISIBLE_RESULTS = 50;

export class SearchSession {
  #sequence = 0;
  #currentRequestId = null;
  #hits = [];
  #selectedIndex = -1;

  begin(query, now = Date.now()) {
    this.#sequence += 1;
    this.#currentRequestId = `ui-${now.toString(36)}-${this.#sequence.toString(36)}`;
    this.#hits = [];
    this.#selectedIndex = -1;
    return { requestId: this.#currentRequestId, query };
  }

  clear() {
    const previousRequestId = this.#currentRequestId;
    this.#currentRequestId = null;
    this.#hits = [];
    this.#selectedIndex = -1;
    return previousRequestId;
  }

  isCurrent(requestId) {
    return requestId !== null && requestId === this.#currentRequestId;
  }

  accept(result) {
    if (!result || !this.isCurrent(result.request_id)) {
      return false;
    }
    this.#hits = (result.response?.hits ?? []).slice(0, MAX_VISIBLE_RESULTS);
    this.#selectedIndex = this.#hits.length > 0 ? 0 : -1;
    return true;
  }

  moveSelection(delta) {
    if (this.#hits.length === 0) {
      this.#selectedIndex = -1;
      return -1;
    }
    const next = this.#selectedIndex + delta;
    this.#selectedIndex = Math.max(0, Math.min(this.#hits.length - 1, next));
    return this.#selectedIndex;
  }

  select(index) {
    if (!Number.isInteger(index) || index < 0 || index >= this.#hits.length) {
      return false;
    }
    this.#selectedIndex = index;
    return true;
  }

  get currentRequestId() {
    return this.#currentRequestId;
  }

  get hits() {
    return this.#hits;
  }

  get selectedIndex() {
    return this.#selectedIndex;
  }

  get selectedHit() {
    return this.#selectedIndex >= 0 ? this.#hits[this.#selectedIndex] : null;
  }
}

export function parentPath(resolvedPath) {
  const separator = Math.max(resolvedPath.lastIndexOf("\\"), resolvedPath.lastIndexOf("/"));
  return separator > 2 ? resolvedPath.slice(0, separator) : resolvedPath;
}

export function matchLabel(matchType) {
  const labels = {
    exact_name: "Exact",
    prefix_name: "Prefix",
    token_name: "Token",
    substring_name: "Substring",
    path: "Path",
    content: "Content",
  };
  return labels[matchType] ?? "Match";
}

export function kindLabel(kind, extension) {
  if (kind === "directory") return "Folder";
  if (extension) return extension.toUpperCase();
  const labels = { file: "File", symlink: "Link", special: "Special", other: "Item" };
  return labels[kind] ?? "Item";
}

export function idleStatus(serviceAvailable, contentMode) {
  if (!serviceAvailable) return "Search service unavailable";
  return contentMode
    ? "Contents mode · type at least 4 characters"
    : "Type to search your catalog";
}

export function resultStatus(contentMode, hitCount, duration) {
  if (hitCount === 0) return contentMode ? "No matching content" : "No matching files";
  return contentMode ? `Found in document contents · ${duration}` : `Found in ${duration}`;
}

export function searchModePresentation(contentMode) {
  return {
    namePressed: String(!contentMode),
    contentPressed: String(contentMode),
    placeholder: contentMode ? "Search document contents..." : "Search files...",
  };
}

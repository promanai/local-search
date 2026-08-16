import { execFileSync } from "node:child_process";

const allowedLicenses = new Set([
  "(MIT OR Apache-2.0) AND Unicode-3.0",
  "0BSD OR MIT OR Apache-2.0",
  "Apache-2.0 / MIT / MPL-2.0",
  "Apache-2.0 / MIT",
  "Apache-2.0 AND ISC",
  "Apache-2.0 AND MIT",
  "Apache-2.0 OR ISC OR MIT",
  "Apache-2.0 OR MIT",
  "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
  "Apache-2.0 WITH LLVM-exception",
  "Apache-2.0",
  "Apache-2.0/MIT",
  "BSD-2-Clause OR Apache-2.0 OR MIT",
  "BSD-3-Clause OR Apache-2.0",
  "BSD-3-Clause OR MIT OR Apache-2.0",
  "BSD-3-Clause",
  "BSL-1.0",
  "CC0-1.0 OR MIT-0 OR Apache-2.0",
  "CDLA-Permissive-2.0",
  "ISC",
  "MIT OR Apache-2.0 OR LGPL-2.1-or-later",
  "MIT OR Apache-2.0 OR Zlib",
  "MIT OR Apache-2.0",
  "MIT OR Zlib OR Apache-2.0",
  "MIT",
  "MIT-0",
  "MIT/Apache-2.0",
  "MPL-2.0",
  "Unicode-3.0",
  "Unlicense OR MIT",
  "Unlicense/MIT",
  "Zlib OR Apache-2.0 OR MIT",
  "Zlib",
  "zlib-acknowledgement OR MIT",
]);

const raw = execFileSync(
  "cargo",
  ["metadata", "--format-version", "1", "--locked"],
  { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 },
);
const metadata = JSON.parse(raw);
if (!Array.isArray(metadata.packages) || metadata.packages.length === 0 || metadata.packages.length > 1_000) {
  throw new Error("Cargo metadata package count is outside the audited bound");
}

const workspace = new Set(metadata.workspace_members);
const failures = [];
const licenseCounts = new Map();
for (const dependency of metadata.packages) {
  if (typeof dependency.name !== "string" || typeof dependency.version !== "string") {
    failures.push("malformed package identity");
    continue;
  }
  if (dependency.source === null) {
    if (!workspace.has(dependency.id)) {
      failures.push(`${dependency.name} ${dependency.version}: unaudited local path dependency`);
    }
  } else if (dependency.source !== "registry+https://github.com/rust-lang/crates.io-index") {
    failures.push(`${dependency.name} ${dependency.version}: unapproved source ${dependency.source}`);
  }
  if (typeof dependency.license !== "string" || !allowedLicenses.has(dependency.license)) {
    failures.push(`${dependency.name} ${dependency.version}: unapproved or missing license ${dependency.license}`);
  } else {
    licenseCounts.set(dependency.license, (licenseCounts.get(dependency.license) ?? 0) + 1);
  }
}

if (failures.length > 0) {
  throw new Error(`Dependency policy rejected ${failures.length} package(s):\n${failures.join("\n")}`);
}
console.log(`Dependency source/license policy: PASS (${metadata.packages.length} packages, ${licenseCounts.size} accepted expressions)`);

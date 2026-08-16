# Extraction Sandbox

Status: mandatory design for v0.2; no extractor is shipped in v0.1

## 1. Invariant

Untrusted documents and third-party `IFilter` implementations never execute inside the agent or elevated broker.

```text
Agent (normal user)
  -> Extractor Supervisor
  -> short-lived/recyclable Extractor Host (normal user, constrained)
  -> native parser or IFilter
```

The elevated WinFS broker is not part of this flow.

## 2. Job contract

An extraction job contains a current-user-accessible file reference, expected object/version, extractor/pipeline version, MIME/type hint, byte/output limits, deadline, and cancellation identifier.

The result contains bounded normalized text or a staged output handle, registered metadata, hashes, timings, extractor identity/version, and a typed terminal/retryable status.

## 3. Host controls

The supervisor enforces:

- wall-clock timeout;
- process memory limit;
- CPU/time budget where available;
- maximum input policy and output bytes;
- cancellation and process termination;
- crash detection;
- host recycling by job count and resource growth;
- no inherited privileged handles;
- restricted child-process behavior;
- bounded IPC frames;
- temporary-file cleanup.

The exact Windows sandbox/job-object/AppContainer strategy is selected by a dedicated v0.2 spike and documented before shipping IFilter support.

## 4. Failure and quarantine

Retries are keyed by object version and extractor pipeline version. Repeated timeout, crash, malformed output, or resource violation quarantines that object/extractor combination. A changed file or upgraded extractor can make it eligible again under policy.

One poisoned document cannot block a queue. Quarantine state and reason are visible to diagnostics and user settings without exposing content in telemetry.

## 5. Large-file policy

An extractor must not read an unbounded file into RAM. Per-type policy selects skip, partial extraction, streaming extraction, or explicit manual enablement. Truncation is marked in the content document.

## 6. Output trust

Extractor output is untrusted. The agent validates encoding, size, metadata keys/types, document identity/version, and protocol version before appending content mutations.

# OPS-GATE-001: Windows lifecycle evidence

Status: **transactional implementation and evidence runner PASS; elevated disposable-VM run
pending**

## Purpose

`OPS-GATE-001` proves the complete Windows package lifecycle against two distinct immutable
bundles:

```text
fresh install
    -> repair
    -> forced failure after payload copy -> baseline restored
    -> forced failure after runtime registration -> baseline restored
    -> candidate upgrade
    -> uninstall KeepIndexes
    -> candidate reinstall
    -> uninstall RemoveIndexes
```

The runner stays outside the candidate bundle so the controller is not supplied by the artifact it
is evaluating.

## Transaction boundary

Before repair or upgrade, the installer captures:

- exact scheduled-task XML and running state;
- broker service command, display name, startup mode, and running state;
- allowlisted payloads, manifest, detached signature, and install marker.

No runtime mutation begins until the payload backup completes. On failure, the installer removes
the partial candidate runtime, restores the previous payload and marker, recreates the prior service
and tasks, and preserves their prior running state. A fresh-install failure removes the exact
previously absent install and state roots. An orphan service/task without an owned install marker is
rejected before mutation.

Failure injection requires the separate explicit `-AllowLifecycleFailureInjection` authorization
and supports exactly:

- `AfterPayloadCopy`;
- `AfterRuntimeRegistration`.

The authorization does not relax bundle signature verification. Any failure-injection plan is
marked `public_release_eligible = false`.

## Running the gate

Use a clean disposable Windows VM and two bundles from different clean commits. A production gate
requires trusted Authenticode signatures; `-AllowUnsignedDevelopmentBundles` is only for local
engineering evidence.

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\packaging\invoke-ops-gate.ps1 `
  -BaselineBundlePath C:\Builds\LocalSearch-baseline `
  -CandidateBundlePath C:\Builds\LocalSearch-candidate `
  -InstallRoot C:\ProgramData\LocalSearchOpsGate\Program `
  -StateRoot C:\ProgramData\LocalSearchOpsGate\State `
  -OutputPath C:\Evidence\ops-gate-001.json `
  -ConfirmDisposableMachine
```

Use `-PlanOnly` first. Execution refuses non-elevated shells, existing install/state roots,
existing LocalSearch tasks or service, matching baseline/candidate commits, output beneath a
disposable root, and absent disposable-machine acknowledgement.

## Acceptance

The runner validates after every relevant stage:

- installed manifest and owner-bound marker match the expected commit;
- both tasks are registered for the authorized SID with `RunLevel = Limited`;
- state ACL inheritance is disabled and grants only owner plus `SYSTEM`;
- the public current-user lifecycle never installs the elevated broker;
- both forced failures restore the baseline commit and runtime;
- `KeepIndexes` preserves the marked state root;
- `RemoveIndexes` removes it;
- no service, task, or install-root orphan remains.

The JSON evidence contains commits, booleans, stage durations, and typed failure identity only. It
explicitly excludes paths, filenames, queries, and content.

## Remaining physical evidence

This workstation shell is not elevated and no disposable VM provider is installed, so the runner
has not been executed against SCM and Task Scheduler here. A trusted signed candidate, second-user
matrix, reboot/logon, and physical x64/ARM64 smoke remain release evidence rather than inferred
PASS.

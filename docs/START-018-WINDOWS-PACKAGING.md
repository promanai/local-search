# START-018: Windows package and operational lifecycle

Status: **engineering implementation PASS; signed-artifact and elevated lifecycle evidence pending**

This milestone turns the release binaries into a reproducible Windows bundle and defines guarded
install, repair, upgrade, uninstall, autostart, broker, retention, and diagnostics operations. It
does not claim that an unsigned development bundle is a public release artifact.

## Bundle contract

`packaging/build-windows-bundle.ps1` must run from a normal, non-elevated PowerShell against a
clean immutable commit. It builds the six release executables with `--locked`, copies the four
operational PowerShell files, records length and SHA-256 for the exact allowlist, and verifies the
finished manifest before producing the ZIP. Output inside the source repository is rejected.

Production signing is fail-closed at installation. When a code-signing certificate thumbprint and
timestamp server are supplied, every executable and operational script is Authenticode-signed
before its manifest hash is recorded. A detached CMS signature authenticates `manifest.json`, and
the installer requires its signer to match every trusted Authenticode payload signer. An unsigned
bundle can only be used with the explicitly named `-AllowUnsignedDevelopmentBundle` switch.

```powershell
cd C:\Projects\local_search
.\packaging\build-windows-bundle.ps1 `
  -OutputDirectory C:\Builds\LocalSearch-0.1.0-win-x64 `
  -SigningCertificateThumbprint '<CODE-SIGNING-THUMBPRINT>' `
  -TimestampServer '<RFC3161-TIMESTAMP-URL>'
```

For a local engineering artifact, omit both signing parameters. Such an artifact is unsuitable for
distribution and requires the unsigned-development exception during installation.

## Install and repair

Installation runs elevated, but the release build does not. The installer verifies the complete
bundle before any mutation, requires the authorized SID to be the interactive installing user,
uses an exact marked install/state root, applies a private state ACL for that user and `SYSTEM`,
and registers:

- `LocalSearchWinFS` as an automatic metadata-only Windows service when broker observation is
  explicitly enabled;
- `LocalSearch Agent` and `LocalSearch Desktop` as limited, interactive per-user logon tasks;
- bounded SCM restart recovery for the broker.

Repair and upgrade stop existing tasks and the broker before replacing binaries, wait for SCM
deletion to complete, and keep a temporary binary rollback copy. Fresh-install copy failure removes
only allowlisted files beneath the newly created exact root. No recursive cleanup is authorized by
an inferred path, filesystem root, or copied marker.

Review an install plan without elevation or mutation:

```powershell
.\install-windows.ps1 `
  -BundlePath C:\Builds\LocalSearch-0.1.0-win-x64 `
  -StateRoot "$env:LOCALAPPDATA\LocalSearch" `
  -EnableBrokerObservation `
  -ObserveRoot C:\ `
  -PlanOnly
```

Run the same command from an elevated PowerShell without `-PlanOnly` after reviewing the JSON. Add
`-AllowUnsignedDevelopmentBundle` only for a local bundle whose provenance you control.

### Content-enabled install

Content remains independently opt-in. First prepare a complete workspace as the normal user; the
workspace must not overlap an indexed root:

```powershell
C:\Builds\LocalSearch-0.1.0-win-x64\localsearch-content-index.exe folder-sync `
  --workspace "$env:LOCALAPPDATA\LocalSearch" `
  --root C:\Projects\PRO
```

Then install with `-EnableContent` and that same `StateRoot`. Before changing the machine, the
installer opens the content generation through the shipped reader and requires
`content-workspace.json`. A valid pre-existing folder-sync workspace may be adopted once by writing
the exact state marker. The package layout deliberately uses `graph.sqlite3`, `catalog`, and
`content-index-v1`, matching the content workspace so metadata and content projection share one
authoritative graph while retaining separate indexes and scopes.

## Uninstall and diagnostics

Uninstall always removes the per-user tasks, optional broker service, and marked program root. The
default `KeepIndexes` policy retains graph, catalog, content generations, and configuration.
`RemoveIndexes` requires an additional exact state marker with the same owner SID before recursive
deletion.

```powershell
.\uninstall-windows.ps1 -Retention KeepIndexes -PlanOnly
.\uninstall-windows.ps1 -Retention RemoveIndexes
```

The diagnostics exporter emits only OS/architecture, service booleans, component length/hash, and
aggregate file/byte counts. Its schema explicitly declares that paths, filenames, queries, and
content are absent.

```powershell
.\export-diagnostics.ps1 -OutputPath C:\Temp\localsearch-diagnostics.json
```

## Automated evidence

`packaging/test-windows-package.ps1` runs on Windows PowerShell in CI and covers:

- exact bundle allowlist, length/SHA-256 verification, tamper rejection, detached-manifest and
  payload-signature fail-closed policy;
- filesystem-root and observation-root bounds;
- Windows command-line quoting for spaces, quotes, and trailing backslashes;
- explicit observation/content planning;
- owner- and exact-root-bound deletion markers;
- both uninstall retention choices;
- diagnostics non-disclosure fixtures;
- parse compatibility of every package script.

## Remaining release evidence

The current shell is non-elevated and no release code-signing certificate is available. Therefore
these rows remain physical/release gates rather than inferred PASS results:

1. execute clean install, repair, upgrade, forced-failure rollback, and both uninstall modes on a
   disposable Windows VM;
2. inspect actual SCM/task ACLs and exercise service recovery as the authorized and a second user;
3. build and verify a timestamped signed x64 artifact, then smoke the exact artifact on x64 and
   physical ARM64 Windows;
4. accept the separate multi-user broker policy before enabling machine-wide multi-user installs.

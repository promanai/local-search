# SECURITY-001: v0.1 multi-user metadata policy

Status: **engineering policy PASS; second-user installation evidence pending**

Review date: 2026-08-16

## Decision

The v0.1 public package is per-user and fail-closed:

- production installation does not start or connect to the elevated WinFS broker;
- graph and content discovery run under the current user's token, so Windows evaluates that
  user's ACL, EFS, availability, and placeholder policy;
- each user owns an independent state root under that user's profile;
- the Agent pipe, scheduled tasks, state ACL, and destructive state marker are bound to the same
  logon SID;
- Desktop, CLI, and MCP remain read-only clients of that user's Agent;
- no v0.1 process listens on TCP.

This policy deliberately trades elevated whole-volume MFT coverage for a security boundary that can
be explained and enforced. An administrator, another interactive user, or an abandoned service
configuration must not cause elevated metadata to enter a public-release index.

## Elevated broker boundary

The WinFS broker remains available for controlled engineering and performance evidence, but it is
not part of the v0.1 public security boundary. Package planning rejects
`-EnableBrokerObservation` unless the operator also supplies the conspicuous
`-AllowElevatedMetadataDevelopmentMode` override. Such a plan records:

```text
metadata_visibility_policy = development-elevated-single-sid
public_release_eligible     = false
```

Without broker observation, the plan records:

```text
metadata_visibility_policy = current-user-token-only
public_release_eligible     = true
```

The override does not weaken transport controls: the broker still accepts one explicit logon SID,
uses an explicit pipe DACL and post-connect impersonation check, rejects remote clients, exposes
metadata-only bounded operations, and never reads file content.

## Lifecycle and ACL changes

- New and rebuilt content is opened under the current user's token.
- Search results are not authorization. Every result action re-resolves the current
  `DocumentId`; missing, moved, offline, or inaccessible objects fail closed.
- A subsequent current-user scan removes objects that disappeared or became inaccessible.
- Removing a Windows user does not transfer that user's index to another user. State ACLs grant
  only that owner SID and `SYSTEM`; cleanup requires the exact owner/root marker contract.
- Administrators and `SYSTEM` remain operating-system trust principals. LocalSearch does not
  claim encryption against machine administrators.

## Automated evidence

The Windows package contract verifies:

- a broker plan without the development override is rejected;
- the override without broker observation is rejected;
- a broker plan is marked ineligible for public release;
- the default plan is current-user-only and release-eligible;
- state ACL inheritance is disabled and grants only owner plus `SYSTEM`;
- copied or wrong-owner markers cannot authorize deletion;
- unsigned production installation fails closed;
- diagnostics contain no paths, filenames, queries, or content.

Agent/broker contract tests additionally cover unauthorized pipe clients, explicit SID
authorization, malformed/version/replay rejection, bounded frames, and absence of content-read
operations.

## Remaining release evidence

On a disposable Windows VM, install as user A and verify that user B cannot open the Agent or broker
pipe, read the state directory, run the registered tasks, or obtain user A metadata. Repeat after
repair, upgrade, removal of user A, and uninstall. This physical row is required for
`RELEASE-GATE-001`; it does not reopen the engineering policy decision above.

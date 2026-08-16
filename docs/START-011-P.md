# START-011-P — Physical power-policy evidence

Status: **IMPLEMENTED — BATTERY/OFF PASS, PHYSICAL MATRIX IN PROGRESS**

This bounded probe validates that the real Windows resource adapter reaches the portable
`SystemPressure` contract and produces the expected deterministic Governor budget. It does not
start the Agent, require administrator privileges, access indexed content, or modify Windows power
settings.

```text
GetSystemPowerStatus + portable resource snapshot
                         |
                         v
                  SystemPressure
                         |
                         v
                ResourceGovernor
                         |
                         v
        sanitized samples + policy acceptance
```

## Evidence contract

The probe records only bounded resource values and policy output:

- AC, battery, or unknown power source;
- battery percentage when Windows exposes it;
- energy-saver state;
- portable memory, CPU, and disk-pressure basis points;
- trusted local-input idle duration;
- selected mode, reason, budget, and transition sequence;
- sample availability and policy-invariant result.

The runner requires a clean Git tree, builds the exact release probe from that commit, requires at
least 80 percent of samples to match the requested physical state, rejects unavailable telemetry,
and writes a machine-readable JSON report with commit provenance.

## Physical matrix

| Power state | Evidence | Result |
| --- | --- | --- |
| AC, energy saver off | pending physical switch | pending |
| Battery, energy saver off | 30/30 matching samples, zero unavailable/violations | **PASS** |
| Battery, energy saver on | pending physical switch | pending |

Run these commands from a normal, non-elevated PowerShell. Do not change power state while one row
is sampling.

### AC power, energy saver off

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\run-start-011-power.ps1 `
  -ExpectedPowerSource Ac `
  -ExpectedEnergySaver Off
```

### Battery power, energy saver off

Unplug external power and confirm Windows energy saver is disabled:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\run-start-011-power.ps1 `
  -ExpectedPowerSource Battery `
  -ExpectedEnergySaver Off
```

### Battery power, energy saver on

Enable Windows energy saver through the normal system UI:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass `
  -File .\benchmarks\run-start-011-power.ps1 `
  -ExpectedPowerSource Battery `
  -ExpectedEnergySaver On
```

## Acceptance

Each row passes only when:

```text
sample coverage >= 75% of the requested cadence
requested physical state coverage >= 80%
unavailable resource samples = 0
policy invariant violations = 0

battery, saver off -> BATTERY, reduced budget
battery, saver on  -> background work paused
telemetry failure  -> PRESSURE, background work paused
```

`START-011-P` is supporting evidence for `START-011`; it does not replace the sustained
filesystem/projection load acceptance or justify a `START-011-PASS` tag by itself.

The accepted reports and their bounded summary are stored under
[`reports/resource/start-011-power`](../reports/resource/start-011-power/README.md).

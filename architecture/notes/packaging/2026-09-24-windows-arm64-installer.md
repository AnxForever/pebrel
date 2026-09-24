# Windows ARM64 installer

## Status

Proposed for review on 2026-09-24.

## Context

Pebrel already builds and validates a native Windows ARM64 application and portable ZIP,
but automatic installation still has no ARM64 installer asset.

## Evidence

- The native ARM64 application, Hook and console runtime already build and pass ARM64 conformance.
- The existing Windows update handoff is architecture-independent once the selected installer and installed payload are native.
- Inno Setup documents `ArchitecturesAllowed=arm64` for installers shipping ARM64 binaries.

## Decision

Parameterize the existing Inno installer by architecture instead of creating a second
installer path. The ARM64 release job reuses its already-built native payload, validates
that payload as ARM64, builds `windows-arm64-setup.exe`, and publishes it beside the ZIP.
Release discovery selects that exact asset on ARM64; Windows x64 behavior is unchanged.

Pebrel 1.9.0 remains the historical ARM64 portable-only release. Later stable manifests
require the ARM64 installer.

## Rejected alternatives

A separate ARM64 installer script and a second updater transaction were rejected because
the existing installer and handoff contracts are architecture-independent once the payload
and release asset are native.

## Consequences

Windows ARM64 gains the same installer-managed update path as x64 without introducing a second
transaction or migration implementation. Stable releases after 1.9.0 gain one additional asset.

## Validation

Run the existing packaging tests, stable-release contracts, and native Windows ARM64 CI,
including actual Inno installer generation.

## Supersedes

None.

## Revisit when

Revisit only if Windows ARM64 requires installer behavior that cannot share the existing x64
migration and update handoff.

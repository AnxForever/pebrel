# Windows ARM64 installer

## Status

Proposed for review on 2026-09-24.

## Context

Pebrel already builds and validates a native Windows ARM64 application and portable ZIP,
but automatic installation still has no ARM64 installer asset.

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

## Validation

Run the existing packaging tests, stable-release contracts, and native Windows ARM64 CI,
including actual Inno installer generation.

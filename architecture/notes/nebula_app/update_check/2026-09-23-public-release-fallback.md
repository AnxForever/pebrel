# Public release discovery when GitHub REST access is limited

## Status

Proposed for review; implemented and verified on macOS arm64.

## Context

A macOS user received HTTP 403 when checking for updates. Startup and manual
checks use the same unauthenticated GitHub REST endpoint, so shared egress IP
quota exhaustion can disable both even when the release website is reachable.

## Evidence

On 2026-09-23, the public REST endpoint returned HTTP 403 with `API rate limit
exceeded`. The official `/releases/latest` website returned HTTP 200 after
redirecting to `/Kuddev/pebrel/releases/tag/v1.9.0`. No account credentials were
needed for the website request. This is separate from macOS installer support.

## Decision

Only REST HTTP 403/429 invokes a second, bounded request to the official latest
release page. Resolve its proxy independently for github.com. The HTTP client's
final URI must name this repository, use HTTPS, and contain an exact numeric
major.minor.patch tag (optional v/V prefix). Do not parse website markup.

The fallback supplies version discovery only: it has no verified asset metadata
and returns no installer. Normal API responses retain existing installer and
SHA-256 handling. Both automatic and manual checks use this shared path.
Local update rehearsals never fall back to the public network.

## Rejected alternatives

- Embedding a GitHub token: introduces credentials and distribution risk for a
  public read-only operation.
- Treating an API failure as up to date: hides real failures and misses upgrades.
- Guessing package URLs or checksums from a tag: discovery does not authorize an
  installer. Scraping HTML would add a fragile metadata parser.
- Implementing a macOS installation transaction in this change: the reported
  failure is version discovery, not installation.

## Consequences

A limited API may add at most one ten-second website request, with at most five
redirects. If neither query succeeds, manual checking reports an error. A newer
fallback version opens the official Releases page for manual download; no
unverified package can be auto-executed. Unsupported future tag formats fail
explicitly rather than producing a guessed version.

## Validation

16 update-check regressions and 16 proxy regressions passed on macOS. Coverage
includes 403/429, ordinary API success, malformed responses, fallback failure,
redirect transport and disallowed hosts/tags. An explicit live test successfully
queried the production checker and the public fallback, both returning 1.9.0.
Independent i18n contracts passed, including zero-allocation lookup. A native
macOS test window completed the Settings check from checking to up to date
(GitHub v1.9.0), without the previous HTTP 403. No native installer transaction
or Windows/Linux runtime acceptance is claimed.

## Supersedes

None.

## Revisit when

GitHub changes the public redirect contract, release tags gain another stable
format, or a first-party metadata endpoint supplies authenticated package hashes
without requiring client credentials.

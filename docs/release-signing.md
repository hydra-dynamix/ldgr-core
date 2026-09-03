# Signed Core release operations

The binary release workflow publishes one versioned, signed Core catalog
contract. It does not use GitHub's latest-release API as an update trust root.
Every release contains the five supported platform archives, SHA-256 sidecars,
and detached Ed25519 archive signatures. The core-index.json.sig file signs
canonical catalog bytes and binds versions, paired agentctl, compatibility
schemas, archive roots, URLs, digests, and signing key IDs.

## Repository configuration

Configure these GitHub Actions secrets:

- LDGR_CATALOG_SIGNING_KEY: base64 for the 32-byte Ed25519 seed belonging to
  the currently embedded catalog trust root.
- LDGR_ARCHIVE_SIGNING_KEY: base64 for the 32-byte archive-signing seed. It
  may equal the catalog key during ordinary operation.
- LDGR_CATALOG_REPOSITORY_TOKEN: a least-privilege token that can push only
  core-index.json and core-index.json.sig to hydra-dynamix/ldgr-releases.

Set LDGR_CATALOG_SIGNING_KEY_ID and LDGR_ARCHIVE_SIGNING_KEY_ID as Actions
repository variables. Their defaults are ldgr-release-2026-01. Never store a
seed, private key, expanded secret, signature workspace, or Actions environment
capture in Git. The release tool reads seeds only from its configured
environment variables and verifies that the catalog secret derives the public
key already present in release-keyring.json.

The publication graph is intentionally strict:

1. all five jobs build paired archives and embedded RELEASE-METADATA.json;
2. one job verifies checksums/metadata, signs all archives, creates and
   independently verifies the candidate catalog, and stages release assets;
3. all five platforms install through the signed catalog, update from the
   previous supported version, validate both versions and the compatibility
   handshake, and run the rollback checkpoint suite;
4. the final job verifies all fifteen hosted assets; and
5. only its last step commits and pushes the signed catalog.

A failed job may leave hosted release assets, but startup checks cannot discover
them because the catalog is unchanged. Delete or replace failed candidate assets
before rerunning. Never manually publish a candidate catalog to bypass the gate.

## One-time initial catalog ceremony

`.github/workflows/core-catalog-bootstrap.yml` is the only supported path when
`hydra-dynamix/ldgr-releases` has no `core-index.json` or
`core-index.json.sig`. It is manually dispatched with the exact
`BOOTSTRAP-CORE-CATALOG` confirmation, runs in the protected
`core-release-signing` environment, and refuses to replace either catalog file.
It uses the same `LDGR_CATALOG_SIGNING_KEY` trust root and
`LDGR_CATALOG_REPOSITORY_TOKEN` as ordinary release publication. A missing,
malformed, or mismatched signing seed fails before any signature is uploaded;
the ceremony never creates a key or an unsigned catalog.

The reviewed input is `release/core-catalog-bootstrap-v1.json`. It pins the
GitHub release ID, tag and tag commit, asset IDs, names, sizes, archive and
checksum SHA-256 values, complete five-platform matrix, paired agentctl
provenance, and the exact compatibility-v2 and legacy profiles. Core 0.1.14 is
the complete supported historical baseline: it is the first historical
five-platform release that also contains the paired agentctl required by the
current atomic update contract. Earlier releases are retained on GitHub but
are not supported signed-catalog update inputs because their archives omit
agentctl; the ceremony must not relabel those incomplete bundles as supported.

Core 0.1.14 is the paired bootstrap archive, but its public binary predates the
`update` command and official installation receipt command. The 0.1.15
transition gate therefore verifies signed installer replacement from 0.1.14 on
all five platforms. Core 0.1.17 later exposed two release-gate defects: its
updater reordered authenticated platform metadata before staging, and installer
compatibility checks could race a background update check. Core 0.1.18 repairs
both defects. The 0.1.18 gate uses the signed installer once to replace 0.1.17;
subsequent releases must exercise ordinary self-update from 0.1.18 or newer.

The workflow retrieves assets by pinned GitHub asset ID and checks remote
release metadata before downloading. `ldgr-release --bootstrap-inventory ...`
then rechecks every byte and checksum, signs each archive with the already
trusted root, signs canonical catalog bytes, and independently verifies every
archive and catalog signature against `release-keyring.json`. Only after that
succeeds does the workflow upload all detached archive signatures, compare the
hosted bytes, and commit both catalog files as its final activation step. A
partial run may therefore leave deterministic archive signatures, but never an
active catalog. Once the catalog exists, disable the bootstrap workflow and use
ordinary append mode; append mode still requires and verifies the existing
signed catalog and accepts the bootstrap output without conversion.

## Compatibility-v2 adapter catalog publication

Adapter release publication is catalog activation, not merely uploading an
archive. Every dispatchable adapter archive contains the generated
`adapter-compatibility.json`; its release record contains the exact
`compatibility` object and the SHA-256 fingerprint of canonical compatibility
JSON. The fingerprint excludes adapter-local store descriptors, while archive
SHA-256 and signature still cover the complete bundle.

The integration workflow `.github/workflows/unified-schema-release.yml` is the
supported publisher. Its gate:

1. checks generated central-contract, Core-profile, release-set, and adapter
   sidecar output;
2. runs the complete compatibility matrix;
3. builds all five supported platforms and requires a non-empty platform matrix
   for every published adapter variant;
4. verifies packaged sidecars byte-for-byte against generated sources and emits
   one index fragment per archive;
5. verifies archive checksums/signatures and merges fragments with
   `scripts/adapter-release-metadata.py`;
6. evaluates every stable adapter variant against every released signed stable
   Core profile, rejecting stale sidecars, ambiguous same-version variants,
   missing platforms, handwritten legacy patch ranges, and incompatible
   central components;
7. signs canonical schema-v2 `index.json`; and
8. uploads release assets before the final single commit activates
   `index.json` and `index.json.sig` in `hydra-dynamix/ldgr-releases`.

For a dry-run review, dispatch the workflow with `publish: false` and retain
`database-contract-release.json`, the Core profile catalog, archive metadata,
and index fragments. Maintainers can locally inspect the generated manifest and
gate a candidate catalog with:

```sh
scripts/schema-release-manifest.py > database-contract-release.json
scripts/adapter-release-metadata.py validate \
  --index /path/to/index.json \
  --core-catalog /path/to/compatibility-core-catalog.json
```

Do not hand-edit catalog compatibility fields, add a v1 release to a schema-v2
catalog, broaden a Core patch range, or publish a partial platform set. The
schema-v2 resolver filters platform/channel/exact version and evaluates
protocol, minimum Core schema, capabilities, and central components before any
download. If multiple compatibility variants share one adapter SemVer, catalog
CI must prove that no released Core profile accepts more than one.

A failed build may leave unreachable hosted assets, but it must not activate a
catalog. If validation fails after assets upload, preserve the active prior
catalog/signature, remove or replace the failed candidate assets, repair the
source/generated metadata, and rerun the complete gate. Never roll back by
rewriting compatibility fields in the active signed index. If an activated
catalog must be superseded, publish a newly signed catalog from trusted source;
installed updates independently retain their plan-wide rollback journal and
prior receipts.

The coherent `release_set_hash` remains in release manifests and receipts for
audit provenance. It is not a v2 discovery or dispatch requirement. A Core patch
or unrelated local-store migration therefore does not require republishing an
unchanged compatible adapter merely to recreate global identity.

## Key rotation

A new key cannot authorize itself. Rotate in two releases:

1. Keep the catalog signing secret and ID on the old embedded trust root.
2. Set the archive signing secret/ID to the successor. The release tool derives
   its public key and adds it to signed release_keys; a conflicting existing ID
   fails the release.
3. Publish that transition catalog. Existing clients trust the old catalog
   signature, then accept the successor only for archives named by that catalog.
4. Embed the successor public key in release-keyring.json in a later Core
   release and wait until that release is the oldest supported updater.
5. Only then switch the catalog signing secret/ID to the successor. Keep the old
   public key embedded for the documented support window.

For emergency revocation, stop catalog publication, remove affected hosted
assets, ship a Core with a revised embedded keyring through an uncompromised
root, and resume only after the previous-supported-version matrix passes. Do not
delete an old embedded key merely to make an already published catalog
unverifiable; publish a signed superseding catalog and follow the support window.

## Mirrors and offline installation

Core and both official installers support:

- LDGR_CORE_UPDATE_INDEX for core-index.json;
- LDGR_CORE_RELEASE_KEYRING for an explicit enterprise/test keyring;
- LDGR_CORE_CATALOG_HELPER for the standard-library installer verifier; and
- LDGR_INSTALL_OFFLINE=1 for installer inputs restricted to file URLs.

Remote sources must use HTTPS, including redirects. Offline mode rejects every
remote source and requires local catalog, signature, keyring, helper, archive,
checksum, and archive-signature URLs. A mirror must copy bytes without rewriting
the catalog. If artifact URLs must change, an already trusted catalog key must
sign a new canonical catalog; mirroring only checksum files does not establish
trust.

The installer helper is part of the installer bootstrap. For a pinned or
air-gapped deployment, distribute the installer, core-catalog.py, and
release-keyring.json from the same reviewed Core tag, then configure all three
source variables with file URLs. Python 3 is a required first-install
dependency. The helper uses only the standard library and fails closed on
non-canonical Ed25519 material, unknown JSON fields, signature or digest
mismatch, unsafe tar paths/links, wrong platform, incomplete paired binaries, or
embedded metadata drift.

Before promoting a mirror, run the release_pipeline Rust test, the
tests/install_catalog.ps1 test, and a disposable signed installer rehearsal.
Preserve the candidate catalog, signatures, workflow run URL, and release commit
as operator evidence.

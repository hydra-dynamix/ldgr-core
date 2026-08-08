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

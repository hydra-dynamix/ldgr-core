#!/usr/bin/env python3
"""Resolve and verify the signed LDGR Core catalog for official installers."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import re
import sys
import tarfile
from pathlib import Path, PurePosixPath
from typing import Any

PLATFORMS = {
    "linux-x86_64", "linux-aarch64", "macos-x86_64", "macos-aarch64",
    "windows-x86_64",
}
SEMVER = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")

# RFC 8032 verification, kept standard-library-only so a first installation
# does not need an existing LDGR binary or a package-manager crypto module.
Q = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = (-121665 * pow(121666, Q - 2, Q)) % Q
I = pow(2, (Q - 1) // 4, Q)


class ContractError(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ContractError(message)


def inverse(value: int) -> int:
    return pow(value, Q - 2, Q)


def recover_x(y: int) -> int:
    xx = ((y * y - 1) * inverse(D * y * y + 1)) % Q
    x = pow(xx, (Q + 3) // 8, Q)
    if (x * x - xx) % Q:
        x = (x * I) % Q
    require((x * x - xx) % Q == 0, "Ed25519 point is not on the curve")
    return Q - x if x & 1 else x


BY = (4 * inverse(5)) % Q
BX = recover_x(BY)
BASE = (BX, BY, 1, (BX * BY) % Q)
IDENTITY = (0, 1, 1, 0)


def point_add(left: tuple[int, ...], right: tuple[int, ...]) -> tuple[int, ...]:
    x1, y1, z1, t1 = left
    x2, y2, z2, t2 = right
    a = ((y1 - x1) * (y2 - x2)) % Q
    b = ((y1 + x1) * (y2 + x2)) % Q
    c = (2 * D * t1 * t2) % Q
    d = (2 * z1 * z2) % Q
    e, f, g, h = b - a, d - c, d + c, b + a
    return ((e * f) % Q, (g * h) % Q, (f * g) % Q, (e * h) % Q)


def scalar_mult(point: tuple[int, ...], scalar: int) -> tuple[int, ...]:
    result, addend = IDENTITY, point
    while scalar:
        if scalar & 1:
            result = point_add(result, addend)
        addend = point_add(addend, addend)
        scalar >>= 1
    return result


def encode_point(point: tuple[int, ...]) -> bytes:
    x, y, z, _ = point
    zi = inverse(z)
    x, y = (x * zi) % Q, (y * zi) % Q
    return int.to_bytes(y | ((x & 1) << 255), 32, "little")


def decode_point(encoded: bytes) -> tuple[int, ...]:
    require(len(encoded) == 32, "Ed25519 point must be 32 bytes")
    raw = int.from_bytes(encoded, "little")
    y = raw & ((1 << 255) - 1)
    require(y < Q, "Ed25519 point encoding is not canonical")
    x = recover_x(y)
    if (x & 1) != (raw >> 255):
        x = Q - x
    require(not (x == 0 and raw >> 255), "Ed25519 point sign is not canonical")
    return (x, y, 1, (x * y) % Q)


def verify_ed25519(public_key: bytes, signature: bytes, message: bytes) -> None:
    require(len(public_key) == 32, "Ed25519 public key must be 32 bytes")
    require(len(signature) == 64, "Ed25519 signature must be 64 bytes")
    encoded_r, encoded_s = signature[:32], signature[32:]
    scalar_s = int.from_bytes(encoded_s, "little")
    require(scalar_s < L, "Ed25519 signature scalar is not canonical")
    public_point, r_point = decode_point(public_key), decode_point(encoded_r)
    identity = encode_point(IDENTITY)
    require(
        encode_point(scalar_mult(public_point, L)) == identity,
        "Ed25519 public key is not in the prime-order subgroup",
    )
    require(
        encode_point(scalar_mult(r_point, L)) == identity,
        "Ed25519 signature R is not in the prime-order subgroup",
    )
    challenge = int.from_bytes(
        hashlib.sha512(encoded_r + public_key + message).digest(), "little"
    ) % L
    expected = point_add(r_point, scalar_mult(public_point, challenge))
    require(
        encode_point(scalar_mult(BASE, scalar_s)) == encode_point(expected),
        "Ed25519 signature did not verify",
    )


def read_json(path: Path, subject: str) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ContractError(f"{subject} is not valid UTF-8 JSON: {error}") from error


def exact_object(value: Any, keys: set[str], subject: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{subject} must be an object")
    actual = set(value)
    require(
        actual == keys,
        f"{subject} fields differ: expected {sorted(keys)}, got {sorted(actual)}",
    )
    return value


def text(value: Any, subject: str) -> str:
    require(
        isinstance(value, str) and value.strip() == value and value,
        f"{subject} must be non-empty text",
    )
    return value


def semver(value: Any, subject: str) -> tuple[Any, ...]:
    value = text(value, subject)
    match = SEMVER.fullmatch(value)
    require(match is not None, f"{subject} must be a semantic version")
    major, minor, patch = (int(match.group(index)) for index in range(1, 4))
    prerelease = match.group(4)
    if prerelease is None:
        pre_key: tuple[Any, ...] = (1,)
    else:
        identifiers = []
        for item in prerelease.split("."):
            require(
                not (item.isdigit() and len(item) > 1 and item.startswith("0")),
                f"{subject} has a non-canonical numeric prerelease identifier",
            )
            identifiers.append((0, int(item)) if item.isdigit() else (1, item))
        pre_key = (0, *identifiers)
    return major, minor, patch, pre_key


def decode_base64(value: Any, size: int, subject: str) -> bytes:
    try:
        decoded = base64.b64decode(text(value, subject), validate=True)
    except (ValueError, base64.binascii.Error) as error:
        raise ContractError(f"{subject} is not canonical base64") from error
    require(len(decoded) == size, f"{subject} must decode to {size} bytes")
    return decoded


def load_keyring(value: Any) -> dict[str, bytes]:
    keyring = exact_object(value, {"keys"}, "release keyring")
    require(
        isinstance(keyring["keys"], list) and keyring["keys"],
        "release keyring keys must be a non-empty array",
    )
    keys: dict[str, bytes] = {}
    for index, item in enumerate(keyring["keys"]):
        item = exact_object(
            item, {"key_id", "public_key"}, f"release keyring keys[{index}]"
        )
        key_id = text(item["key_id"], f"release keyring keys[{index}].key_id")
        require(key_id not in keys, f"duplicate release key id {key_id}")
        keys[key_id] = decode_base64(
            item["public_key"], 32, f"release key {key_id}"
        )
    return keys


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")


def verify_envelope(
    envelope_value: Any, keys: dict[str, bytes], message: bytes, subject: str
) -> str:
    envelope = exact_object(
        envelope_value,
        {"algorithm", "key_id", "signature"},
        f"{subject} signature",
    )
    require(
        envelope["algorithm"] == "Ed25519",
        f"{subject} signature algorithm must be Ed25519",
    )
    key_id = text(envelope["key_id"], f"{subject} signature key_id")
    require(key_id in keys, f"{subject} signature names unknown key {key_id}")
    verify_ed25519(
        keys[key_id],
        decode_base64(envelope["signature"], 64, f"{subject} signature"),
        message,
    )
    return key_id


def validate_url(value: Any, offline: bool, subject: str) -> str:
    value = text(value, subject)
    if offline:
        require(value.startswith("file://"), f"offline {subject} must use file://")
    else:
        require(
            value.startswith("https://") or value.startswith("file://"),
            f"{subject} must use HTTPS or file://",
        )
    return value


def validate_catalog(value: Any, offline: bool) -> dict[str, Any]:
    catalog = exact_object(
        value, {"schema_version", "release_keys", "releases"}, "Core catalog"
    )
    require(catalog["schema_version"] == 1, "unsupported Core catalog schema")
    require(isinstance(catalog["release_keys"], list), "release_keys must be an array")
    require(
        isinstance(catalog["releases"], list) and catalog["releases"],
        "Core catalog releases must be non-empty",
    )
    versions: set[str] = set()
    for release_index, value in enumerate(catalog["releases"]):
        subject = f"releases[{release_index}]"
        release = exact_object(
            value,
            {
                "version", "channel", "minimum_updater_version", "core_commit",
                "source_repository", "agentctl", "compatibility", "platforms",
            },
            subject,
        )
        version = text(release["version"], f"{subject}.version")
        version_key = semver(version, f"{subject}.version")
        require(version not in versions, f"duplicate Core version {version}")
        versions.add(version)
        require(
            release["channel"] in {"stable", "prerelease"},
            f"{subject}.channel is invalid",
        )
        require(
            (release["channel"] == "stable") == (version_key[3] == (1,)),
            f"{subject}.channel does not match its semantic version",
        )
        semver(release["minimum_updater_version"], f"{subject}.minimum_updater_version")
        require(COMMIT.fullmatch(text(release["core_commit"], f"{subject}.core_commit")) is not None, f"{subject}.core_commit is invalid")
        require(REPOSITORY.fullmatch(text(release["source_repository"], f"{subject}.source_repository")) is not None, f"{subject}.source_repository is invalid")
        agentctl = exact_object(release["agentctl"], {"version", "repository", "commit"}, f"{subject}.agentctl")
        semver(agentctl["version"], f"{subject}.agentctl.version")
        require(REPOSITORY.fullmatch(text(agentctl["repository"], f"{subject}.agentctl.repository")) is not None, f"{subject}.agentctl.repository is invalid")
        require(COMMIT.fullmatch(text(agentctl["commit"], f"{subject}.agentctl.commit")) is not None, f"{subject}.agentctl.commit is invalid")
        compatibility = exact_object(release["compatibility"], {"launcher_compatibility_schema", "error_recovery_schema", "release_metadata_schema"}, f"{subject}.compatibility")
        require(compatibility == {"launcher_compatibility_schema": "ldgr.launcher-compatibility.v1", "error_recovery_schema": 1, "release_metadata_schema": 1}, f"{subject}.compatibility is unsupported")
        require(isinstance(release["platforms"], list) and release["platforms"], f"{subject}.platforms must be non-empty")
        seen: set[str] = set()
        for platform_index, value in enumerate(release["platforms"]):
            platform_subject = f"{subject}.platforms[{platform_index}]"
            platform = exact_object(value, {"platform", "archive_url", "archive_root", "sha256", "signature_url", "signing_key_id"}, platform_subject)
            tag = text(platform["platform"], f"{platform_subject}.platform")
            require(tag in PLATFORMS and tag not in seen, f"{platform_subject}.platform is invalid or duplicated")
            seen.add(tag)
            require(platform["archive_root"] == f"ldgr-core-{version}", f"{platform_subject}.archive_root is not version-bound")
            validate_url(platform["archive_url"], offline, f"{platform_subject}.archive_url")
            validate_url(platform["signature_url"], offline, f"{platform_subject}.signature_url")
            require(SHA256.fullmatch(text(platform["sha256"], f"{platform_subject}.sha256")) is not None, f"{platform_subject}.sha256 is invalid")
            text(platform["signing_key_id"], f"{platform_subject}.signing_key_id")
    return catalog


def resolve(args: argparse.Namespace) -> None:
    catalog = validate_catalog(read_json(args.catalog, "Core catalog"), args.offline)
    trusted = load_keyring(read_json(args.keyring, "Core release keyring"))
    catalog_key_id = verify_envelope(
        read_json(args.signature, "Core catalog signature"),
        trusted,
        canonical_json(catalog),
        "Core catalog",
    )
    if catalog["release_keys"]:
        for key_id, key in load_keyring({"keys": catalog["release_keys"]}).items():
            require(key_id not in trusted or trusted[key_id] == key, f"catalog key {key_id} conflicts with a trusted key")
            trusted[key_id] = key
    candidates = []
    for release in catalog["releases"]:
        if args.version and release["version"] != args.version:
            continue
        if not args.version and release["channel"] != "stable" and not args.prerelease:
            continue
        platform = next((item for item in release["platforms"] if item["platform"] == args.platform), None)
        if platform is not None:
            candidates.append((semver(release["version"], "selected release version"), release, platform))
    require(candidates, f"no signed Core release matches platform {args.platform}")
    _, release, platform = max(candidates, key=lambda item: item[0])
    key_id = platform["signing_key_id"]
    require(key_id in trusted, f"selected archive names unknown key {key_id}")
    resolved = {
        "schema_version": 1,
        "catalog_signing_key_id": catalog_key_id,
        "version": release["version"],
        "channel": release["channel"],
        "minimum_updater_version": release["minimum_updater_version"],
        "core_commit": release["core_commit"],
        "source_repository": release["source_repository"],
        "agentctl": release["agentctl"],
        "compatibility": release["compatibility"],
        "platform": platform,
        "archive_signing_public_key": base64.b64encode(trusted[key_id]).decode("ascii"),
    }
    args.output.write_text(json.dumps(resolved, sort_keys=True, indent=2) + "\n", encoding="utf-8")


def safe_members(archive: tarfile.TarFile, root: str) -> dict[str, tarfile.TarInfo]:
    members: dict[str, tarfile.TarInfo] = {}
    prefix = f"{root}/"
    for member in archive.getmembers():
        path = PurePosixPath(member.name)
        require(not path.is_absolute() and ".." not in path.parts, f"archive contains unsafe path {member.name}")
        normalized = str(path).rstrip("/")
        require(normalized == root or normalized.startswith(prefix), f"archive path escapes declared root: {member.name}")
        require(not (member.issym() or member.islnk() or member.isdev()), f"archive contains unsupported link or device: {member.name}")
        members[normalized] = member
    return members


def verify_archive(args: argparse.Namespace) -> None:
    resolved = exact_object(
        read_json(args.resolved, "resolved Core release"),
        {"schema_version", "catalog_signing_key_id", "version", "channel", "minimum_updater_version", "core_commit", "source_repository", "agentctl", "compatibility", "platform", "archive_signing_public_key"},
        "resolved Core release",
    )
    require(resolved["schema_version"] == 1, "unsupported resolved release schema")
    platform = resolved["platform"]
    archive_bytes = args.archive.read_bytes()
    actual = hashlib.sha256(archive_bytes).hexdigest()
    require(actual == platform["sha256"], f"archive SHA-256 mismatch: expected {platform['sha256']}, got {actual}")
    require(args.checksum.read_text(encoding="utf-8").split()[0].lower() == platform["sha256"], "checksum sidecar does not match the signed catalog")
    archive_key = {platform["signing_key_id"]: decode_base64(resolved["archive_signing_public_key"], 32, "archive signing public key")}
    envelope = read_json(args.signature, "Core archive signature")
    require(envelope.get("key_id") == platform["signing_key_id"], "archive signature key id does not match the signed catalog")
    verify_envelope(envelope, archive_key, archive_bytes, "Core archive")
    root = platform["archive_root"]
    with tarfile.open(args.archive, mode="r:gz") as archive:
        members = safe_members(archive, root)
        metadata_name = f"{root}/RELEASE-METADATA.json"
        require(metadata_name in members and members[metadata_name].isfile(), "archive is missing RELEASE-METADATA.json")
        require(members[metadata_name].size <= 64 * 1024, "archive release metadata is oversized")
        metadata_file = archive.extractfile(members[metadata_name])
        require(metadata_file is not None, "archive release metadata is unreadable")
        metadata = json.loads(metadata_file.read().decode("utf-8"))
        expected = {
            "schema_version": resolved["compatibility"]["release_metadata_schema"],
            "package": "ldgr-core", "binary": "ldgr", "version": resolved["version"],
            "agentctl_version": resolved["agentctl"]["version"],
            "agentctl_repository": resolved["agentctl"]["repository"],
            "agentctl_commit": resolved["agentctl"]["commit"],
            "launcher_compatibility_schema": resolved["compatibility"]["launcher_compatibility_schema"],
            "error_recovery_schema": resolved["compatibility"]["error_recovery_schema"],
            "platform": platform["platform"], "commit": resolved["core_commit"],
            "source_repository": resolved["source_repository"],
        }
        require(metadata == expected, "embedded RELEASE-METADATA.json does not match the signed catalog")
        extension = ".exe" if platform["platform"].startswith("windows-") else ""
        for binary in (f"ldgr{extension}", f"agentctl{extension}"):
            name = f"{root}/{platform['platform']}/{binary}"
            require(name in members and members[name].isfile(), f"archive is missing paired binary {binary}")


def field(args: argparse.Namespace) -> None:
    resolved = read_json(args.resolved, "resolved Core release")
    fields = {
        "version": resolved["version"],
        "agentctl_version": resolved["agentctl"]["version"],
        "archive_url": resolved["platform"]["archive_url"],
        "signature_url": resolved["platform"]["signature_url"],
        "sha256": resolved["platform"]["sha256"],
        "signing_key_id": resolved["platform"]["signing_key_id"],
        "archive_root": resolved["platform"]["archive_root"],
    }
    print(fields[args.name])


def build_parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    command = commands.add_parser("resolve")
    command.add_argument("--catalog", type=Path, required=True)
    command.add_argument("--signature", type=Path, required=True)
    command.add_argument("--keyring", type=Path, required=True)
    command.add_argument("--platform", choices=sorted(PLATFORMS), required=True)
    command.add_argument("--version")
    command.add_argument("--prerelease", action="store_true")
    command.add_argument("--offline", action="store_true")
    command.add_argument("--output", type=Path, required=True)
    command.set_defaults(run=resolve)
    command = commands.add_parser("verify-archive")
    command.add_argument("--resolved", type=Path, required=True)
    command.add_argument("--archive", type=Path, required=True)
    command.add_argument("--checksum", type=Path, required=True)
    command.add_argument("--signature", type=Path, required=True)
    command.set_defaults(run=verify_archive)
    command = commands.add_parser("field")
    command.add_argument("--resolved", type=Path, required=True)
    command.add_argument(
        "--name",
        choices=[
            "version", "agentctl_version", "archive_url", "signature_url",
            "sha256", "signing_key_id", "archive_root",
        ],
        required=True,
    )
    command.set_defaults(run=field)
    return root


def main() -> int:
    try:
        args = build_parser().parse_args()
        args.run(args)
        return 0
    except (ContractError, OSError, UnicodeError, json.JSONDecodeError, tarfile.TarError, KeyError, IndexError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Validate, render, package, publish, and verify LaserStream core crates."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERSION_FILE = ROOT / "VERSION"
TEMPLATE_VERSION = "0.0.0"
SEMVER = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
CRATES = ("laserstream-core-proto", "laserstream-core-client")
MANIFESTS = {
    "laserstream-core-proto": ROOT / "yellowstone-grpc-proto/Cargo.toml",
    "laserstream-core-client": ROOT / "yellowstone-grpc-client/Cargo.toml",
}


def run(command: list[str], *, capture: bool = False) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
    )


def parse_version(value: str) -> str:
    if SEMVER.fullmatch(value) is None:
        raise ValueError(f"version must be MAJOR.MINOR.PATCH, found {value!r}")
    return value


def canonical_version() -> str:
    return parse_version(VERSION_FILE.read_text().strip())


def package_version(path: Path, package_name: str) -> str:
    import tomllib

    data = tomllib.loads(path.read_text())
    for package in data["package"]:
        if package["name"] == package_name:
            return package["version"]
    raise ValueError(f"{path}: package {package_name!r} not found")


def recorded_versions() -> dict[str, str]:
    import tomllib

    root = tomllib.loads((ROOT / "Cargo.toml").read_text())
    proto = tomllib.loads(MANIFESTS["laserstream-core-proto"].read_text())
    client = tomllib.loads(MANIFESTS["laserstream-core-client"].read_text())
    wasm = tomllib.loads((ROOT / "yellowstone-grpc-client-nodejs/solana-encoding-wasm/Cargo.toml").read_text())
    return {
        "yellowstone-grpc-proto/Cargo.toml": proto["package"]["version"],
        "yellowstone-grpc-client/Cargo.toml": client["package"]["version"],
        "Cargo.toml client dependency": root["workspace"]["dependencies"]["yellowstone-grpc-client"]["version"],
        "Cargo.toml proto dependency": root["workspace"]["dependencies"]["yellowstone-grpc-proto"]["version"],
        "Cargo.lock client package": package_version(ROOT / "Cargo.lock", "laserstream-core-client"),
        "Cargo.lock proto package": package_version(ROOT / "Cargo.lock", "laserstream-core-proto"),
        "wasm Cargo.toml proto dependency": wasm["dependencies"]["yellowstone-grpc-proto"]["version"],
        "wasm Cargo.lock proto package": package_version(
            ROOT / "yellowstone-grpc-client-nodejs/solana-encoding-wasm/Cargo.lock",
            "laserstream-core-proto",
        ),
    }


def check_versions(expected: str) -> None:
    mismatches = {location: actual for location, actual in recorded_versions().items() if actual != expected}
    if mismatches:
        for location, actual in mismatches.items():
            print(f"{location}: expected {expected}, found {actual}", file=sys.stderr)
        raise SystemExit(1)
    print(f"All LaserStream core versions match {expected}")


def replace_once(path: Path, content: str, pattern: str, version: str) -> str:
    updated, count = re.subn(pattern, rf"\g<1>{version}\g<2>", content, count=1, flags=re.MULTILINE)
    if count != 1:
        raise ValueError(f"{path}: expected one matching version, replaced {count}")
    return updated


def rendered_files(version: str) -> dict[Path, str]:
    parse_version(version)
    replacements = (
        (MANIFESTS["laserstream-core-proto"], r'(?s)(\[package\].*?\nversion = ")[^"]+("\n)'),
        (MANIFESTS["laserstream-core-client"], r'(?s)(\[package\].*?\nversion = ")[^"]+("\n)'),
        (ROOT / "Cargo.toml", r'^(yellowstone-grpc-client = \{[^\n]*version = ")[^"]+("[^\n]*\})$'),
        (ROOT / "Cargo.toml", r'^(yellowstone-grpc-proto = \{[^\n]*version = ")[^"]+("[^\n]*\})$'),
        (ROOT / "Cargo.lock", r'(?s)(\[\[package\]\]\nname = "laserstream-core-client"\nversion = ")[^"]+("\n)'),
        (ROOT / "Cargo.lock", r'(?s)(\[\[package\]\]\nname = "laserstream-core-proto"\nversion = ")[^"]+("\n)'),
        (
            ROOT / "yellowstone-grpc-client-nodejs/solana-encoding-wasm/Cargo.toml",
            r'^(yellowstone-grpc-proto = \{[^\n]*version = ")[^"]+("[^\n]*\})$',
        ),
        (
            ROOT / "yellowstone-grpc-client-nodejs/solana-encoding-wasm/Cargo.lock",
            r'(?s)(\[\[package\]\]\nname = "laserstream-core-proto"\nversion = ")[^"]+("\n)',
        ),
    )
    outputs: dict[Path, str] = {}
    for path, pattern in replacements:
        content = outputs.get(path, path.read_text())
        outputs[path] = replace_once(path, content, pattern, version)
    return outputs


def write_atomically(outputs: dict[Path, str]) -> None:
    temporary: list[tuple[Path, Path]] = []
    try:
        for path, content in outputs.items():
            handle, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
            temporary_path = Path(name)
            with os.fdopen(handle, "w") as file:
                file.write(content)
            temporary.append((path, temporary_path))
        for path, temporary_path in temporary:
            os.replace(temporary_path, path)
    finally:
        for _, temporary_path in temporary:
            temporary_path.unlink(missing_ok=True)


def render() -> None:
    check_versions(TEMPLATE_VERSION)
    version = canonical_version()
    write_atomically(rendered_files(version))
    check_versions(version)


def request_json(url: str) -> dict | None:
    request = urllib.request.Request(url, headers={"User-Agent": "laserstream-core-release"})
    last_error = "unknown registry error"
    for attempt in range(4):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            if error.code != 429 and error.code < 500:
                raise RuntimeError(f"registry request failed with HTTP {error.code}: {url}") from error
            last_error = f"HTTP {error.code}"
        except urllib.error.URLError as error:
            last_error = str(error.reason)
        if attempt < 3:
            time.sleep(2**attempt)
    raise RuntimeError(f"registry request failed after retries: {url}: {last_error}")


def crate_metadata(crate: str, version: str) -> dict | None:
    return request_json(f"https://crates.io/api/v1/crates/{crate}/{version}")


def published_crate_sha(crate: str, version: str) -> str:
    with tempfile.TemporaryDirectory() as directory:
        request = urllib.request.Request(
            f"https://crates.io/api/v1/crates/{crate}/{version}/download",
            headers={"User-Agent": "laserstream-core-release"},
        )
        archive_path = Path(directory) / "crate.tar.gz"
        with urllib.request.urlopen(request, timeout=60) as response:
            archive_path.write_bytes(response.read())
        with tarfile.open(archive_path, "r:gz") as archive:
            member = next((item for item in archive.getmembers() if item.name.endswith("/.cargo_vcs_info.json")), None)
            if member is None:
                raise RuntimeError(f"{crate}@{version} is missing .cargo_vcs_info.json")
            file = archive.extractfile(member)
            if file is None:
                raise RuntimeError(f"could not read {crate}@{version} provenance")
            return json.load(file)["git"]["sha1"]


def check_release_source(tag: str, sha: str) -> None:
    version = canonical_version()
    check_versions(version)
    if tag != f"laserstream-core-v{version}":
        raise SystemExit(f"release tag {tag!r} must be laserstream-core-v{version}")
    head = run(["git", "rev-parse", "HEAD"], capture=True).stdout.strip()
    tag_sha = run(["git", "rev-parse", f"refs/tags/{tag}^{{}}"], capture=True).stdout.strip()
    if head != sha or tag_sha != sha:
        raise SystemExit(f"release SHA mismatch: event={sha}, checkout={head}, tag={tag_sha}")
    run([
        "git",
        "fetch",
        "--force",
        "origin",
        "+refs/heads/laserstream-core/main:refs/remotes/origin/laserstream-core/main",
    ])
    if subprocess.run(
        ["git", "merge-base", "--is-ancestor", sha, "origin/laserstream-core/main"],
        cwd=ROOT,
    ).returncode != 0:
        raise SystemExit(f"release commit {sha} is not on laserstream-core/main")
    for crate in CRATES:
        if crate_metadata(crate, version) is not None:
            published_sha = published_crate_sha(crate, version)
            if published_sha != sha:
                raise SystemExit(f"{crate}@{version} came from {published_sha}, expected {sha}")
    print(f"Release {tag} is valid from {sha}")


def packaged_crate_sha(crate: str, version: str) -> str:
    archive_path = ROOT / "target/package" / f"{crate}-{version}.crate"
    if not archive_path.is_file():
        raise RuntimeError(f"cargo package did not create {archive_path}")
    with tarfile.open(archive_path, "r:gz") as archive:
        member = next((item for item in archive.getmembers() if item.name.endswith("/.cargo_vcs_info.json")), None)
        if member is None:
            raise RuntimeError(f"{archive_path} is missing .cargo_vcs_info.json")
        file = archive.extractfile(member)
        if file is None:
            raise RuntimeError(f"could not read provenance from {archive_path}")
        return json.load(file)["git"]["sha1"]


def prepare(sha: str) -> None:
    version = canonical_version()
    check_versions(version)
    package_dir = ROOT / "target/package"
    for crate in CRATES:
        (package_dir / f"{crate}-{version}.crate").unlink(missing_ok=True)
        command = ["cargo", "package", "--locked", "--allow-dirty", "--package", crate]
        if crate == "laserstream-core-client":
            command.append("--no-verify")
        run(command)
        packaged_sha = packaged_crate_sha(crate, version)
        if packaged_sha != sha:
            raise RuntimeError(f"{crate} package provenance is {packaged_sha}, expected {sha}")
    print(f"Prepared both LaserStream core crates from {sha}")


def publish(crate: str, sha: str) -> None:
    if crate not in CRATES:
        raise ValueError(f"unknown crate {crate!r}")
    version = canonical_version()
    for attempt in range(6):
        if crate_metadata(crate, version) is not None:
            published_sha = published_crate_sha(crate, version)
            if published_sha != sha:
                raise RuntimeError(f"{crate}@{version} came from {published_sha}, expected {sha}")
            print(f"{crate}@{version} already matches {sha}; skipping")
            return
        result = subprocess.run(
            ["cargo", "publish", "--locked", "--allow-dirty", "--package", crate],
            cwd=ROOT,
            text=True,
        )
        if result.returncode == 0:
            return
        if attempt < 5:
            time.sleep(10 * (attempt + 1))
    raise SystemExit(f"failed to publish {crate}@{version}")


def verify(sha: str) -> None:
    version = canonical_version()
    for attempt in range(24):
        if all(
            crate_metadata(crate, version) is not None and published_crate_sha(crate, version) == sha
            for crate in CRATES
        ):
            print(f"Verified both LaserStream core crates {version} from {sha}")
            return
        if attempt < 23:
            time.sleep(10)
    raise SystemExit(f"LaserStream core {version} was not fully visible with provenance {sha}")


def main() -> None:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("check-template")
    subparsers.add_parser("render")
    subparsers.add_parser("check")

    release_parser = subparsers.add_parser("preflight-release")
    release_parser.add_argument("--tag", required=True)
    release_parser.add_argument("--sha", required=True)

    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--sha", required=True)

    publish_parser = subparsers.add_parser("publish")
    publish_parser.add_argument("--crate", required=True, choices=CRATES)
    publish_parser.add_argument("--sha", required=True)

    verify_parser = subparsers.add_parser("verify")
    verify_parser.add_argument("--sha", required=True)

    args = parser.parse_args()
    if args.command == "check-template":
        canonical_version()
        check_versions(TEMPLATE_VERSION)
    elif args.command == "render":
        render()
    elif args.command == "check":
        check_versions(canonical_version())
    elif args.command == "preflight-release":
        check_release_source(args.tag, args.sha)
    elif args.command == "prepare":
        prepare(args.sha)
    elif args.command == "publish":
        publish(args.crate, args.sha)
    else:
        verify(args.sha)


if __name__ == "__main__":
    main()

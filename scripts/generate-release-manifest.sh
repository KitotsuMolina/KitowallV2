#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
WORKSPACE_VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
VERSION="${VERSION:-$WORKSPACE_VERSION}"
TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
DIST_DIR="${DIST_DIR:-$ROOT/dist}"
BINARY_PATH="${BINARY_PATH:-$ROOT/target/$TARGET/release/kitowall}"
REPOSITORY="${REPOSITORY:-KitotsuMolina/KitowallV2}"
TAG="${TAG:-v$VERSION}"
CHANNEL="${CHANNEL:-stable}"
COMMIT="${COMMIT:-$(git -C "$ROOT" rev-parse HEAD)}"
ARCHIVE_NAME="kitowall-$VERSION-$TARGET.tar.zst"
SBOM_NAME="kitowall-$VERSION-$TARGET.spdx.json"
MANIFEST_NAME="kitowall-$VERSION-$TARGET.manifest.json"
ARCHIVE_PATH="$DIST_DIR/$ARCHIVE_NAME"
SBOM_PATH="$DIST_DIR/$SBOM_NAME"
MANIFEST_PATH="$DIST_DIR/$MANIFEST_NAME"

for command in jq readelf sha256sum stat; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'Missing required command: %s\n' "$command" >&2
    exit 127
  fi
done

if [[ "$VERSION" != "$WORKSPACE_VERSION" ]]; then
  printf 'Requested version %s does not match Cargo.toml version %s\n' \
    "$VERSION" "$WORKSPACE_VERSION" >&2
  exit 1
fi

if [[ "$TAG" != "v$VERSION" ]]; then
  printf 'Tag %s does not match Cargo.toml version %s\n' "$TAG" "$VERSION" >&2
  exit 1
fi

if [[ ! "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  printf 'Commit must be a full 40-character Git SHA\n' >&2
  exit 1
fi

if [[ ! -f "$ARCHIVE_PATH" || ! -f "$SBOM_PATH" || ! -x "$BINARY_PATH" ]]; then
  printf 'Archive, SBOM, or release binary is missing\n' >&2
  exit 1
fi

DETECTED_GLIBC="$(
  readelf --version-info "$BINARY_PATH" |
    grep -oE 'GLIBC_[0-9]+\.[0-9]+' |
    sed 's/^GLIBC_//' |
    sort -V |
    tail -n 1
)"
GLIBC_MINIMUM="${GLIBC_MINIMUM:-$DETECTED_GLIBC}"

if [[ ! "$GLIBC_MINIMUM" =~ ^[0-9]+\.[0-9]+$ ]]; then
  printf 'Could not determine the minimum required glibc version\n' >&2
  exit 1
fi

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

jq -n \
  --arg version "$VERSION" \
  --arg repository "$REPOSITORY" \
  --arg tag "$TAG" \
  --arg channel "$CHANNEL" \
  --arg commit "$COMMIT" \
  --arg target "$TARGET" \
  --arg glibc_minimum "$GLIBC_MINIMUM" \
  --arg archive_name "$ARCHIVE_NAME" \
  --arg archive_sha "$(sha256 "$ARCHIVE_PATH")" \
  --argjson archive_size "$(stat -c %s "$ARCHIVE_PATH")" \
  --arg binary_sha "$(sha256 "$BINARY_PATH")" \
  --argjson binary_size "$(stat -c %s "$BINARY_PATH")" \
  --arg license_sha "$(sha256 "$ROOT/LICENSE")" \
  --argjson license_size "$(stat -c %s "$ROOT/LICENSE")" \
  --arg sbom_name "$SBOM_NAME" \
  --arg sbom_sha "$(sha256 "$SBOM_PATH")" \
  '{
    schema_version: 1,
    kind: "kitotsu.release-artifact",
    distribution_contract: "1.0",
    product: {
      id: "kitowall",
      version: $version,
      license: "MIT",
      repository: $repository,
      contract_version: "1.0"
    },
    release: {
      tag: $tag,
      channel: $channel,
      commit: $commit
    },
    platform: {
      os: "linux",
      arch: "x86_64",
      target: $target,
      libc: {
        family: "glibc",
        minimum: $glibc_minimum
      }
    },
    artifact: {
      file_name: $archive_name,
      format: "tar.zst",
      size_bytes: $archive_size,
      sha256: $archive_sha
    },
    payload: [
      {
        path: "bin/kitowall",
        kind: "executable",
        mode: "0755",
        size_bytes: $binary_size,
        sha256: $binary_sha
      },
      {
        path: "share/licenses/kitowall/LICENSE",
        kind: "license",
        mode: "0644",
        size_bytes: $license_size,
        sha256: $license_sha
      }
    ],
    entrypoints: [
      {
        name: "kitowall",
        path: "bin/kitowall"
      }
    ],
    requirements: {
      modules: [
        {
          id: "kitsune-compositor",
          constraint: ">=0.1.0, <0.2.0",
          optional: false
        }
      ],
      host_capabilities: [
        {
          id: "session.wayland",
          optional: false
        },
        {
          id: "renderer.awww",
          optional: false
        }
      ]
    },
    integrations: {
      desktop_entries: []
    },
    sbom: {
      file_name: $sbom_name,
      format: "spdx-json",
      sha256: $sbom_sha
    }
  }' >"$MANIFEST_PATH"

printf '%s\n' "$MANIFEST_PATH"

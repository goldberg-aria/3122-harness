#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-goldberg-aria/3122-harness}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
  Linux) os_target="unknown-linux-gnu" ;;
  Darwin) os_target="apple-darwin" ;;
  *)
    echo "unsupported OS: $uname_s" >&2
    exit 1
    ;;
esac

case "$uname_m" in
  x86_64|amd64) arch_target="x86_64" ;;
  arm64|aarch64) arch_target="aarch64" ;;
  *)
    echo "unsupported architecture: $uname_m" >&2
    exit 1
    ;;
esac

target="${arch_target}-${os_target}"
api_url="https://api.github.com/repos/${REPO}/releases/latest"

if command -v curl >/dev/null 2>&1; then
  response="$(curl -fsSL "$api_url")"
else
  echo "curl is required to install 3122" >&2
  exit 1
fi

tag="$(printf '%s' "$response" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
asset_name="3122-${tag}-${target}.tar.gz"
download_url="$(printf '%s' "$response" | grep -o "https://[^\"[:space:]]*${asset_name}" | head -n1)"

if [ -z "$tag" ] || [ -z "$download_url" ]; then
  echo "could not find a release asset for ${target}" >&2
  exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

archive_path="${tmp_dir}/${asset_name}"
curl -fsSL "$download_url" -o "$archive_path"
mkdir -p "$INSTALL_DIR"
tar -xzf "$archive_path" -C "$tmp_dir"
install -m 0755 "${tmp_dir}/3122" "${INSTALL_DIR}/3122"

echo "installed 3122 ${tag} to ${INSTALL_DIR}/3122"
case ":$PATH:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "add ${INSTALL_DIR} to PATH if it is not already there"
    ;;
esac

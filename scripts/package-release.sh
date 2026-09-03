#!/usr/bin/env bash
set -Eeuo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
output_dir=${1:-"$repo_root/dist"}
target=${QTR_RELEASE_TARGET:-x86_64-unknown-linux-gnu}
version=${QTR_VERSION:-}

if [[ -z "$version" ]]; then
    version=$(sed -nE 's/^version = "([^"]+)"/\1/p' "$repo_root/Cargo.toml" | head -n 1)
fi
version=${version#v}

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || {
    printf 'invalid qtr version: %s\n' "$version" >&2
    exit 1
}
[[ "$target" == "x86_64-unknown-linux-gnu" ]] || {
    printf 'unsupported qtr release target: %s\n' "$target" >&2
    exit 1
}

binary="$repo_root/target/release/qtr"
web_dir="$repo_root/web/dist"
[[ -x "$binary" ]] || {
    printf 'missing release binary: %s\n' "$binary" >&2
    exit 1
}
[[ -f "$web_dir/index.html" ]] || {
    printf 'missing Web UI build: %s/index.html\n' "$web_dir" >&2
    exit 1
}

archive_root="qtr-v${version}-${target}"
archive_name="${archive_root}.tar.gz"
temp_root=${TMPDIR:-"$repo_root/.tmp"}
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" log -1 --format=%ct)}
mkdir -p "$temp_root"
staging_dir=$(mktemp -d "$temp_root/qtr-release.XXXXXX")
trap 'rm -rf -- "$staging_dir"' EXIT

mkdir -p \
    "$staging_dir/$archive_root/bin" \
    "$staging_dir/$archive_root/share/qtr"
install -m 0755 "$binary" "$staging_dir/$archive_root/bin/qtr"
cp -a "$web_dir" "$staging_dir/$archive_root/share/qtr/web"

mkdir -p "$output_dir"
output_dir=$(cd -- "$output_dir" && pwd)
archive="$output_dir/$archive_name"

tar \
    --create \
    --directory "$staging_dir" \
    --file "$archive" \
    --gzip \
    --group 0 \
    --numeric-owner \
    --owner 0 \
    --sort name \
    --mtime "@$source_date_epoch" \
    "$archive_root"
(
    cd -- "$output_dir"
    sha256sum "$archive_name" >"$archive_name.sha256"
)

printf '%s\n' "$archive"
printf '%s\n' "$archive.sha256"

#!/usr/bin/env bash
#
# Build .deb, .rpm and .apk packages for mapshoot from the fully static musl
# binaries that cargo-dist stages in target/distrib/.
#
# cargo-dist has no built-in deb/rpm/apk support, so this runs as an
# `extra-artifacts` build step in the global build phase, where every local
# artifact has already been downloaded into target/distrib/.
#
# It also runs locally at any time, e.g.:
#     dist build --target x86_64-unknown-linux-musl
#     MAPSHOOT_PACKAGE_TARGETS=x86_64-unknown-linux-musl packaging/build-packages.sh
#
# Requires: bash >= 4, awk, tar (with xz support), gzip, curl, sha256sum, and
# `file` from file(1).
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

dist_dir=target/distrib
work_dir=target/package-work

# Targets to package. Overridable to package a subset during local testing;
# dist itself verifies that the full set of declared artifacts exists.
if [[ -n ${MAPSHOOT_PACKAGE_TARGETS:-} ]]; then
  read -r -a targets <<<"$MAPSHOOT_PACKAGE_TARGETS"
else
  targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
fi

# --- pinned, checksum-verified nfpm --------------------------------------
nfpm_version=2.48.0
nfpm_base="https://github.com/goreleaser/nfpm/releases/download/v${nfpm_version}"

case "$(uname -m)" in
    x86_64 | amd64)
        nfpm_arch=x86_64
        nfpm_sha256=6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750
        ;;
    aarch64 | arm64)
        nfpm_arch=arm64
        nfpm_sha256=a9a75213763783b7669719e505474c748c5ad7dc66591a195619deb50f85c2f4
        ;;
    *)
        echo "build-packages.sh: unsupported build host $(uname -m)" >&2
        exit 1
        ;;
esac

nfpm_dir="target/nfpm-${nfpm_version}-${nfpm_arch}"
if [[ ! -x ${nfpm_dir}/nfpm ]]; then
    echo "build-packages.sh: fetching nfpm ${nfpm_version} (${nfpm_arch})"
    tmp_dir=$(mktemp -d)
    trap 'rm -rf "$tmp_dir"' EXIT
    tarball="nfpm_${nfpm_version}_Linux_${nfpm_arch}.tar.gz"
    curl -fsSL "${nfpm_base}/${tarball}" -o "${tmp_dir}/${tarball}"
    echo "${nfpm_sha256}  ${tmp_dir}/${tarball}" | sha256sum -c - >/dev/null
    mkdir -p "$nfpm_dir"
    tar -xzf "${tmp_dir}/${tarball}" -C "$nfpm_dir" nfpm
fi
nfpm="$PWD/${nfpm_dir}/nfpm"

# --- version -------------------------------------------------------------
# Read [package] version straight from Cargo.toml: the global build job that
# runs this script has no cargo/rustup setup step of its own.
version=$(awk -F'"' '
    /^\[package\]/ { in_package = 1; next }
    /^\[/          { in_package = 0 }
    in_package && /^version[[:space:]]*=/ { print $2; exit }
' Cargo.toml)
if [[ -z ${version} ]]; then
    echo "build-packages.sh: cannot determine the crate version from Cargo.toml" >&2
    exit 1
fi

mkdir -p "$work_dir"

# --- build one package set per target ------------------------------------
packages=()
for target in "${targets[@]}"; do
    case "$target" in
        x86_64-*) goarch=amd64 ;;
        aarch64-*) goarch=arm64 ;;
        *)
            echo "build-packages.sh: unsupported target ${target}" >&2
            exit 1
            ;;
    esac

    tarball="${dist_dir}/mapshoot-${target}.tar.xz"
    if [[ ! -f ${tarball} ]]; then
        echo "build-packages.sh: missing ${tarball}; run 'dist build' first" >&2
        exit 1
    fi

    bindir="${work_dir}/${target}"
    rm -rf "$bindir"
    mkdir -p "$bindir"
    tar -xJf "$tarball" -C "$bindir" --strip-components=1

    # Accept both "statically linked" and "static-pie linked"; reject anything
    # that depends on a runtime loader.
    binary_description=$(file -b "${bindir}/mapshoot")
    if [[ $binary_description == *"dynamically linked"* ]]; then
        echo "build-packages.sh: ${target} binary is not statically linked: ${binary_description}" >&2
        exit 1
    fi

    for format in deb rpm apk; do
        out="${dist_dir}/mapshoot-${goarch}.${format}"
        ARCH="$goarch" VERSION="$version" BINDIR="$bindir" \
            "$nfpm" package --config packaging/nfpm.yaml --packager "$format" --target "$out"
        packages+=("mapshoot-${goarch}.${format}")
        echo "build-packages.sh: built ${out}"
    done
done

# --- checksums over the packages -----------------------------------------
(
    cd "$dist_dir"
    sha256sum "${packages[@]}" >mapshoot-packages.sha256
)
echo "build-packages.sh: wrote ${dist_dir}/mapshoot-packages.sha256"

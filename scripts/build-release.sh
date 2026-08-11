#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
output_dir=${1:-"$repo_root/dist"}

if [[ -n ${MLX_GUARD_MATURIN_BIN:-} ]]; then
    if [[ $("$MLX_GUARD_MATURIN_BIN" --version) != "maturin 1.13.3" ]]; then
        echo "MLX_GUARD_MATURIN_BIN must select maturin 1.13.3" >&2
        exit 1
    fi
    maturin=("$MLX_GUARD_MATURIN_BIN")
else
    maturin=(uvx --from maturin==1.13.3 maturin)
fi
if [[ -n ${MLX_GUARD_PYTHON_BIN:-} ]]; then
    python=("$MLX_GUARD_PYTHON_BIN")
else
    python=(uv run --locked python)
fi

if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo "release build requires macOS arm64" >&2
    exit 1
fi

mkdir -p "$output_dir"
if [[ -n $(find "$output_dir" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
    echo "release output directory must be empty" >&2
    exit 1
fi
package_dir="$output_dir/packages"
source_dir="$output_dir/source"
mkdir -p "$package_dir" "$source_dir"

cd "$repo_root"
version=$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)
if [[ -z $version ]]; then
    echo "workspace version is unavailable" >&2
    exit 1
fi

"${maturin[@]}" build \
    --release \
    --locked \
    --out "$package_dir"
"${maturin[@]}" sdist --out "$source_dir"

shopt -s nullglob
wheels=("$package_dir"/*.whl)
sdists=("$source_dir"/*.tar.gz)
if [[ ${#wheels[@]} -ne 1 || ${#sdists[@]} -ne 1 ]]; then
    echo "release build expected exactly one wheel and one source distribution" >&2
    exit 1
fi
wheel=${wheels[0]}
sdist=${sdists[0]}
expected_wheel="mlx_guard-${version}-py3-none-macosx_11_0_arm64.whl"
expected_sdist="mlx_guard-${version}.tar.gz"
if [[ ${wheel##*/} != "$expected_wheel" || ${sdist##*/} != "$expected_sdist" ]]; then
    echo "release artifact names do not match workspace version $version" >&2
    exit 1
fi

"${python[@]}" "$repo_root/scripts/sanitize_wheel_sbom.py" "$wheel"
sbom="$output_dir/mlx-guard-cli.cyclonedx.json"
unzip -p "$wheel" 'mlx_guard-*.dist-info/sboms/*.json' >"$sbom"

(
    cd "$output_dir"
    shasum -a 256 \
        "packages/${wheel##*/}" \
        "source/${sdist##*/}" \
        "${sbom##*/}" >SHA256SUMS
)

echo "built release artifacts in $output_dir"

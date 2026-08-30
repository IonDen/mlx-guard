#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
proof_root=$(mktemp -d "${TMPDIR:-/tmp}/mlx-guard-wheel.XXXXXX")
trap 'rm -rf -- "$proof_root"' EXIT
artifact_dir=${1:-}

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
    echo "wheel proof requires macOS arm64" >&2
    exit 1
fi

if [[ -n $artifact_dir ]]; then
    artifact_dir=$(cd "$artifact_dir" && pwd)
    wheel_dir="$artifact_dir/packages"
    sdist_dir="$artifact_dir/source"
else
    wheel_dir="$proof_root/wheel"
    sdist_dir="$proof_root/sdist"
    mkdir -p "$wheel_dir" "$sdist_dir"
fi

cd "$repo_root"
version=$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)
if [[ -z $version ]]; then
    echo "workspace version is unavailable" >&2
    exit 1
fi
package_name="mlx_guard-${version}"
if [[ -z $artifact_dir ]]; then
    "${maturin[@]}" build \
        --release \
        --locked \
        --out "$wheel_dir"
    "${maturin[@]}" sdist --out "$sdist_dir"
fi

shopt -s nullglob
wheels=("$wheel_dir"/*.whl)
if [[ ${#wheels[@]} -ne 1 ]]; then
    echo "expected exactly one wheel" >&2
    exit 1
fi
wheel=${wheels[0]}
if [[ ${wheel##*/} != "${package_name}-py3-none-macosx_11_0_arm64.whl" ]]; then
    echo "unexpected wheel tag: ${wheel##*/}" >&2
    exit 1
fi

"${python[@]}" "$repo_root/scripts/sanitize_wheel_sbom.py" "$wheel"

wheel_listing=$(unzip -Z1 "$wheel")
for required in \
    'mlx_guard/__init__.py' \
    'mlx_guard/_binary.py' \
    'mlx_guard/_checkpoint.py' \
    'mlx_guard/_client.py' \
    'mlx_guard/py.typed' \
    "${package_name}.data/scripts/mlx-guard" \
    "${package_name}.dist-info/licenses/LICENSE" \
    "${package_name}.dist-info/licenses/THIRD_PARTY_LICENSES.md" \
    "${package_name}.dist-info/sboms/mlx-guard-cli.cyclonedx.json"; do
    if ! grep -Fqx "$required" <<<"$wheel_listing"; then
        echo "wheel is missing $required" >&2
        exit 1
    fi
done

for forbidden in 'CLAUDE.md' 'AGENTS.md' '.git/' '.codex/' 'docs/backlog/' 'superpowers/'; do
    if grep -Fq "$forbidden" <<<"$wheel_listing"; then
        echo "wheel contains forbidden workspace path $forbidden" >&2
        exit 1
    fi
done

wheel_sbom=$(unzip -p "$wheel" 'mlx_guard-*.dist-info/sboms/*.json')
if grep -Eq 'path\+file:|download_url=file:|/(Users|home|private|tmp)/' <<<"$wheel_sbom"; then
    echo "wheel SBOM contains a local filesystem reference" >&2
    exit 1
fi
if grep -aEq 'BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY|pypi-[A-Za-z0-9_-]{20}|hf_[A-Za-z0-9]{20}' < <(unzip -p "$wheel"); then
    echo "wheel contains secret-like material" >&2
    exit 1
fi

wheel_metadata=$(unzip -p "$wheel" 'mlx_guard-*.dist-info/METADATA')
if ! grep -Fqx 'Requires-Python: >=3.10, <3.15' <<<"$wheel_metadata"; then
    echo "wheel has an unexpected Python compatibility range" >&2
    exit 1
fi

python_versions=(3.10 3.11 3.12 3.13 3.14)
for python_version in "${python_versions[@]}"; do
    environment="$proof_root/python-$python_version"
    uv venv --python "$python_version" "$environment"
    uv pip install --python "$environment/bin/python" --no-deps "$wheel"
    (
        cd "$proof_root"
        "$environment/bin/python" -W error -m unittest discover \
            -s "$repo_root/python/tests" -p 'test_*.py' -v
    )
done

editable="$proof_root/editable"
if [[ -n ${MLX_GUARD_BUILD_BACKEND_PATH:-} ]]; then
    if [[ -z ${MLX_GUARD_MATURIN_BIN:-} ]]; then
        echo "MLX_GUARD_BUILD_BACKEND_PATH requires MLX_GUARD_MATURIN_BIN" >&2
        exit 1
    fi
    if [[ ! -d "$MLX_GUARD_BUILD_BACKEND_PATH/maturin" ]]; then
        echo "MLX_GUARD_BUILD_BACKEND_PATH must contain the maturin package" >&2
        exit 1
    fi
    python_312=$(uv python find 3.12)
    "$python_312" -m venv "$editable"
    PYTHONPATH="$MLX_GUARD_BUILD_BACKEND_PATH" \
        PATH="$(dirname "$MLX_GUARD_MATURIN_BIN"):$PATH" \
        "$python_312" -m pip --python "$editable" install \
        --no-deps --no-build-isolation --editable "$repo_root"
else
    UV_PROJECT_ENVIRONMENT="$editable" uv sync --locked --python 3.12
fi
(
    cd "$proof_root"
    "$editable/bin/python" -W error -m unittest discover \
        -s "$repo_root/python/tests" -p 'test_*.py' -v
)

sdists=("$sdist_dir"/*.tar.gz)
if [[ ${#sdists[@]} -ne 1 ]]; then
    echo "expected exactly one source distribution" >&2
    exit 1
fi
sdist=${sdists[0]}
sdist_listing=$(tar -tzf "$sdist")
for required in \
    "${package_name}/LICENSE" \
    "${package_name}/CHANGELOG.md" \
    "${package_name}/RELEASE_NOTES.md" \
    "${package_name}/SECURITY.md" \
    "${package_name}/THIRD_PARTY_LICENSES.md" \
    "${package_name}/crates/mlx-guard-cli/src/runtime.rs" \
    "${package_name}/crates/mlx-guard-core/src/lib.rs" \
    "${package_name}/docs/EXAMPLES.md" \
    "${package_name}/docs/RELEASE.md" \
    "${package_name}/docs/SUPPORT.md" \
    "${package_name}/docs/THREAT_MODEL.md" \
    "${package_name}/docs/integrations/PYTHON_ADAPTER.md" \
    "${package_name}/docs/integrations/WRAP_A_COMMAND.md" \
    "${package_name}/python/mlx_guard/_binary.py" \
    "${package_name}/python/mlx_guard/_checkpoint.py" \
    "${package_name}/python/mlx_guard/_client.py"; do
    if ! grep -Fqx "$required" <<<"$sdist_listing"; then
        echo "source distribution is missing $required" >&2
        exit 1
    fi
done

for forbidden in 'CLAUDE.md' 'AGENTS.md' '.git/' '.codex/' 'docs/backlog/' 'superpowers/'; do
    if grep -Fq "$forbidden" <<<"$sdist_listing"; then
        echo "source distribution contains forbidden workspace path $forbidden" >&2
        exit 1
    fi
done
if grep -aEq '/Users/|/home/runner/|BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY|pypi-[A-Za-z0-9_-]{20}|hf_[A-Za-z0-9]{20}' < <(tar -xOzf "$sdist"); then
    echo "source distribution contains a local path or secret-like material" >&2
    exit 1
fi

sdist_environment="$proof_root/sdist-python-3.12"
uv venv --python 3.12 "$sdist_environment"
sdist_source="$proof_root/sdist-source"
sdist_wheel_dir="$proof_root/sdist-wheel"
mkdir -p "$sdist_source" "$sdist_wheel_dir"
tar -xzf "$sdist" -C "$sdist_source"
(
    cd "$sdist_source/${package_name}"
    "${maturin[@]}" build --release --locked --out "$sdist_wheel_dir"
)
sdist_wheels=("$sdist_wheel_dir"/*.whl)
if [[ ${#sdist_wheels[@]} -ne 1 ]]; then
    echo "source distribution build expected exactly one wheel" >&2
    exit 1
fi
uv pip install --python "$sdist_environment/bin/python" --no-deps "${sdist_wheels[0]}"
(
    cd "$proof_root"
    "$sdist_environment/bin/python" -W error -m unittest discover \
        -s "$repo_root/python/tests" -p 'test_*.py' -v
)

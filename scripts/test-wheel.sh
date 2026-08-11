#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
proof_root=$(mktemp -d "${TMPDIR:-/tmp}/mlx-guard-wheel.XXXXXX")
trap 'rm -rf -- "$proof_root"' EXIT

if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo "wheel proof requires macOS arm64" >&2
    exit 1
fi

wheel_dir="$proof_root/wheel"
sdist_dir="$proof_root/sdist"
mkdir -p "$wheel_dir" "$sdist_dir"

cd "$repo_root"
uvx --from maturin==1.13.3 maturin build \
    --release \
    --locked \
    --out "$wheel_dir"

shopt -s nullglob
wheels=("$wheel_dir"/*.whl)
if [[ ${#wheels[@]} -ne 1 ]]; then
    echo "expected exactly one wheel" >&2
    exit 1
fi
wheel=${wheels[0]}
if [[ ${wheel##*/} != mlx_guard-0.1.0-py3-none-macosx_11_0_arm64.whl ]]; then
    echo "unexpected wheel tag: ${wheel##*/}" >&2
    exit 1
fi

wheel_listing=$(unzip -Z1 "$wheel")
for required in \
    'mlx_guard/__init__.py' \
    'mlx_guard/_binary.py' \
    'mlx_guard/_checkpoint.py' \
    'mlx_guard/_client.py' \
    'mlx_guard/py.typed' \
    'mlx_guard-0.1.0.data/scripts/mlx-guard' \
    'mlx_guard-0.1.0.dist-info/licenses/LICENSE' \
    'mlx_guard-0.1.0.dist-info/sboms/mlx-guard-cli.cyclonedx.json'; do
    if ! rg -Fxq "$required" <<<"$wheel_listing"; then
        echo "wheel is missing $required" >&2
        exit 1
    fi
done

wheel_metadata=$(unzip -p "$wheel" 'mlx_guard-*.dist-info/METADATA')
if ! rg -Fxq 'Requires-Python: >=3.10, <3.15' <<<"$wheel_metadata"; then
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
uv venv --python 3.12 "$editable"
VIRTUAL_ENV="$editable" uvx --from maturin==1.13.3 maturin develop --release --locked
(
    cd "$proof_root"
    "$editable/bin/python" -W error -m unittest discover \
        -s "$repo_root/python/tests" -p 'test_*.py' -v
)

uvx --from maturin==1.13.3 maturin sdist --out "$sdist_dir"
sdists=("$sdist_dir"/*.tar.gz)
if [[ ${#sdists[@]} -ne 1 ]]; then
    echo "expected exactly one source distribution" >&2
    exit 1
fi
sdist=${sdists[0]}
sdist_listing=$(tar -tzf "$sdist")
for required in \
    'mlx_guard-0.1.0/LICENSE' \
    'mlx_guard-0.1.0/crates/mlx-guard-cli/src/runtime.rs' \
    'mlx_guard-0.1.0/crates/mlx-guard-core/src/lib.rs' \
    'mlx_guard-0.1.0/python/mlx_guard/_binary.py' \
    'mlx_guard-0.1.0/python/mlx_guard/_checkpoint.py' \
    'mlx_guard-0.1.0/python/mlx_guard/_client.py'; do
    if ! rg -Fxq "$required" <<<"$sdist_listing"; then
        echo "source distribution is missing $required" >&2
        exit 1
    fi
done

sdist_environment="$proof_root/sdist-python-3.12"
uv venv --python 3.12 "$sdist_environment"
uv pip install --python "$sdist_environment/bin/python" --no-deps "$sdist"
(
    cd "$proof_root"
    "$sdist_environment/bin/python" -W error -m unittest discover \
        -s "$repo_root/python/tests" -p 'test_*.py' -v
)

#!/usr/bin/env bash
# Release-flow guard.
# Part A: files that must DERIVE the workspace version must not hardcode one. This catches a
#         reversion of the derivation and any mlx-guard-scoped version literal, stale or current;
#         tool pins such as `maturin 1.13.3` are ignored.
# Part B: files that legitimately carry the version must carry the CURRENT one. Between releases
#         this passes trivially; in a release PR it fails until every listed file is refreshed.
set -euo pipefail
repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"
version=$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)
if [[ -z $version ]]; then
    echo "workspace version is unavailable" >&2
    exit 1
fi
status=0

for file in scripts/test-wheel.sh scripts/build-release.sh .github/workflows/release.yml; do
    if grep -nE 'mlx[_-]guard[-/]v?[0-9]+\.[0-9]+\.[0-9]+' "$file"; then
        echo "$file hardcodes an mlx-guard version; derive it from Cargo.toml" >&2
        status=1
    fi
done
if grep -nE "^[[:space:]]*-[[:space:]]*'?v[0-9]+\.[0-9]+\.[0-9]+'?[[:space:]]*$" .github/workflows/release.yml; then
    echo ".github/workflows/release.yml pins a literal tag; use the semver glob" >&2
    status=1
fi

for file in RELEASE_NOTES.md THIRD_PARTY_LICENSES.md docs/EXAMPLES.md; do
    if ! grep -qF "$version" "$file"; then
        echo "$file does not mention the workspace version $version" >&2
        status=1
    fi
done
if ! grep -qE "^## \[$version\]" CHANGELOG.md; then
    echo "CHANGELOG.md has no [$version] section" >&2
    status=1
fi
exit "$status"

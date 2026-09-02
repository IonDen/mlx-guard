#!/usr/bin/env bash
# Classify a set of changed paths, one per line on stdin, as "docs" or "code".
#
# "docs" means every path is a documentation surface that nothing in the build or the test
# suites reads for content, so the Rust and Python jobs have nothing to prove. Anything else
# is "code". So is an empty set: an unreadable or empty diff (unknown base, force-push) must
# never be mistaken for "nothing to test", so the fail-safe direction is to run everything.
#
# Keep this list narrow and anchored. A README under crates/ is not on it on purpose, and
# neither is anything under .github/, scripts/, or the packaging files: those change what CI
# does or what ships, and must run the full pipeline.
set -euo pipefail

seen=0
while IFS= read -r path || [[ -n $path ]]; do
    [[ -z $path ]] && continue
    seen=1
    case "$path" in
        README.md | CHANGELOG.md | RELEASE_NOTES.md | SECURITY.md | LICENSE | THIRD_PARTY_LICENSES.md) ;;
        docs/* | evidence/*) ;;
        *)
            echo code
            exit 0
            ;;
    esac
done

if [[ $seen -eq 0 ]]; then
    echo code
else
    echo docs
fi

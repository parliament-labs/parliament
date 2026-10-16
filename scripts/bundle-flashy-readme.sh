#!/usr/bin/env bash
#
# Bundle the Flashy-README into a zippable artifact suitable for
# emailing or dropping into a chat:
#
#   artifacts/parliament-flashy-readme-<sha>.zip
#     parliament-flashy-readme/
#       README.md          (source Flashy-README.md with image paths
#                           rewritten from docs/screenshots/ → images/)
#       images/
#         <slug>.png       (only PNGs actually referenced in README.md)
#         provenance.json  (if present)
#
# Prerequisites:
#   1. Screenshot specs have been run (PNGs live in docs/screenshots/).
#   2. Flashy-README.md references images via markdown syntax
#      `![alt](docs/screenshots/<slug>.png)`, not HTML comments.
#      (During drafting, image refs are HTML comments so GitHub
#      renders cleanly. Swap to markdown syntax before bundling.)
#
# Output file is named with the current git short-SHA so recipients
# can tell which version they received.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
SRC="${REPO_ROOT}/Flashy-README.md"
SHA="$(git -C "${REPO_ROOT}" rev-parse --short HEAD)"
STAGE="${REPO_ROOT}/artifacts/parliament-flashy-readme"
OUT="${REPO_ROOT}/artifacts/parliament-flashy-readme-${SHA}.zip"

if [[ ! -f "${SRC}" ]]; then
  echo "error: ${SRC} not found" >&2
  exit 1
fi

rm -rf "${STAGE}" "${OUT}"
mkdir -p "${STAGE}/images"

# Rewrite image paths in the markdown: docs/screenshots/X.png → images/X.png
sed 's|docs/screenshots/|images/|g' "${SRC}" > "${STAGE}/README.md"

# Copy only PNGs actually referenced in the output README. Warn on any
# reference that points at a file that doesn't exist yet.
missing=0
while read -r rel; do
  src="${REPO_ROOT}/docs/screenshots/${rel#images/}"
  if [[ -f "${src}" ]]; then
    cp "${src}" "${STAGE}/${rel}"
  else
    echo "warn: referenced PNG missing: ${src}" >&2
    missing=$((missing + 1))
  fi
done < <(grep -oE 'images/[a-zA-Z0-9_-]+\.png' "${STAGE}/README.md" | sort -u)

# Include provenance metadata if the screenshot specs produced any.
if [[ -f "${REPO_ROOT}/docs/screenshots/provenance.json" ]]; then
  cp "${REPO_ROOT}/docs/screenshots/provenance.json" "${STAGE}/images/provenance.json"
fi

# Zip. Run from artifacts/ so paths inside the zip stay relative.
(cd "${REPO_ROOT}/artifacts" && zip -r "parliament-flashy-readme-${SHA}.zip" "parliament-flashy-readme") >/dev/null

echo "bundled: ${OUT}"
ls -lh "${OUT}"

if (( missing > 0 )); then
  echo "warn: ${missing} image reference(s) missing from docs/screenshots/" >&2
  echo "warn: capture screenshots and re-run this script before sharing" >&2
  exit 2
fi

#!/usr/bin/env bash
# Prints the release notes for a tag: its section of CHANGELOG.md, then how to install.
#
#   scripts/release-notes.sh v0.1.0 > dist/notes.md
#
# Fails when CHANGELOG.md has no "## <version> – <date>" section for the tag, so a release
# never goes out without notes. The release workflow runs this.
set -euo pipefail
cd "$(dirname "$0")/.."

tag="${1:?which tag? e.g. scripts/release-notes.sh v0.1.0}"
version="${tag#v}"
section="$(awk -v v="$version" '
  /^## / { if (found) exit; found = ($2 == v) ; next }
  found
' CHANGELOG.md)"
# Drop the blank lines around the section.
section="$(printf '%s\n' "$section" | sed -e '/./,$!d' | tac | sed -e '/./,$!d' | tac)"
if [[ -z $section ]]; then
  echo "CHANGELOG.md has no section for $version" >&2
  exit 1
fi

cat <<NOTES
$section

## Installing

Unpack the archive, then run \`./install.sh\` to install Rhein for yourself (into \`~/.local\`), or \`./install.sh --system\` for everyone (into \`/usr\`).

Rhein needs ALSA, PipeWire, a Vulkan driver, xkbcommon, fontconfig and glibc 2.35 or newer, plus xdg-desktop-portal with your desktop's backend for screen sharing. On other systems, build it from source with \`./install\`.

Rhein is beta software, and its cryptography has not been audited. Please report security problems as described in [SECURITY.md](https://github.com/alpwrk/rhein/blob/main/SECURITY.md).
NOTES

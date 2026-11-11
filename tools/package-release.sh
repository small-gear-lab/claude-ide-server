#!/usr/bin/env bash
# Pack what a release ships. Run by the release workflow, and by hand to see what a release holds.
#
#   tools/package-release.sh server <version> <target> <executable> <outdir>
#       claude-ide-server-<version>-<target>.tar.gz (.zip for a Windows target): the executable, the licenses, the README
#       and contrib/ (the files for systemd, launchd and Task Scheduler), under one directory of the same name.
#   tools/package-release.sh nvim <version> <outdir>
#       claude-ide-nvim-<version>.zip: the Neovim adapter (editors/nvim without its tests) and the licenses, under one
#       directory of the same name. It is Lua only, so there is one, for every system.
#
# The files are taken from the working tree; the order and the times inside an archive do not depend on when it is made, so
# that two runs on one commit give the same archive.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
kind=${1:?kind: server or nvim}

# One time for everything in an archive: the commit that is packed (or now, outside a repository).
stamp=$(git -C "$root" log -1 --format=%ct 2>/dev/null || date +%s)

pack() { # pack <directory name> <format> <outdir>: archives ./<directory name> of the current directory
  local dir=$1 format=$2 out=$3
  # What does not depend on the machine that packs: the modes (the executable is the one file that runs), the times, the order.
  find "$dir" -type d -exec chmod 755 {} +
  find "$dir" -type f -exec chmod 644 {} +
  find "$dir" -type f \( -name claude-ide-server -o -name claude-ide-server.exe \) -exec chmod 755 {} +
  find "$dir" -exec touch -d "@$stamp" {} +
  case "$format" in
    zip) find "$dir" | LC_ALL=C sort | zip -X -q "$out/$dir.zip" -@ ;;
    tar.gz) find "$dir" -print0 | LC_ALL=C sort -z \
        | tar --null --no-recursion --owner=0 --group=0 --numeric-owner -T - -cf - | gzip -n > "$out/$dir.tar.gz" ;;
  esac
}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

case "$kind" in
  server)
    version=${2:?version} target=${3:?target} executable=${4:?executable} out=${5:?outdir}
    name="claude-ide-server-$version-$target"
    mkdir -p "$stage/$name" "$out"
    install -m 755 "$executable" "$stage/$name/"
    cp "$root/LICENSE-APACHE" "$root/LICENSE-MIT" "$root/README.md" "$stage/$name/"
    cp -R "$root/contrib" "$stage/$name/contrib"
    case "$target" in *windows*) format=zip ;; *) format=tar.gz ;; esac
    ;;
  nvim)
    version=${2:?version} out=${3:?outdir}
    name="claude-ide-nvim-$version"
    mkdir -p "$stage/$name" "$out"
    cp -R "$root/editors/nvim/lua" "$root/editors/nvim/plugin" "$stage/$name/"
    cp "$root/editors/nvim/README.md" "$root/LICENSE-APACHE" "$root/LICENSE-MIT" "$stage/$name/"
    format=zip
    ;;
  *) echo "unknown kind: $kind" >&2; exit 2 ;;
esac

out=$(cd "$out" && pwd)
(cd "$stage" && pack "$name" "$format" "$out")
echo "$out/$name.$format"

#!/usr/bin/env bash
# Build the documentation.
#
#   tools/build-docs.sh          build HTML (docs/_build/html/{en,ja}) and
#                                regenerate the Japanese Markdown (docs/ja and README.ja.md, checked in)
#   tools/build-docs.sh update   refresh locale/ja/*.po from the English source first
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
docs="$root/docs"
out="$docs/_build"

run() { uv run --no-project --quiet --with-requirements "$docs/requirements.txt" "$@"; }

if [ "${1:-}" = "update" ]; then
  run sphinx-build -b gettext "$docs" "$out/gettext"
  run sphinx-intl update -p "$out/gettext" -l ja -d "$docs/locale"
fi

run sphinx-build -b html -d "$out/doctrees/en" "$docs" "$out/html/en"
run sphinx-build -b html -d "$out/doctrees/ja" -D language=ja "$docs" "$out/html/ja"
printf '<meta http-equiv="refresh" content="0; url=en/">\n' > "$out/html/index.html"

# Japanese Markdown, readable directly on GitHub.
rm -rf "$out/md"
# (The links stay as they are written, which is what the README needs.)
run sphinx-build -b markdown -d "$out/doctrees/md" -D language=ja \
  -D extensions=myst_parser,sphinx_markdown_builder -D myst_all_links_external=1 "$docs" "$out/md"
mkdir -p "$docs/ja"
for f in "$out"/md/*.md; do
  [ "$(basename "$f")" = readme.md ] || cp "$f" "$docs/ja/"
done
# The README in Japanese lives at the top of the repository: its links to the documents point
# to the Japanese ones, and the link to the other language points back to the English README.
sed -e 's#](docs/\([A-Z]\)#](docs/ja/\1#g' -e 's#](README.ja.md)#](README.md)#' "$out/md/readme.md" > "$root/README.ja.md"
# The Markdown builder writes `0.130` as `0.13` in a table: put the numbers back.
for f in "$docs"/*.md; do
  [ -f "$docs/ja/$(basename "$f")" ] && python3 "$root/tools/restore-numbers.py" "$f" "$docs/ja/$(basename "$f")"
done
python3 "$root/tools/restore-numbers.py" "$root/README.md" "$root/README.ja.md"
# The code blocks of the README are not translated by gettext: see docs/locale/ja/readme-code.json.
python3 "$root/tools/translate-code.py" "$root/README.md" "$root/README.ja.md"

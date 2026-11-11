"""Sphinx configuration.

English is the source language. Japanese is built from gettext catalogs in
locale/ja/LC_MESSAGES/*.po; see tools/build-docs.sh.
"""

project = "claude-ide-server"
author = "Small Gear Lab"
copyright = "2026, Small Gear Lab"

extensions = ["myst_parser"]
source_suffix = {".md": "markdown"}
myst_heading_anchors = 3

exclude_patterns = ["_build", "ja", "locale", "requirements.txt"]

# readme.md includes the README of the repository, whose links are relative to the repository.
suppress_warnings = ["myst.xref_missing"]

language = "en"
locale_dirs = ["locale/"]
gettext_compact = False

html_theme = "alabaster"

#!/usr/bin/env python3
"""Replace the code blocks of README.ja.md by their Japanese versions.

The text of a block of README.md is looked up in docs/locale/ja/readme-code.json (English text
to Japanese text). A block that is not there stays in English, and is named on standard error.
usage: translate-code.py README.md README.ja.md   (README.ja.md is rewritten)
"""
import json
import os
import re
import sys

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
table = json.load(open(os.path.join(root, "docs/locale/ja/readme-code.json")))
english = open(sys.argv[1]).read()
path = sys.argv[2]
japanese = open(path).read()
fence = re.compile(r"(```[a-z]*\n)(.*?)(```)", re.S)
before = [m.group(2) for m in fence.finditer(english)]
index = 0


def put(match):
    global index
    original = before[index]
    index += 1
    if original in table:
        return match.group(1) + table[original] + match.group(3)
    print("not translated: " + original.split("\n")[0], file=sys.stderr)
    return match.group(0)


if len(before) == len(fence.findall(japanese)):
    open(path, "w").write(fence.sub(put, japanese))

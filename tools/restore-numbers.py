#!/usr/bin/env python3
"""Put back the numbers of the tables that the Markdown builder rewrote.

The builder turns a cell such as `0.130` into `0.13`. For every table cell of the Japanese file
whose text is the same number as the cell at the same place of the English file, the English
text is written back. usage: restore-numbers.py ENGLISH.md JAPANESE.md (JAPANESE.md is rewritten)
"""
import re
import sys

NUMBER = re.compile(r"^[0-9]+(\.[0-9]+)?$")


def cells(lines):
    return [c.strip() for line in lines if line.startswith("|") for c in line.strip().strip("|").split("|")]


def main():
    english = open(sys.argv[1]).read().split("\n")
    path = sys.argv[2]
    japanese = open(path).read().split("\n")
    want = cells(english)
    if len(want) != len(cells(japanese)):
        return  # not the same tables: leave the file alone
    i = 0
    out = []
    for line in japanese:
        if not line.startswith("|"):
            out.append(line)
            continue
        parts = line.rstrip().strip("|").split("|")
        fixed = []
        for part in parts:
            text = part.strip()
            target = want[i]
            i += 1
            if text != target and NUMBER.match(text) and NUMBER.match(target) and float(text) == float(target):
                lead = len(part) - len(part.lstrip())
                width = len(part)
                new = " " * lead + target
                part = new.ljust(width) if len(new) <= width else new + " "
            fixed.append(part)
        out.append("|" + "|".join(fixed) + "|")
    open(path, "w").write("\n".join(out))


main()

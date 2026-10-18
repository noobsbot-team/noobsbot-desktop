"""Writes data/emoji.txt (the emoji picker's table, E7.2) from Unicode's emoji-test.txt.

Usage: python3 -I scripts/emoji-table.py <emoji-test.txt> > data/emoji.txt
Keeps fully-qualified emoji without skin-tone variants and drops the Component group; `= Group` lines start each
category, then one `<emoji> <name>` per line in Unicode's order.
"""

import sys

group = None
out = []
for line in open(sys.argv[1], encoding="utf-8"):
    if line.startswith("# group: "):
        group = line[len("# group: "):].strip()
        if group != "Component":
            out.append(f"= {group}")
    elif "; fully-qualified" in line and group != "Component":
        emoji, name = line.split("# ", 1)[1].split(" ", 1)
        name = name.split(" ", 1)[1].strip()  # drop the "E1.0" version
        if "skin tone" not in name:
            out.append(f"{emoji} {name}")
print("\n".join(out))

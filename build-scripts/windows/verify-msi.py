#!/usr/bin/env python3
"""Check that MSI packaging preserved each executable, including its signature."""
import hashlib
from pathlib import Path
import sys

source, extracted = map(Path, sys.argv[1:])
for built, installed in [
    ("playit-cli.exe", "playit.exe"),
    ("playitd.exe", "playitd.exe"),
    ("playitd-tray.exe", "playitd-tray.exe"),
]:
    matches = list(extracted.rglob(installed))
    if len(matches) != 1:
        raise SystemExit(f"Expected one {installed} in the MSI; found {len(matches)}")
    expected = hashlib.sha256((source / built).read_bytes()).digest()
    actual = hashlib.sha256(matches[0].read_bytes()).digest()
    if actual != expected:
        raise SystemExit(f"MSI changed {installed}; its signature would be invalid")
print("MSI contents match all three staged executables")

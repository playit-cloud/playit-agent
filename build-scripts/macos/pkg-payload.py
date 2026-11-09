#!/usr/bin/env python3
"""Unpack and repack the payload of an expanded macOS component package on Linux.

`xar -xf` expands a flat installer package into a directory per component package.
Each component holds a gzip-compressed cpio (odc) Payload, a Bom listing every
payload entry with its size and checksum, and PackageInfo with payload totals.

  extract COMPONENT ROOT   Unpack COMPONENT/Payload into ROOT. Ownership, modes, and
                           modification times go to COMPONENT/payload-metadata.json,
                           since an unprivileged extract cannot keep them.
  pack COMPONENT ROOT      Write ROOT back to COMPONENT/Payload and COMPONENT/Bom,
                           and update the totals in COMPONENT/PackageInfo.

Entries kept from the original payload keep their recorded metadata. Entries added
after extraction, such as _CodeSignature/CodeResources from signing, take their owner
from the closest recorded parent directory and their mode from the file system.
pack runs `mkbom` from bomutils to write the Bom.
"""

import gzip
import json
import os
import re
import stat
import subprocess
import sys
import tempfile

METADATA = "payload-metadata.json"
TRAILER = "TRAILER!!!"


def cksum(data):
    """POSIX cksum CRC, which a Bom stores for each file and symlink."""
    crc = 0
    length = len(data)
    for byte in data:
        crc = _CKSUM_TABLE[((crc >> 24) ^ byte) & 0xFF] ^ ((crc << 8) & 0xFFFFFFFF)
    while length:
        crc = _CKSUM_TABLE[((crc >> 24) ^ length) & 0xFF] ^ ((crc << 8) & 0xFFFFFFFF)
        length >>= 8
    return ~crc & 0xFFFFFFFF


def _cksum_table():
    table = []
    for i in range(256):
        crc = i << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7) if crc & 0x80000000 else (crc << 1)
        table.append(crc & 0xFFFFFFFF)
    return table


_CKSUM_TABLE = _cksum_table()


def read_cpio(data):
    """Yield (name, header fields, contents) from an odc cpio archive."""
    offset = 0
    while True:
        header = data[offset:offset + 76]
        if len(header) != 76 or header[:6] != b"070707":
            raise SystemExit("Payload is not an odc cpio archive")
        fields = {
            "ino": int(header[12:18], 8),
            "mode": int(header[18:24], 8),
            "uid": int(header[24:30], 8),
            "gid": int(header[30:36], 8),
            "nlink": int(header[36:42], 8),
            "mtime": int(header[48:59], 8),
        }
        name_size = int(header[59:65], 8)
        file_size = int(header[65:76], 8)
        offset += 76
        name = data[offset:offset + name_size - 1].decode()
        offset += name_size
        contents = data[offset:offset + file_size]
        offset += file_size
        if name == TRAILER:
            return
        yield name, fields, contents


def cpio_header(name, mode, uid, gid, nlink, mtime, ino, size):
    encoded = name.encode() + b"\0"
    return (b"070707" + b"%06o" % 0 + b"%06o" % ino + b"%06o" % mode + b"%06o" % uid
            + b"%06o" % gid + b"%06o" % nlink + b"%06o" % 0 + b"%011o" % mtime
            + b"%06o" % len(encoded) + b"%011o" % size + encoded)


def load_payload(component):
    with open(os.path.join(component, "Payload"), "rb") as f:
        data = f.read()
    if data[:4] == b"pbzx":
        raise SystemExit("pbzx payloads are not supported; build the package with pkgbuild's default compression")
    if data[:2] != b"\x1f\x8b":
        raise SystemExit("Payload is not gzip-compressed")
    return gzip.decompress(data)


def local_path(root, name):
    relative = os.path.normpath(name)
    if relative == ".":
        return root
    if relative.startswith("..") or os.path.isabs(relative):
        raise SystemExit(f"Refusing payload path outside the root: {name}")
    return os.path.join(root, relative)


def extract(component, root):
    if os.path.exists(root) and os.listdir(root):
        raise SystemExit(f"{root} must be empty")
    os.makedirs(root, exist_ok=True)
    metadata = {}
    seen_inodes = set()
    for name, fields, contents in read_cpio(load_payload(component)):
        path = local_path(root, name)
        kind = stat.S_IFMT(fields["mode"])
        if kind == stat.S_IFDIR:
            os.makedirs(path, exist_ok=True)
            os.chmod(path, 0o755)
        elif kind == stat.S_IFREG:
            if fields["nlink"] > 1:
                if fields["ino"] in seen_inodes:
                    raise SystemExit(f"Hard links are not supported: {name}")
                seen_inodes.add(fields["ino"])
            with open(path, "wb") as f:
                f.write(contents)
            # Keep files writable for signing; the archived mode is restored on pack.
            os.chmod(path, (stat.S_IMODE(fields["mode"]) | 0o600) & 0o755)
        elif kind == stat.S_IFLNK:
            os.symlink(contents.decode(), path)
        else:
            raise SystemExit(f"Unsupported payload entry type: {name}")
        metadata[os.path.normpath(name)] = {k: fields[k] for k in ("mode", "uid", "gid", "nlink", "mtime")}
    with open(os.path.join(component, METADATA), "w") as f:
        json.dump(metadata, f, indent=1, sort_keys=True)


def walk(root):
    """Yield payload-relative paths, each directory before its contents."""
    yield "."
    for directory, dirs, files in os.walk(root):
        dirs.sort()
        for entry in sorted(dirs + files):
            yield os.path.relpath(os.path.join(directory, entry), root)


def pack(component, root):
    with open(os.path.join(component, METADATA)) as f:
        metadata = json.load(f)
    archive = bytearray()
    bom_lines = []
    files = 0
    install_kbytes = 0
    for ino, relative in enumerate(walk(root), start=1):
        path = root if relative == "." else os.path.join(root, relative)
        info = os.lstat(path)
        kind = stat.S_IFMT(info.st_mode)
        recorded = metadata.get(relative)
        if recorded is None:
            parent = os.path.dirname(relative) or "."
            while parent not in metadata:
                parent = os.path.dirname(parent) or "."
            owner = metadata[parent]
            mode = kind | (stat.S_IMODE(info.st_mode) & 0o755)
            recorded = {"mode": mode, "uid": owner["uid"], "gid": owner["gid"],
                        "nlink": 2 if kind == stat.S_IFDIR else 1, "mtime": int(info.st_mtime)}
        elif stat.S_IFMT(recorded["mode"]) != kind:
            raise SystemExit(f"{relative} changed type after extraction")
        name = "." if relative == "." else "./" + relative
        if kind == stat.S_IFDIR:
            contents = b""
        elif kind == stat.S_IFREG:
            with open(path, "rb") as f:
                contents = f.read()
        elif kind == stat.S_IFLNK:
            contents = os.readlink(path).encode()
        else:
            raise SystemExit(f"Unsupported file type: {path}")
        archive += cpio_header(name, recorded["mode"], recorded["uid"], recorded["gid"],
                               recorded["nlink"], recorded["mtime"], ino, len(contents))
        archive += contents
        line = f"{name}\t{recorded['mode']:o}\t{recorded['uid']}/{recorded['gid']}\t{recorded['mtime']}"
        if kind != stat.S_IFDIR:
            line += f"\t{len(contents)}\t{cksum(contents)}"
        if kind == stat.S_IFLNK:
            line += "\t" + contents.decode()
        bom_lines.append(line)
        files += 1
        install_kbytes += (len(contents) + 1023) // 1024
    archive += cpio_header(TRAILER, 0, 0, 0, 1, 0, 0, 0)

    with open(os.path.join(component, "Payload"), "wb") as f:
        f.write(gzip.compress(bytes(archive), compresslevel=9, mtime=0))
    with tempfile.NamedTemporaryFile("w", suffix=".bomlist") as listing:
        listing.write("\n".join(bom_lines) + "\n")
        listing.flush()
        subprocess.run(["mkbom", "-i", listing.name, os.path.join(component, "Bom")], check=True,
                       stdout=subprocess.DEVNULL)

    package_info = os.path.join(component, "PackageInfo")
    with open(package_info) as f:
        text = f.read()
    text, count = re.subn(r'(<payload\b[^>]*\bnumberOfFiles=")\d+(")', rf"\g<1>{files}\2", text)
    text, count2 = re.subn(r'(<payload\b[^>]*\binstallKBytes=")\d+(")', rf"\g<1>{install_kbytes}\2", text)
    if count != 1 or count2 != 1:
        raise SystemExit("PackageInfo has no <payload numberOfFiles installKBytes> element")
    with open(package_info, "w") as f:
        f.write(text)
    os.remove(os.path.join(component, METADATA))
    print(install_kbytes)


def main():
    if len(sys.argv) != 4 or sys.argv[1] not in ("extract", "pack"):
        print(__doc__.strip(), file=sys.stderr)
        sys.exit(2)
    command, component, root = sys.argv[1:]
    (extract if command == "extract" else pack)(component, root)


if __name__ == "__main__":
    main()

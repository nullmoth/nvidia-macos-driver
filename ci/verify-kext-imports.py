#!/usr/bin/env python3
"""Reject new unresolved imports relative to the matching stock kexts."""
import json
from pathlib import Path
import struct


def undefined_symbols(path):
    data = Path(path).read_bytes()
    magic, cpu, _, kind, count, _, _, _ = struct.unpack_from("<8I", data)
    if (magic, cpu, kind) != (0xfeedfacf, 0x1000007, 11):
        raise ValueError(f"Expected a thin x86_64 kext: {path}")
    position = 32
    for _ in range(count):
        command, size = struct.unpack_from("<II", data, position)
        if size < 8 or position + size > len(data):
            raise ValueError("Invalid Mach-O load command")
        if command == 2:  # LC_SYMTAB
            offset, number, strings, string_size = struct.unpack_from("<4I", data, position + 8)
            if offset + 16 * number > len(data) or strings + string_size > len(data):
                raise ValueError("Invalid Mach-O symbol table")
            result = set()
            for index in range(number):
                name, kind, _, _, _ = struct.unpack_from("<IBBHQ", data, offset + 16 * index)
                if name and not kind & 0xe0 and (kind & 0xe) == 0 and kind & 1:
                    end = data.index(0, strings + name, strings + string_size)
                    result.add(data[strings + name:end].decode("utf-8"))
            return result
        position += size
    raise ValueError("Mach-O symbol table missing")


def verify_imports(compiled, stock, report):
    results = {}
    unexpected = []
    for name in ("NVRM", "NVRMFB"):
        suffix = Path(name + ".kext") / "Contents/MacOS" / name
        original = undefined_symbols(stock / suffix)
        candidate = undefined_symbols(compiled / suffix)
        extra = sorted(candidate - original)
        results[name] = {"stock_imports": len(original), "candidate_imports": len(candidate),
                         "new_unresolved_imports": extra}
        unexpected.extend(extra)
    report.write_text(json.dumps(results, indent=2) + "\n")
    if unexpected:
        raise RuntimeError("New unresolved imports require investigation: " + ", ".join(unexpected))
    print("Both kexts use only imports already present in the matching stock binaries.")

#!/usr/bin/env python3
"""Fan speeds from the SMC: FNum (fan count) and F<n>Ac (actual RPM), read through AppleSMC's user client.

On a real Mac these come from Apple's SMC; on a PC they come from VirtualSMC, which SMCSuperIO feeds from the board's
Super I/O chip (the EFI 1401 builds carries NullMoth's source build of both). The benchmark thermal guard used to read a
private probe kext for one chip (ITE IT8696E); this reads the standard keys any of them publish. Read only: it never
writes a key.

usage: fan_read_smc.py [--json]   -> one line per fan, or {"fans": [rpm, ...], "source": "..."}; exit 1 when the SMC or
       its fan keys cannot be read (the reason is printed, never an empty "0 fans" that looks like a reading)."""
import ctypes
import ctypes.util
import json
import struct
import sys

KERNEL_INDEX_SMC = 2
SMC_CMD_READ_KEYINFO = 9
SMC_CMD_READ_BYTES = 5


class _Vers(ctypes.Structure):
    _fields_ = [("major", ctypes.c_uint8), ("minor", ctypes.c_uint8), ("build", ctypes.c_uint8),
                ("reserved", ctypes.c_uint8), ("release", ctypes.c_uint16)]


class _PLimit(ctypes.Structure):
    _fields_ = [("version", ctypes.c_uint16), ("length", ctypes.c_uint16), ("cpuPLimit", ctypes.c_uint32),
                ("gpuPLimit", ctypes.c_uint32), ("memPLimit", ctypes.c_uint32)]


class _KeyInfo(ctypes.Structure):
    _fields_ = [("dataSize", ctypes.c_uint32), ("dataType", ctypes.c_uint32), ("dataAttributes", ctypes.c_uint8)]


class _Param(ctypes.Structure):
    """SMCParamStruct, the 80-byte record AppleSMC's selector 2 takes and returns."""
    _fields_ = [("key", ctypes.c_uint32), ("vers", _Vers), ("pLimitData", _PLimit), ("keyInfo", _KeyInfo),
                ("result", ctypes.c_uint8), ("status", ctypes.c_uint8), ("data8", ctypes.c_uint8),
                ("data32", ctypes.c_uint32), ("bytes", ctypes.c_uint8 * 32)]


def _fourcc(s):
    return struct.unpack(">I", s.encode("ascii"))[0]


class SMC:
    def __init__(self):
        iokit = ctypes.cdll.LoadLibrary(ctypes.util.find_library("IOKit"))
        self.io = iokit
        iokit.IOServiceMatching.restype = ctypes.c_void_p
        iokit.IOServiceGetMatchingService.restype = ctypes.c_uint32
        iokit.IOServiceGetMatchingService.argtypes = [ctypes.c_uint32, ctypes.c_void_p]
        svc = iokit.IOServiceGetMatchingService(0, iokit.IOServiceMatching(b"AppleSMC"))
        if not svc:
            raise OSError("no AppleSMC service")
        task = ctypes.c_uint32.in_dll(ctypes.CDLL(None), "mach_task_self_")
        conn = ctypes.c_uint32()
        kr = iokit.IOServiceOpen(svc, task, 0, ctypes.byref(conn))
        iokit.IOObjectRelease(svc)
        if kr:
            raise OSError(f"IOServiceOpen(AppleSMC) failed: 0x{kr & 0xffffffff:x}")
        self.conn = conn
        iokit.IOConnectCallStructMethod.argtypes = [ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_size_t,
                                                    ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t)]

    def _call(self, inp):
        out = _Param()
        size = ctypes.c_size_t(ctypes.sizeof(out))
        kr = self.io.IOConnectCallStructMethod(self.conn, KERNEL_INDEX_SMC, ctypes.byref(inp), ctypes.sizeof(inp),
                                               ctypes.byref(out), ctypes.byref(size))
        if kr or out.result:
            raise KeyError(f"SMC call failed (kr 0x{kr & 0xffffffff:x}, result {out.result})")
        return out

    def read(self, key):
        """-> (type fourcc, bytes) for key."""
        p = _Param(); p.key = _fourcc(key); p.data8 = SMC_CMD_READ_KEYINFO
        info = self._call(p).keyInfo
        p = _Param(); p.key = _fourcc(key); p.keyInfo.dataSize = info.dataSize; p.data8 = SMC_CMD_READ_BYTES
        out = self._call(p)
        return struct.pack(">I", info.dataType).decode("ascii", "replace"), bytes(out.bytes[:info.dataSize])

    def close(self):
        self.io.IOServiceClose(self.conn)


def decode(kind, raw):
    """SMC numeric types the fan keys use: flt (little-endian float on Apple silicon and VirtualSMC), fpe2 (unsigned
    14.2 fixed point, big-endian, older Intel Macs), ui8/ui16. Anything else raises."""
    if kind == "flt " and len(raw) == 4:
        return struct.unpack("<f", raw)[0]
    if kind == "fpe2" and len(raw) == 2:
        return struct.unpack(">H", raw)[0] / 4.0
    if kind in ("ui8 ", "ui16") and raw:
        return int.from_bytes(raw, "big")
    raise ValueError(f"unknown SMC type {kind!r} ({len(raw)} bytes)")


def fans(smc=None):
    own = smc is None
    smc = smc or SMC()
    try:
        n = int(decode(*smc.read("FNum")))
        return [round(decode(*smc.read(f"F{i}Ac")), 1) for i in range(n)]
    finally:
        if own:
            smc.close()


def main(argv):
    try:
        rpm = fans()
    except (OSError, KeyError, ValueError) as e:
        print(f"fan_read_smc: cannot read fans: {e}", file=sys.stderr)
        return 1
    if "--json" in argv:
        print(json.dumps({"fans": rpm, "source": "SMC FNum/F<n>Ac"}))
    else:
        for i, r in enumerate(rpm):
            print(f"fan {i}: {r:.0f} rpm")
        if not rpm:
            print("the SMC reports no fans")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

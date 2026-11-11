# The (Apple class, offset) pairs the accelerator kext reads or writes by fixed offset.
PAIRS = [("IOAccelMemoryMap", 0x80), ("IOAccelDisplayMachine", 0x108), ("IOAccelDisplayMachine", 0x10c), ("IOAccelLegacyDisplayMachine", 0x108),
         ("IOGraphicsAccelerator2", 0xc78), ("IOGraphicsAccelerator2", 0xcb0), ("IOAccelDisplayPipe", 0x98), ("IOAccelLegacyDisplayPipe", 0x98),
         ("IOAccelVidMemory", 0xc8), ("IOAccelDisplayMachine", 0x88), ("IOAccelLegacyDisplayMachine", 0x88), ("IOAccelSysMemory", 0x40), ("IOAccelMemory", 0x40), ("IOAccelVidMemory", 0x40),
         ("IOAccelResource2", 0x88), ("IOAccelResource2", 0xb0), ("IOAccelResource2", 0xb2), ("IOAccelResource2", 0xb8), ("IOAccelResource2", 0xc0),
         ("IOAccelResource2", 0xc), ("IOAccelResource2", 0x30), ("IOAccelResource2", 0xe0), ("IOAccelResource2", 0x14)]

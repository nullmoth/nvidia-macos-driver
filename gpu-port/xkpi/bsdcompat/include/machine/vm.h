/* FreeBSD <machine/vm.h> (x86): memory attributes. They select the cache type a mapping gets (PAT). */
#pragma once
#define VM_MEMATTR_UNCACHEABLE    0x00
#define VM_MEMATTR_WRITE_COMBINING 0x01
#define VM_MEMATTR_WRITE_THROUGH  0x04
#define VM_MEMATTR_WRITE_PROTECTED 0x05
#define VM_MEMATTR_WRITE_BACK     0x06
#define VM_MEMATTR_WEAK_UNCACHEABLE 0x07
#define VM_MEMATTR_DEFAULT        VM_MEMATTR_WRITE_BACK
#define VM_MEMATTR_DEVICE         VM_MEMATTR_UNCACHEABLE

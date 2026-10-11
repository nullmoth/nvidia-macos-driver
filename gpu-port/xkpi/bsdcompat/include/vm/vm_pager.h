/* FreeBSD <vm/vm_pager.h>: pager return codes LinuxKPI's fault path uses. */
#pragma once
#define VM_PAGER_OK 0
#define VM_PAGER_BAD 1
#define VM_PAGER_FAIL 2
#define VM_PAGER_PEND 3
#define VM_PAGER_ERROR 4
#define VM_PAGER_AGAIN 5

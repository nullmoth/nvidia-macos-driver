/* FreeBSD NUMA domain sets: macOS does not expose NUMA to kexts; everything is domain 0. */
#pragma once
struct domainset;
#define DOMAINSET_RR() ((struct domainset *)0)
#define DOMAINSET_PREF(d) ((struct domainset *)0)
#define vm_ndomains 1

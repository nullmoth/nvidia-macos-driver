/* FreeBSD <sys/module.h> on xnu: the kext is the module; DECLARE_MODULE hands its event handler to the SYSINIT set. */
#pragma once
#include <sys/kernel.h>
typedef struct module *module_t;
typedef enum modeventtype { MOD_LOAD, MOD_UNLOAD, MOD_SHUTDOWN, MOD_QUIESCE } modeventtype_t;
typedef int (*modeventhand_t)(module_t, int, void *);
typedef struct moduledata { const char *name; modeventhand_t evhand; void *priv; } moduledata_t;
#define MODULE_VERSION(name, version)
#define MODULE_DEPEND(mod, dep, min, pref, max)
#define MODULE_PNP_INFO(d, b, unique, t, n)
void nmx_module_event(void *arg);
#define DECLARE_MODULE(name, data, sub, order) \
    SYSINIT(name##module, sub, order, nmx_module_event, &(data))
int module_getid(module_t);

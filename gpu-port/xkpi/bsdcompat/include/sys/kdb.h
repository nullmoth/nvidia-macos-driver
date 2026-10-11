/* FreeBSD kdb(9): the in-kernel debugger is xnu's; LinuxKPI only asks whether it is active. */
#pragma once
#define kdb_active 0
#define KDB_WHY_UNSET NULL
#define kdb_backtrace() do { } while (0)
#define kdb_enter(why, msg) panic("%s", (msg))

/* FreeBSD jails do not exist on macOS: every thread is in the host. */
#pragma once
#define jailed(cred) 0

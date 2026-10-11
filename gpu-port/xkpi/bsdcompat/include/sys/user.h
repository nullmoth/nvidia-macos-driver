/* FreeBSD <sys/user.h>: process/file info exported to tools (kinfo_file); only the type is named here */
#pragma once
struct kinfo_file { int kf_type; int kf_flags; };
#define KF_TYPE_DEV 7

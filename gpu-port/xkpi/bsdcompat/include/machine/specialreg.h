/* FreeBSD <machine/specialreg.h> (x86): the CPUID feature bits LinuxKPI tests (Intel SDM vol. 2A, CPUID). */
#pragma once
/* CPUID leaf 1 EAX signature fields; family and model with their extended parts (FreeBSD's amd64 forms) */
#define CPUID_STEPPING 0x0000000f
#define CPUID_MODEL 0x000000f0
#define CPUID_FAMILY 0x00000f00
#define CPUID_EXT_MODEL 0x000f0000
#define CPUID_EXT_FAMILY 0x0ff00000
#define CPUID_TO_MODEL(id) ((((id) & CPUID_MODEL) >> 4) | (((id) & CPUID_EXT_MODEL) >> 12))
#define CPUID_TO_FAMILY(id) ((((id) & CPUID_FAMILY) >> 8) + (((id) & CPUID_EXT_FAMILY) >> 20))
#define CPUID_TO_STEPPING(id) ((id) & CPUID_STEPPING)
#define CPUID_FPU 0x00000001
#define CPUID_TSC 0x00000010
#define CPUID_MSR 0x00000020
#define CPUID_PAT 0x00010000
#define CPUID_CLFSH 0x00080000
#define CPUID_SSE 0x02000000
#define CPUID_SSE2 0x04000000
#define CPUID2_SSE3 0x00000001
#define CPUID2_SSSE3 0x00000200
#define CPUID2_SSE41 0x00080000
#define CPUID2_SSE42 0x00100000
#define CPUID2_POPCNT 0x00800000
#define CPUID2_AVX 0x10000000
#define CPUID2_HV 0x80000000
#define CPUID_STDEXT_AVX2 0x00000020
#define CPUID_STDEXT_CLFLUSHOPT 0x00800000
#define CPUID_STDEXT_AVX512F 0x00010000
#define AMDID_LM 0x20000000
#define MSR_IA32_MISC_ENABLE 0x1a0
#define MSR_PAT 0x277

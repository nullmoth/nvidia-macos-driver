/* FreeBSD <machine/cpufunc.h> on xnu (x86_64): the few CPU instructions LinuxKPI issues directly. */
#pragma once
#include <stdint.h>
static inline uint64_t rdtsc(void) { return __builtin_ia32_rdtsc(); }
static inline void ia32_pause(void) { __builtin_ia32_pause(); }
static inline void mfence(void) { __asm__ __volatile__("mfence" ::: "memory"); }
static inline void lfence(void) { __asm__ __volatile__("lfence" ::: "memory"); }
static inline void sfence(void) { __asm__ __volatile__("sfence" ::: "memory"); }
static inline void clflush(unsigned long addr) { __asm__ __volatile__("clflush %0" : : "m"(*(volatile char *)addr)); }
static inline void clflushopt(unsigned long addr) { __asm__ __volatile__(".byte 0x66; clflush %0" : : "m"(*(volatile char *)addr)); }
static inline void wbinvd(void) { __asm__ __volatile__("wbinvd" ::: "memory"); }
static inline void cpuid_count(unsigned ax, unsigned cx, unsigned *p)
{ __asm__ __volatile__("cpuid" : "=a"(p[0]), "=b"(p[1]), "=c"(p[2]), "=d"(p[3]) : "0"(ax), "c"(cx)); }
static inline void do_cpuid(unsigned ax, unsigned *p) { cpuid_count(ax, 0, p); }
static inline uint8_t inb(unsigned port) { uint8_t v; __asm__ __volatile__("inb %w1, %0" : "=a"(v) : "Nd"(port)); return v; }
static inline uint16_t inw(unsigned port) { uint16_t v; __asm__ __volatile__("inw %w1, %0" : "=a"(v) : "Nd"(port)); return v; }
static inline uint32_t inl(unsigned port) { uint32_t v; __asm__ __volatile__("inl %w1, %0" : "=a"(v) : "Nd"(port)); return v; }
static inline void outb(unsigned port, uint8_t v) { __asm__ __volatile__("outb %0, %w1" : : "a"(v), "Nd"(port)); }
static inline void outw(unsigned port, uint16_t v) { __asm__ __volatile__("outw %0, %w1" : : "a"(v), "Nd"(port)); }
static inline void outl(unsigned port, uint32_t v) { __asm__ __volatile__("outl %0, %w1" : : "a"(v), "Nd"(port)); }
uint64_t rdmsr(unsigned msr);
int rdmsr_safe(unsigned msr, uint64_t *val);

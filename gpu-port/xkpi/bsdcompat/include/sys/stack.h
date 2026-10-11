/* FreeBSD stack(9) capture: on macOS a kernel backtrace is printed with OSReportWithBacktrace */
#pragma once
struct stack { int depth; };
#define stack_save(st) do { (st)->depth = 0; } while (0)
#define stack_print(st) do { } while (0)
#define stack_print_ddb(st) do { } while (0)
#define STACK_DECLARE_STATIC(st)

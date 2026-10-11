/* FreeBSD <sys/_task.h>: task and timeout_task structures (layout as FreeBSD). */
#pragma once
#include <sys/queue.h>
#include <sys/_callout.h>
typedef void task_fn_t(void *context, int pending);
struct task {
    STAILQ_ENTRY(task) ta_link;
    uint16_t ta_pending;
    uint8_t ta_priority;
    uint8_t ta_flags;
    task_fn_t *ta_func;
    void *ta_context;
};
#define TASK_ENQUEUED 0x1
#define TASK_NOENQUEUE 0x2
#define TASK_NETWORK 0x4
struct taskqueue;
struct timeout_task {
    struct taskqueue *q;
    struct task t;
    struct callout c;
    int f;
};

/* FreeBSD taskqueue(9) on xnu: each queue runs its tasks on its own kernel threads, in priority then FIFO order. */
#pragma once
#include <sys/types.h>
#include <sys/_task.h>
#include <sys/callout.h>
struct taskqueue;
#define TASKQUEUE_FAIL_IF_PENDING (1 << 0)
#define TASKQUEUE_FAIL_IF_CANCELING (1 << 1)
typedef void (*taskqueue_enqueue_fn)(void *context);
typedef void (*taskqueue_callback_fn)(void *context);
enum taskqueue_callback_type { TASKQUEUE_CALLBACK_TYPE_INIT, TASKQUEUE_CALLBACK_TYPE_SHUTDOWN };
struct taskqueue *taskqueue_create(const char *name, int mflags, taskqueue_enqueue_fn enqueue, void *context);
struct taskqueue *taskqueue_create_fast(const char *name, int mflags, taskqueue_enqueue_fn enqueue, void *context);
int taskqueue_start_threads(struct taskqueue **tqp, int count, int pri, const char *name, ...) __printflike(4, 5);
int taskqueue_enqueue(struct taskqueue *queue, struct task *task);
int taskqueue_enqueue_flags(struct taskqueue *queue, struct task *task, int flags);
int taskqueue_enqueue_timeout(struct taskqueue *queue, struct timeout_task *timeout_task, int nticks);
int taskqueue_enqueue_timeout_sbt(struct taskqueue *queue, struct timeout_task *timeout_task, sbintime_t sbt, sbintime_t pr, int flags);
int taskqueue_poll_is_busy(struct taskqueue *queue, struct task *task);
int taskqueue_cancel(struct taskqueue *queue, struct task *task, u_int *pendp);
int taskqueue_cancel_timeout(struct taskqueue *queue, struct timeout_task *timeout_task, u_int *pendp);
void taskqueue_drain(struct taskqueue *queue, struct task *task);
void taskqueue_drain_timeout(struct taskqueue *queue, struct timeout_task *timeout_task);
void taskqueue_drain_all(struct taskqueue *queue);
void taskqueue_quiesce(struct taskqueue *queue);
void taskqueue_free(struct taskqueue *queue);
void taskqueue_run(struct taskqueue *queue);
void taskqueue_block(struct taskqueue *queue);
void taskqueue_unblock(struct taskqueue *queue);
int taskqueue_member(struct taskqueue *queue, struct thread *td);
void taskqueue_set_callback(struct taskqueue *queue, enum taskqueue_callback_type cb_type, taskqueue_callback_fn callback, void *context);
void taskqueue_thread_enqueue(void *context);
#define TASK_INITIALIZER(priority, func, context) \
    { .ta_pending = 0, .ta_priority = (priority), .ta_func = (func), .ta_context = (context) }
#define TASK_INIT_FLAGS(task, priority, func, context, flags) do { \
    (task)->ta_pending = 0; (task)->ta_priority = (priority); (task)->ta_flags = (flags); \
    (task)->ta_func = (func); (task)->ta_context = (context); } while (0)
#define TASK_INIT(t, p, f, c) TASK_INIT_FLAGS(t, p, f, c, 0)
void _timeout_task_init(struct taskqueue *queue, struct timeout_task *timeout_task, int priority, task_fn_t func, void *context);
#define TIMEOUT_TASK_INIT(queue, timeout_task, priority, func, context) \
    _timeout_task_init(queue, timeout_task, priority, func, context)
extern struct taskqueue *taskqueue_thread;
extern struct taskqueue *taskqueue_fast;
extern struct taskqueue *taskqueue_swi;
extern struct taskqueue *taskqueue_swi_giant;
#define TASKQUEUE_DECLARE(name) extern struct taskqueue *taskqueue_##name

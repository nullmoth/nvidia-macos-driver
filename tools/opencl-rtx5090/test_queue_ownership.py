#!/usr/bin/env python3
"""Extract actual Mesa finish/barrier/fence functions; run bounded pthread checks."""
from pathlib import Path
import argparse
import tempfile
import hashlib
import json
import re
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--original', type=Path, required=True)
parser.add_argument('--candidate', type=Path, required=True)
args = parser.parse_args()
output = tempfile.TemporaryDirectory(prefix='mesa-queue-cpu-')
ROOT = Path(output.name)
MESA = args.original


def function(text, name, occurrence=0):
    matches = list(re.finditer(r'(?m)^(?:static )?(?:void|bool)\s*\n?' + name + r'\s*\(', text))
    start = matches[occurrence].start()
    begin = text.index('{', matches[occurrence].end())
    depth = 1
    end = begin + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]


PREAMBLE = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <unistd.h>
typedef pthread_mutex_t mtx_t;
typedef pthread_cond_t cnd_t;
typedef pthread_mutex_t simple_mtx_t;
enum { mtx_plain = 0 };
typedef struct { unsigned count, waiters; uint64_t sequence; mtx_t mutex; cnd_t condvar; } util_barrier;
struct util_queue_fence { mtx_t mutex; cnd_t cond; bool signalled; };
struct context {
   util_barrier *barrier;
   unsigned count, active, returned, mutex_destroys, cond_destroys, premature;
   bool pause, paused, release, mutex_freed, cond_freed;
};
static struct context contexts[4096];
static unsigned num_contexts;
static pthread_mutex_t monitor = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
static bool pause_next, controller_stop;
static struct context *find_barrier(util_barrier *barrier) {
   for (unsigned i = num_contexts; i; --i)
      if (contexts[i-1].barrier == barrier) return &contexts[i-1];
   return NULL;
}
static struct context *find_mutex(mtx_t *mutex) {
   for (unsigned i = num_contexts; i; --i)
      if (&contexts[i-1].barrier->mutex == mutex) return &contexts[i-1];
   return NULL;
}
static struct context *find_cond(cnd_t *cond) {
   for (unsigned i = num_contexts; i; --i)
      if (&contexts[i-1].barrier->condvar == cond) return &contexts[i-1];
   return NULL;
}
static int mtx_init(mtx_t *m, int type) { (void)type; return pthread_mutex_init(m, NULL); }
static int mtx_lock(mtx_t *m) { return pthread_mutex_lock(m); }
static int mtx_unlock(mtx_t *m) { return pthread_mutex_unlock(m); }
/* Record the old path's invalid destroy requests without executing UB. Its
 * final request, after all waiters leave, destroys the real pthread object. */
static void mtx_destroy(mtx_t *m) {
   pthread_mutex_lock(&monitor);
   struct context *c = find_mutex(m);
   if (!c) { pthread_mutex_unlock(&monitor); assert(!pthread_mutex_destroy(m)); return; }
   c->mutex_destroys++;
   c->premature += c->active != 0;
   bool destroy = c->active == 0 && !c->mutex_freed;
   if (destroy) c->mutex_freed = true;
   pthread_mutex_unlock(&monitor);
   if (destroy) assert(!pthread_mutex_destroy(m));
}
static int cnd_init(cnd_t *c) { return pthread_cond_init(c, NULL); }
static int cnd_broadcast(cnd_t *c) { return pthread_cond_broadcast(c); }
static void cnd_destroy(cnd_t *cond) {
   pthread_mutex_lock(&monitor);
   struct context *c = find_cond(cond);
   if (!c) { pthread_mutex_unlock(&monitor); assert(!pthread_cond_destroy(cond)); return; }
   c->cond_destroys++;
   c->premature += c->active != 0;
   bool destroy = c->active == 0 && !c->cond_freed;
   if (destroy) c->cond_freed = true;
   pthread_mutex_unlock(&monitor);
   if (destroy) assert(!pthread_cond_destroy(cond));
}
static int cnd_wait(cnd_t *cond, mtx_t *mutex) {
   int ret = pthread_cond_wait(cond, mutex);
   assert(!ret);
   pthread_mutex_lock(&monitor);
   struct context *c = find_cond(cond);
   if (c && c->pause && !c->paused) {
      c->paused = true;
      pthread_mutex_unlock(mutex);
      pthread_cond_broadcast(&changed);
      while (!c->release) pthread_cond_wait(&changed, &monitor);
      pthread_mutex_unlock(&monitor);
      pthread_mutex_lock(mutex);
   } else pthread_mutex_unlock(&monitor);
   return ret;
}
#define u_cnd_monotonic_init cnd_init
#define u_cnd_monotonic_destroy cnd_destroy
#define u_cnd_monotonic_broadcast cnd_broadcast
#define u_cnd_monotonic_wait cnd_wait
#define simple_mtx_lock mtx_lock
#define simple_mtx_unlock mtx_unlock
'''

WRAPPERS = r'''
static void util_barrier_init(util_barrier *barrier, unsigned count) {
   actual_barrier_init(barrier, count);
   pthread_mutex_lock(&monitor);
   assert(num_contexts < 4096);
   contexts[num_contexts++] = (struct context){.barrier=barrier, .count=count,
                                             .pause=pause_next && count>1};
   pthread_mutex_unlock(&monitor);
}
static bool util_barrier_wait(util_barrier *barrier) {
   pthread_mutex_lock(&monitor);
   struct context *c = find_barrier(barrier); assert(c); c->active++;
   pthread_mutex_unlock(&monitor);
   bool result = actual_barrier_wait(barrier);
   pthread_mutex_lock(&monitor);
   c->active--; c->returned++;
   pthread_cond_broadcast(&changed);
   pthread_mutex_unlock(&monitor);
   return result;
}
typedef void (*execute_fn)(void *, void *, int);
struct job { void *data; struct util_queue_fence *fence; execute_fn execute; };
struct util_queue {
   simple_mtx_t finish_lock; mtx_t lock; cnd_t jobs_ready;
   unsigned num_threads, queued, read_idx, write_idx;
   bool create_threads_on_demand, stop;
   struct job jobs[16]; pthread_t threads[4];
};
static void util_queue_add_job_locked(struct util_queue *q, void *data,
   struct util_queue_fence *fence, execute_fn execute, execute_fn cleanup,
   size_t size, bool locked) {
   assert(locked && !cleanup && !size && q->queued < 16);
   fence->signalled = false;
   q->jobs[q->write_idx++ % 16] = (struct job){data, fence, execute};
   q->queued++;
   pthread_cond_broadcast(&q->jobs_ready);
}
#define util_queue_fence_wait _util_queue_fence_wait
'''

MAIN = r'''
static void *worker(void *arg) {
   struct util_queue *q = arg;
   for (;;) {
      pthread_mutex_lock(&q->lock);
      while (!q->queued && !q->stop) pthread_cond_wait(&q->jobs_ready, &q->lock);
      if (q->stop && !q->queued) { pthread_mutex_unlock(&q->lock); return NULL; }
      struct job job = q->jobs[q->read_idx++ % 16]; q->queued--;
      pthread_mutex_unlock(&q->lock);
      job.execute(job.data, NULL, 0);
      util_queue_fence_signal(job.fence);
   }
}
static void *controller(void *unused) {
   (void)unused;
   pthread_mutex_lock(&monitor);
   while (!controller_stop) {
      for (unsigned i=0; i<num_contexts; i++) {
         struct context *c=&contexts[i];
         if (c->paused && c->returned == c->count-1 && !c->release) {
            c->release=true; pthread_cond_broadcast(&changed);
         }
      }
      if (!controller_stop) pthread_cond_wait(&changed, &monitor);
   }
   pthread_mutex_unlock(&monitor); return NULL;
}
struct finish_request { struct util_queue *queue; unsigned rounds; };
static void *finisher(void *arg) {
   struct finish_request *request=arg;
   for (unsigned i=0; i<request->rounds; i++) util_queue_finish(request->queue);
   return NULL;
}
static void run_queue(unsigned threads) {
   struct util_queue q={.num_threads=threads, .create_threads_on_demand=true};
   assert(!pthread_mutex_init(&q.finish_lock,NULL));
   assert(!pthread_mutex_init(&q.lock,NULL));
   assert(!pthread_cond_init(&q.jobs_ready,NULL));
   for (unsigned i=0;i<threads;i++) assert(!pthread_create(&q.threads[i],NULL,worker,&q));
   pause_next=true;
   for (unsigned i=0;i<8;i++) util_queue_finish(&q);
   pause_next=false;
   pthread_t callers[3]; struct finish_request request={&q,64};
   for (unsigned i=0;i<3;i++) assert(!pthread_create(&callers[i],NULL,finisher,&request));
   for (unsigned i=0;i<3;i++) assert(!pthread_join(callers[i],NULL));
   pthread_mutex_lock(&q.lock); q.stop=true; pthread_cond_broadcast(&q.jobs_ready);
   pthread_mutex_unlock(&q.lock);
   for (unsigned i=0;i<threads;i++) assert(!pthread_join(q.threads[i],NULL));
   unsigned before=num_contexts; q.num_threads=0; util_queue_finish(&q); assert(before==num_contexts);
   assert(!pthread_cond_destroy(&q.jobs_ready));
   assert(!pthread_mutex_destroy(&q.lock)); assert(!pthread_mutex_destroy(&q.finish_lock));
}
int main(void) {
   pthread_t control; assert(!pthread_create(&control,NULL,controller,NULL));
   run_queue(1); run_queue(2); run_queue(4);
   pthread_mutex_lock(&monitor); controller_stop=true; pthread_cond_broadcast(&changed);
   pthread_mutex_unlock(&monitor); assert(!pthread_join(control,NULL));
   unsigned bad=0,premature=0,repeated=0,paused=0;
   for(unsigned i=0;i<num_contexts;i++) {
      struct context *c=&contexts[i];
      assert(!c->active && c->returned==c->count && c->mutex_freed && c->cond_freed);
      if(c->pause) { assert(c->paused && c->release); paused++; }
      bad += c->mutex_destroys!=1 || c->cond_destroys!=1 || c->premature!=0;
      premature+=c->premature; repeated+=c->mutex_destroys>1;
   }
   printf("{\"finish_calls\":%u,\"paused_waiters\":%u,\"bad_lifetimes\":%u,\"premature_destroys\":%u,\"repeated_destroys\":%u}\n",
          num_contexts,paused,bad,premature,repeated);
   return bad?1:0;
}
'''


def main():
    base = (MESA / 'src/util/u_queue.c').read_text()
    fixed = (args.candidate / 'src/util/u_queue.c').read_text()
    thread = (MESA / 'src/util/u_thread.c').read_text()
    actual_barriers = '\n'.join(function(thread, 'util_barrier_' + name, 1)
                               for name in ('init', 'destroy', 'wait'))
    actual_barriers = actual_barriers.replace('void util_barrier_init(', 'void actual_barrier_init(')
    actual_barriers = actual_barriers.replace('bool util_barrier_wait(', 'bool actual_barrier_wait(')
    fences = '\n'.join(function(base, name) for name in
                       ('util_queue_fence_signal','_util_queue_fence_wait',
                        'util_queue_fence_init','util_queue_fence_destroy'))
    # _util_queue_fence_wait occurs in FUTEX and STANDARD branches; select STANDARD.
    fences = fences.replace(function(base, '_util_queue_fence_wait'),
                            function(base, '_util_queue_fence_wait', 1))
    results = {}
    for label, source, wanted in [('fixed', fixed, 0), ('old_negative_control', base, 1)]:
        actual_queue = '\n'.join(function(source,n) for n in
                                 ('util_queue_finish_execute','util_queue_finish'))
        generated = PREAMBLE + actual_barriers + WRAPPERS + fences + actual_queue + MAIN
        cfile=ROOT/(label+'.c'); binary=ROOT/label; cfile.write_text(generated)
        subprocess.run(['xcrun','clang','-std=c11','-O1','-g','-pthread','-fsanitize=address,undefined',
                        '-fno-omit-frame-pointer',str(cfile),'-o',str(binary)],check=True,timeout=30)
        run=subprocess.run([str(binary)],capture_output=True,text=True,timeout=20)
        assert run.returncode==wanted,(label,run.returncode,run.stdout,run.stderr)
        assert not run.stderr,(label,run.stderr)
        result=json.loads(run.stdout); assert result['finish_calls']==600 and result['paused_waiters']==16
        if wanted: assert result['bad_lifetimes']==400 and result['repeated_destroys']==400 and result['premature_destroys']>0
        else: assert result['bad_lifetimes']==0
        results[label]={'exit':run.returncode,**result,'generated_sha256':hashlib.sha256(cfile.read_bytes()).hexdigest()}
    (ROOT/'cpu-result.json').write_text(json.dumps({'status':'PASS','results':results,
        'scope':'Actual extracted Mesa finish/callback/barrier/fence bodies; real pthreads. Job scheduler is a bounded test adapter. Invalid old destruction is recorded and deferred to avoid executing undefined behavior.'},indent=2)+'\n')
    print(json.dumps(results,indent=2))


if __name__=='__main__': main()

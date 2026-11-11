/*
 * nm_cxx_min.cpp: the pieces of the C++ runtime library that Mesa's C++ sources need and 10.6 does not have.
 *
 * 10.6 ships the older C++ library only (libstdc++ 6.0.9); the newer one arrived in 10.7. Mesa is compiled against
 * the newer library's headers, almost all of which is header code that lands in the objects themselves. What is
 * left over as out-of-line library symbols is 26 names; the older library and its runtime provide 17 of them with
 * the same binary interface (operator new and delete, the exception runtime, the type-information classes). The
 * other nine are defined here and linked into the bundle, so the 10.6 build links against the older library and
 * never asks for the newer one.
 */
#include <cstdlib>
#include <mutex>
#include <new>
#include <stdexcept>
#include <pthread.h>
#include <stdio.h>

namespace std {
inline namespace __1 {

/* std::mutex is a plain pthread mutex in this library's layout. */
mutex::~mutex() noexcept { pthread_mutex_destroy(&__m_); }
void mutex::lock() { if (pthread_mutex_lock(&__m_) != 0) abort(); }
void mutex::unlock() noexcept { pthread_mutex_unlock(&__m_); }

/* The hash containers ask for the next prime at or above n when they grow. Trial division is enough: the sizes
 * are bucket counts, and a table grows a few dozen times in its life. */
size_t __next_prime(size_t n)
{
    if (n <= 2) return 2;
    if ((n & 1) == 0) n++;
    for (;; n += 2) {
        bool prime = true;
        for (size_t d = 3; d <= n / d; d += 2)
            if (n % d == 0) { prime = false; break; }
        if (prime) return n;
    }
}

}  // namespace __1

/*
 * The header code throws std::length_error when a container is asked to grow past its maximum size. Building
 * that exception needs a constructor the older library does not have. Nothing in Mesa recovers from it, so at
 * this floor the condition ends the process with its message instead of unwinding.
 */
length_error::~length_error() noexcept {}

bad_array_new_length::~bad_array_new_length() noexcept {}
const char *bad_array_new_length::what() const noexcept { return "bad array new length"; }

}  // namespace std

/* The constructor is defined by its linker names rather than as a member, because a member definition would have
 * to construct the class's string member, whose own constructor is one more symbol of the newer library. Both
 * forms (complete object and base object) are the same function here: neither returns. */
extern "C" {
__attribute__((noreturn, visibility("hidden"))) void nm_logic_error_ctor(void *self, const char *what_arg) __asm__("__ZNSt11logic_errorC1EPKc");
__attribute__((noreturn, visibility("hidden"))) void nm_logic_error_ctor_base(void *self, const char *what_arg) __asm__("__ZNSt11logic_errorC2EPKc");
void nm_logic_error_ctor(void *self, const char *what_arg)
{
    (void)self;
    fprintf(stderr, "nm-gl: fatal C++ logic error: %s\n", what_arg ? what_arg : "");
    abort();
}
void nm_logic_error_ctor_base(void *self, const char *what_arg) { nm_logic_error_ctor(self, what_arg); }

/* The same for the exception an overflowing array allocation would throw: its constructor would need the base
 * class's, which the older library only has as header code of its own. An array size that overflows ends the
 * process here. */
__attribute__((noreturn, visibility("hidden"))) void nm_bad_array_ctor(void *self) __asm__("__ZNSt20bad_array_new_lengthC1Ev");
__attribute__((noreturn, visibility("hidden"))) void nm_bad_array_ctor_base(void *self) __asm__("__ZNSt20bad_array_new_lengthC2Ev");
void nm_bad_array_ctor(void *self)
{
    (void)self;
    fprintf(stderr, "nm-gl: fatal: array allocation size overflow\n");
    abort();
}
void nm_bad_array_ctor_base(void *self) { nm_bad_array_ctor(self); }
}

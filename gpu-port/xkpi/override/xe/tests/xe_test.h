/* SPDX-License-Identifier: MIT */
/*
 * xe's KUnit hooks, with KUnit off (the port builds no tests). Written for this port; Linux's version is GPL-2.0.
 * XE_TEST_DECLARE drops its declaration, XE_TEST_ONLY is false, so test-only branches compile away.
 */
#ifndef _XE_TEST_H_
#define _XE_TEST_H_

#define XE_TEST_DECLARE(x)
#define XE_TEST_ONLY(x) 0
#define xe_cur_kunit_priv(id) NULL
#define xe_cur_kunit() NULL

#endif

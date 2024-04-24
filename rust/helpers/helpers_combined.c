// SPDX-License-Identifier: GPL-2.0

#include <linux/bug.h>
#include "helpers.h"

__rust_helper __noreturn void rust_helper_BUG(void)
{
	BUG();
}
// SPDX-License-Identifier: GPL-2.0

#include <linux/build_bug.h>

/*
 * `bindgen` binds the C `size_t` type as the Rust `usize` type, so we can
 * use it in contexts where Rust expects a `usize` like slice (array) indices.
 * `usize` is defined to be the same as C's `uintptr_t` type (can hold any
 * pointer) but not necessarily the same as `size_t` (can hold the size of any
 * single object). Most modern platforms use the same concrete integer type for
 * both of them, but in case we find ourselves on a platform where
 * that's not true, fail early instead of risking ABI or
 * integer-overflow issues.
 *
 * If your platform fails this assertion, it means that you are in
 * danger of integer-overflow bugs (even if you attempt to add
 * `--no-size_t-is-usize`). It may be easiest to change the kernel ABI on
 * your platform such that `size_t` matches `uintptr_t` (i.e., to increase
 * `size_t`, because `uintptr_t` has to be at least as big as `size_t`).
 */
static_assert(
	sizeof(size_t) == sizeof(uintptr_t) &&
	__alignof__(size_t) == __alignof__(uintptr_t),
	"Rust code expects C `size_t` to match Rust `usize`"
);
// SPDX-License-Identifier: GPL-2.0

#include <linux/export.h>
#include <linux/errname.h>
#include "helpers.h"

__rust_helper const char *rust_helper_errname(int err)
{
	return errname(err);
}
// SPDX-License-Identifier: GPL-2.0

#include <linux/err.h>
#include <linux/export.h>
#include "helpers.h"

__rust_helper __force void *rust_helper_ERR_PTR(long err)
{
	return ERR_PTR(err);
}

__rust_helper bool rust_helper_IS_ERR(__force const void *ptr)
{
	return IS_ERR(ptr);
}

__rust_helper long rust_helper_PTR_ERR(__force const void *ptr)
{
	return PTR_ERR(ptr);
}
// SPDX-License-Identifier: GPL-2.0

#include <kunit/test-bug.h>
#include <linux/export.h>
#include "helpers.h"

__rust_helper struct kunit *rust_helper_kunit_get_current_test(void)
{
	return kunit_get_current_test();
}
// SPDX-License-Identifier: GPL-2.0

#include <linux/export.h>
#include <linux/mutex.h>
#include "helpers.h"

__rust_helper void rust_helper_mutex_lock(struct mutex *lock)
{
	mutex_lock(lock);
}
// SPDX-License-Identifier: GPL-2.0

#include <linux/export.h>
#include <linux/refcount.h>
#include "helpers.h"

__rust_helper refcount_t rust_helper_REFCOUNT_INIT(int n)
{
	return (refcount_t)REFCOUNT_INIT(n);
}

__rust_helper void rust_helper_refcount_inc(refcount_t *r)
{
	refcount_inc(r);
}

__rust_helper bool rust_helper_refcount_dec_and_test(refcount_t *r)
{
	return refcount_dec_and_test(r);
}
// SPDX-License-Identifier: GPL-2.0

#include <linux/export.h>
#include <linux/sched/signal.h>
#include "helpers.h"

__rust_helper int rust_helper_signal_pending(struct task_struct *t)
{
	return signal_pending(t);
}
// SPDX-License-Identifier: GPL-2.0

#include <linux/export.h>
#include <linux/spinlock.h>
#include "helpers.h"

__rust_helper void rust_helper___spin_lock_init(spinlock_t *lock, const char *name,
				  struct lock_class_key *key)
{
#ifdef CONFIG_DEBUG_SPINLOCK
	__raw_spin_lock_init(spinlock_check(lock), name, key, LD_WAIT_CONFIG);
#else
	spin_lock_init(lock);
#endif
}

__rust_helper void rust_helper_spin_lock(spinlock_t *lock)
{
	spin_lock(lock);
}

__rust_helper void rust_helper_spin_unlock(spinlock_t *lock)
{
	spin_unlock(lock);
}
// SPDX-License-Identifier: GPL-2.0

#include <linux/export.h>
#include <linux/sched/task.h>
#include "helpers.h"

__rust_helper struct task_struct *rust_helper_get_current(void)
{
	return current;
}

__rust_helper void rust_helper_get_task_struct(struct task_struct *t)
{
	get_task_struct(t);
}

__rust_helper void rust_helper_put_task_struct(struct task_struct *t)
{
	put_task_struct(t);
}
// SPDX-License-Identifier: GPL-2.0

#include <linux/export.h>
#include <linux/wait.h>
#include "helpers.h"

__rust_helper void rust_helper_init_wait(struct wait_queue_entry *wq_entry)
{
	init_wait(wq_entry);
}
// SPDX-License-Identifier: GPL-2.0

#include <linux/export.h>
#include <linux/workqueue.h>
#include "helpers.h"

__rust_helper void rust_helper_init_work_with_key(struct work_struct *work, work_func_t func,
				    bool onstack, const char *name,
				    struct lock_class_key *key)
{
	__init_work(work, onstack);
	work->data = (atomic_long_t)WORK_DATA_INIT();
	lockdep_init_map(&work->lockdep_map, name, key, 0);
	INIT_LIST_HEAD(&work->entry);
	work->func = func;
}

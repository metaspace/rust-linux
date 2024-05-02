#include <linux/xarray.h>

__rust_helper void rust_helper_xa_init_flags(struct xarray *xa, gfp_t flags)
{
	xa_init_flags(xa, flags);
}

__rust_helper bool rust_helper_xa_empty(struct xarray *xa)
{
	return xa_empty(xa);
}

__rust_helper int rust_helper_xa_alloc(struct xarray *xa, u32 *id, void *entry,
			 struct xa_limit limit, gfp_t gfp)
{
	return xa_alloc(xa, id, entry, limit, gfp);
}

__rust_helper void rust_helper_xa_lock(struct xarray *xa)
{
	xa_lock(xa);
}

__rust_helper void rust_helper_xa_unlock(struct xarray *xa)
{
	xa_unlock(xa);
}

__rust_helper int rust_helper_xa_err(void *entry)
{
	return xa_err(entry);
}

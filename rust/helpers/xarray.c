#include <linux/xarray.h>

void rust_helper_xa_init_flags(struct xarray *xa, gfp_t flags)
{
	xa_init_flags(xa, flags);
}
EXPORT_SYMBOL_GPL(rust_helper_xa_init_flags);

bool rust_helper_xa_empty(struct xarray *xa)
{
	return xa_empty(xa);
}
EXPORT_SYMBOL_GPL(rust_helper_xa_empty);

int rust_helper_xa_alloc(struct xarray *xa, u32 *id, void *entry,
			 struct xa_limit limit, gfp_t gfp)
{
	return xa_alloc(xa, id, entry, limit, gfp);
}
EXPORT_SYMBOL_GPL(rust_helper_xa_alloc);

void rust_helper_xa_lock(struct xarray *xa)
{
	xa_lock(xa);
}
EXPORT_SYMBOL_GPL(rust_helper_xa_lock);

void rust_helper_xa_unlock(struct xarray *xa)
{
	xa_unlock(xa);
}
EXPORT_SYMBOL_GPL(rust_helper_xa_unlock);

int rust_helper_xa_err(void *entry)
{
	return xa_err(entry);
}
EXPORT_SYMBOL_GPL(rust_helper_xa_err);

#include <linux/cacheflush.h>
#include <linux/mm.h>
#include <linux/pagemap.h>

void rust_helper_folio_get(struct folio *folio)
{
	folio_get(folio);
}
EXPORT_SYMBOL_GPL(rust_helper_folio_get);

void rust_helper_folio_put(struct folio *folio)
{
	folio_put(folio);
}
EXPORT_SYMBOL_GPL(rust_helper_folio_put);

struct page *rust_helper_folio_page(struct folio *folio, size_t n)
{
	return folio_page(folio, n);
}

loff_t rust_helper_folio_pos(struct folio *folio)
{
	return folio_pos(folio);
}
EXPORT_SYMBOL_GPL(rust_helper_folio_pos);

size_t rust_helper_folio_size(struct folio *folio)
{
	return folio_size(folio);
}
EXPORT_SYMBOL_GPL(rust_helper_folio_size);

void rust_helper_folio_mark_uptodate(struct folio *folio)
{
	folio_mark_uptodate(folio);
}
EXPORT_SYMBOL_GPL(rust_helper_folio_mark_uptodate);

void rust_helper_folio_set_error(struct folio *folio)
{
	folio_set_error(folio);
}
EXPORT_SYMBOL_GPL(rust_helper_folio_set_error);

#ifndef CONFIG_NUMA
struct folio* rust_helper_folio_alloc(gfp_t gfp, unsigned int order)
{
  return folio_alloc(gfp, order);
}
EXPORT_SYMBOL_GPL(rust_helper_folio_alloc);
#endif

void rust_helper_flush_dcache_folio(struct folio *folio)
{
	flush_dcache_folio(folio);
}
EXPORT_SYMBOL_GPL(rust_helper_flush_dcache_folio);

void *rust_helper_kmap_local_folio(struct folio *folio, size_t offset)
{
	return kmap_local_folio(folio, offset);
}
EXPORT_SYMBOL_GPL(rust_helper_kmap_local_folio);

void *rust_helper_kmap(struct page *page)
{
	return kmap(page);
}
EXPORT_SYMBOL_GPL(rust_helper_kmap);

void rust_helper_kunmap(struct page *page)
{
	return kunmap(page);
}
EXPORT_SYMBOL_GPL(rust_helper_kunmap);

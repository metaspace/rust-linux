#include <linux/cacheflush.h>
#include <linux/mm.h>
#include <linux/pagemap.h>

__rust_helper void rust_helper_folio_get(struct folio *folio)
{
	folio_get(folio);
}

__rust_helper void rust_helper_folio_put(struct folio *folio)
{
	folio_put(folio);
}

__rust_helper struct page *rust_helper_folio_page(struct folio *folio, size_t n)
{
	return folio_page(folio, n);
}

__rust_helper loff_t rust_helper_folio_pos(struct folio *folio)
{
	return folio_pos(folio);
}

__rust_helper size_t rust_helper_folio_size(struct folio *folio)
{
	return folio_size(folio);
}

__rust_helper void rust_helper_folio_mark_uptodate(struct folio *folio)
{
	folio_mark_uptodate(folio);
}

__rust_helper void rust_helper_folio_set_error(struct folio *folio)
{
	folio_set_error(folio);
}

#ifndef CONFIG_NUMA
__rust_helper struct folio* rust_helper_folio_alloc(gfp_t gfp, unsigned int order)
{
  return folio_alloc(gfp, order);
}
#endif

__rust_helper void rust_helper_flush_dcache_folio(struct folio *folio)
{
	flush_dcache_folio(folio);
}

__rust_helper void *rust_helper_kmap_local_folio(struct folio *folio, size_t offset)
{
	return kmap_local_folio(folio, offset);
}

__rust_helper void *rust_helper_kmap(struct page *page)
{
	return kmap(page);
}

__rust_helper void rust_helper_kunmap(struct page *page)
{
	return kunmap(page);
}

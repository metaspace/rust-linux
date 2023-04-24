
#include <linux/bio.h>

__attribute__((always_inline))
void rust_helper_bio_advance_iter_single(const struct bio *bio,
                                           struct bvec_iter *iter, unsigned int bytes)
{
	bio_advance_iter_single(bio, iter, bytes);
}

__attribute__((always_inline)) void *rust_helper_kmap(struct page *page)
{
	return kmap(page);
}

__attribute__((always_inline)) void rust_helper_kunmap(struct page *page)
{
	return kunmap(page);
}

__attribute__((always_inline)) void *rust_helper_kmap_atomic(struct page *page)
{
	return kmap_atomic(page);
}

__attribute__((always_inline)) void rust_helper_kunmap_atomic(void* address)
{
	kunmap_atomic(address);
}

__attribute__((always_inline)) struct page *
rust_helper_alloc_pages(gfp_t gfp_mask, unsigned int order)
{
	return alloc_pages(gfp_mask, order);
}

__attribute__((always_inline)) void rust_helper_spin_lock_irq(spinlock_t *lock)
{
	spin_lock_irq(lock);
}

__attribute__((always_inline)) void
rust_helper_spin_unlock_irq(spinlock_t *lock)
{
	spin_unlock_irq(lock);
}
__attribute__((always_inline)) unsigned long
rust_helper_spin_lock_irqsave(spinlock_t *lock)
{
	unsigned long flags;

	spin_lock_irqsave(lock, flags);

	return flags;
}
__attribute__((always_inline)) void
rust_helper_spin_unlock_irqrestore(spinlock_t *lock, unsigned long flags)
{
	spin_unlock_irqrestore(lock, flags);
}

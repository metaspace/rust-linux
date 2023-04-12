
#include <linux/bio.h>
#include <linux/blk-mq.h>

__attribute__((always_inline)) void
rust_helper_bio_advance_iter_single(const struct bio *bio,
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

__attribute__((always_inline)) void rust_helper_kunmap_atomic(void *address)
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

__attribute__((always_inline)) void rust_helper_spin_lock(spinlock_t *lock)
{
	spin_lock(lock);
}

__attribute__((always_inline)) void rust_helper_spin_unlock(spinlock_t *lock)
{
	spin_unlock(lock);
}
__attribute__((always_inline)) void rust_helper_refcount_inc(refcount_t *r)
{
	refcount_inc(r);
}

__attribute__((always_inline)) bool
rust_helper_refcount_dec_and_test(refcount_t *r)
{
	return refcount_dec_and_test(r);
}

__attribute__((always_inline)) bool rust_helper_IS_ERR(__force const void *ptr)
{
	return IS_ERR(ptr);
}

__attribute__((always_inline)) long rust_helper_PTR_ERR(__force const void *ptr)
{
	return PTR_ERR(ptr);
}

__attribute__((always_inline)) void *
rust_helper_blk_mq_rq_to_pdu(struct request *rq)
{
	return blk_mq_rq_to_pdu(rq);
}

__attribute__((always_inline)) struct request *
rust_helper_blk_mq_tag_to_rq(struct blk_mq_tags *tags, unsigned int tag)
{
	return blk_mq_tag_to_rq(tags, tag);
}

__attribute__((always_inline)) unsigned short
rust_helper_blk_rq_nr_phys_segments(struct request *rq)
{
	return blk_rq_nr_phys_segments(rq);
}

__attribute__((always_inline)) unsigned int
rust_helper_blk_rq_payload_bytes(struct request *rq)
{
	return blk_rq_payload_bytes(rq);
}

__attribute__((always_inline)) u32
rust_helper_readl(const volatile void __iomem *addr)
{
	return readl(addr);
}

__attribute__((always_inline)) u64
rust_helper_readq(const volatile void __iomem *addr)
{
	return readq(addr);
}

__attribute__((always_inline)) void
rust_helper_writel(u32 value, volatile void __iomem *addr)
{
	writel(value, addr);
}

__attribute__((always_inline)) void
rust_helper_writeq(u64 value, volatile void __iomem *addr)
{
	writeq(value, addr);
}

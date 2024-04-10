// SPDX-License-Identifier: GPL-2.0

#include <linux/bio.h>
#include <linux/blk-mq.h>
#include <linux/blkdev.h>

struct bio_vec rust_helper_req_bvec(struct request *rq)
{
	return req_bvec(rq);
}
EXPORT_SYMBOL_GPL(rust_helper_req_bvec);

void *rust_helper_blk_mq_rq_to_pdu(struct request *rq)
{
	return blk_mq_rq_to_pdu(rq);
}
EXPORT_SYMBOL_GPL(rust_helper_blk_mq_rq_to_pdu);

struct request *rust_helper_blk_mq_rq_from_pdu(void *pdu)
{
	return blk_mq_rq_from_pdu(pdu);
}
EXPORT_SYMBOL_GPL(rust_helper_blk_mq_rq_from_pdu);

void rust_helper_bio_advance_iter_single(const struct bio *bio,
					 struct bvec_iter *iter,
					 unsigned int bytes)
{
	bio_advance_iter_single(bio, iter, bytes);
}
EXPORT_SYMBOL_GPL(rust_helper_bio_advance_iter_single);

// SPDX-License-Identifier: GPL-2.0

#include <linux/bio.h>
#include <linux/blk-mq.h>
#include <linux/blkdev.h>

void *rust_helper_blk_mq_rq_to_pdu(struct request *rq)
{
	return blk_mq_rq_to_pdu(rq);
}

struct request *rust_helper_blk_mq_rq_from_pdu(void *pdu)
{
	return blk_mq_rq_from_pdu(pdu);
}

void rust_helper_bio_advance_iter_single(const struct bio *bio,
					 struct bvec_iter *iter,
					 unsigned int bytes)
{
	bio_advance_iter_single(bio, iter, bytes);
}

struct request *rust_helper_blk_mq_tag_to_rq(struct blk_mq_tags *tags,
					     unsigned int tag)
{
	return blk_mq_tag_to_rq(tags, tag);
}

unsigned int rust_helper_blk_rq_payload_bytes(struct request *rq)
{
	return blk_rq_payload_bytes(rq);
}

unsigned short rust_helper_blk_rq_nr_phys_segments(struct request *rq)
{
	return blk_rq_nr_phys_segments(rq);
}

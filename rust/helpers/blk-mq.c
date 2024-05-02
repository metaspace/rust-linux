#include <linux/bio.h>
#include <linux/blk-mq.h>
#include <linux/blkdev.h>

__rust_helper struct bio_vec rust_helper_req_bvec(struct request *rq)
{
	return req_bvec(rq);
}

__rust_helper void *rust_helper_blk_mq_rq_to_pdu(struct request *rq)
{
	return blk_mq_rq_to_pdu(rq);
}

__rust_helper struct request *rust_helper_blk_mq_rq_from_pdu(void *pdu)
{
	return blk_mq_rq_from_pdu(pdu);
}

__rust_helper void rust_helper_bio_advance_iter_single(const struct bio *bio,
					 struct bvec_iter *iter,
					 unsigned int bytes)
{
	bio_advance_iter_single(bio, iter, bytes);
}

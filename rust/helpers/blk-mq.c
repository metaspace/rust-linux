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

// ----
bool rust_helper_req_ref_inc_not_zero(struct request *req)
{
	return atomic_inc_not_zero(&req->ref);
}
EXPORT_SYMBOL_GPL(rust_helper_req_ref_inc_not_zero);

bool rust_helper_req_ref_put_and_test(struct request *req)
{
	return atomic_dec_and_test(&req->ref);
}
EXPORT_SYMBOL_GPL(rust_helper_req_ref_put_and_test);

void rust_helper_blk_mq_free_request_internal(struct request *req)
{
	__blk_mq_free_request(req);
}
EXPORT_SYMBOL_GPL(rust_helper_blk_mq_free_request_internal);

struct request *rust_helper_blk_mq_tag_to_rq(struct blk_mq_tags *tags,
					     unsigned int tag)
{
	return blk_mq_tag_to_rq(tags, tag);
}
EXPORT_SYMBOL_GPL(rust_helper_blk_mq_tag_to_rq);

unsigned int rust_helper_blk_rq_payload_bytes(struct request *rq)
{
	return blk_rq_payload_bytes(rq);
}
EXPORT_SYMBOL_GPL(rust_helper_blk_rq_payload_bytes);

unsigned short rust_helper_blk_rq_nr_phys_segments(struct request *rq)
{
	return blk_rq_nr_phys_segments(rq);
}
EXPORT_SYMBOL_GPL(rust_helper_blk_rq_nr_phys_segments);

// SPDX-License-Identifier: GPL-2.0
/*
 * Copyright 2023 Andreas Hindborg <a.hindborg@samsung.com>
 */
#include <linux/blkzoned.h>
#include <linux/ublk_cmd.h>
#include "ublk_drv.h"

void ublk_set_nr_zones(struct ublk_device *ub)
{
	const struct ublk_param_basic *p = &ub->params.basic;

	if (ub->dev_info.flags & UBLK_F_ZONED && p->chunk_sectors)
		ub->ub_disk->nr_zones = p->dev_sectors / p->chunk_sectors;
}

void ublk_dev_param_zoned_apply(struct ublk_device *ub)
{
	const struct ublk_param_zoned *p = &ub->params.zoned;

	if (ub->dev_info.flags & UBLK_F_ZONED) {
		disk_set_max_active_zones(ub->ub_disk, p->max_active_zones);
		disk_set_max_open_zones(ub->ub_disk, p->max_open_zones);
	}
}

int ublk_revalidate_disk_zones(struct gendisk *disk)
{
	return blk_revalidate_disk_zones(disk, NULL);
}

// Based on virtblk_alloc_report_buffer
static void *ublk_alloc_report_buffer(struct ublk_device *ublk,
				      unsigned int nr_zones,
				      unsigned int zone_sectors, size_t *buflen)
{
	struct request_queue *q = ublk->ub_disk->queue;
	size_t bufsize;
	void *buf;

	nr_zones = min_t(unsigned int, nr_zones,
			 get_capacity(ublk->ub_disk) >> ilog2(zone_sectors));

	bufsize = nr_zones * sizeof(struct blk_zone);
	bufsize =
		min_t(size_t, bufsize, queue_max_hw_sectors(q) << SECTOR_SHIFT);
	bufsize = min_t(size_t, bufsize, queue_max_segments(q) << PAGE_SHIFT);

	while (bufsize >= sizeof(struct blk_zone)) {
		buf = __vmalloc(bufsize, GFP_KERNEL | __GFP_NORETRY);
		if (buf) {
			*buflen = bufsize;
			return buf;
		}
		bufsize >>= 1;
	}

	bufsize = 0;
	return NULL;
}

int ublk_report_zones(struct gendisk *disk, sector_t sector,
		      unsigned int nr_zones, report_zones_cb cb, void *data)
{
	unsigned int done_zones = 0;
	struct ublk_device *ub = disk->private_data;
	unsigned int zone_size_sectors = disk->queue->limits.chunk_sectors;
	unsigned int first_zone = sector >> ilog2(zone_size_sectors);
	struct blk_zone *buffer;
	size_t buffer_length;
	unsigned int max_zones_per_request;

	if (!(ub->dev_info.flags & UBLK_F_ZONED))
		return -EOPNOTSUPP;

	nr_zones = min_t(unsigned int, ub->ub_disk->nr_zones - first_zone,
			 nr_zones);

	buffer = ublk_alloc_report_buffer(ub, nr_zones, zone_size_sectors,
					  &buffer_length);
	if (!buffer)
		return -ENOMEM;

	max_zones_per_request = buffer_length / sizeof(struct blk_zone);

	while (done_zones < nr_zones) {
		unsigned int remaining_zones = nr_zones - done_zones;
		unsigned int zones_in_request = min_t(
			unsigned int, remaining_zones, max_zones_per_request);
		int err = 0;
		struct request *req;
		struct ublk_rq_data *pdu;
		blk_status_t status;

		memset(buffer, 0, buffer_length);

		req = blk_mq_alloc_request(disk->queue, REQ_OP_DRV_IN, 0);
		if (IS_ERR(req))
			return PTR_ERR(req);

		pdu = blk_mq_rq_to_pdu(req);
		pdu->operation = UBLK_IO_OP_REPORT_ZONES;
		pdu->sector = sector;
		pdu->nr_sectors = remaining_zones * zone_size_sectors;

		err = blk_rq_map_kern(disk->queue, req, buffer, buffer_length,
					GFP_KERNEL);
		if (err) {
			blk_mq_free_request(req);
			kvfree(buffer);
			return err;
		}

		status = blk_execute_rq(req, 0);
		err = blk_status_to_errno(status);
		blk_mq_free_request(req);
		if (err) {
			kvfree(buffer);
			return err;
		}

		for (unsigned int i = 0; i < zones_in_request; i++) {
			struct blk_zone *zone = buffer + i;

			err = cb(zone, i, data);
			if (err)
				return err;

			done_zones++;
			sector += zone_size_sectors;

			/* A zero length zone means don't ask for more zones */
			if (!zone->len) {
				kvfree(buffer);
				return done_zones;
			}
		}
	}

	kvfree(buffer);
	return done_zones;
}

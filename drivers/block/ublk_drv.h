/* SPDX-License-Identifier: GPL-2.0 */

#ifndef _UBLK_DRV_H
#define _UBLK_DRV_H

#include <uapi/linux/ublk_cmd.h>
#include <linux/blk-mq.h>
#include <linux/cdev.h>

struct ublk_device {
	struct gendisk *ub_disk;

	char *__queues;

	unsigned int queue_size;
	struct ublksrv_ctrl_dev_info dev_info;

	struct blk_mq_tag_set tag_set;

	struct cdev cdev;
	struct device cdev_dev;

#define UB_STATE_OPEN 0
#define UB_STATE_USED 1
#define UB_STATE_DELETED 2
	unsigned long state;
	int ub_number;

	struct mutex mutex;

	spinlock_t mm_lock;
	struct mm_struct *mm;

	struct ublk_params params;

	struct completion completion;
	unsigned int nr_queues_ready;
	unsigned int nr_privileged_daemon;

	/*
	 * Our ubq->daemon may be killed without any notification, so
	 * monitor each queue's daemon periodically
	 */
	struct delayed_work monitor_work;
	struct work_struct quiesce_work;
	struct work_struct stop_work;
};

struct ublk_rq_data {
	struct llist_node node;
	struct callback_head work;
	enum ublk_op operation;
	__u64 sector;
	__u32 nr_sectors;
};

void ublk_set_nr_zones(struct ublk_device *ub);
void ublk_dev_param_zoned_apply(struct ublk_device *ub);
int ublk_revalidate_disk_zones(struct gendisk *disk);

#ifdef CONFIG_BLK_DEV_UBLK_ZONED
int ublk_report_zones(struct gendisk *disk, sector_t sector,
		      unsigned int nr_zones, report_zones_cb cb,
		      void *data);
#else
#define ublk_report_zones NULL
#endif

#endif

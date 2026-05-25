/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * UAPI for /dev/memlog — LLM context-window audit log.
 *
 * See wintermute autobuilder PRD-memlog.md.  Records are CBOR-encoded
 * blobs ≤ MEMLOG_RECORD_MAX bytes; the kernel preserves them in a
 * fixed-size circular buffer that survives the writing process's death.
 */
#ifndef _UAPI_LINUX_MEMLOG_H
#define _UAPI_LINUX_MEMLOG_H

#include <linux/ioctl.h>
#include <linux/types.h>

#define MEMLOG_DEVICE_NAME	"memlog"
#define MEMLOG_MAJOR		244	/* unreserved char-major; soft-collidable, change if needed */
#define MEMLOG_RECORD_MAX	(64 * 1024)
#define MEMLOG_SCHEMA_VERSION	1

#define MEMLOG_IOCTL_BASE	0xCB	/* CBor — not in linux/ioctl.txt at v0.1 */
#define MEMLOG_IOCTL_STATS	_IOR(MEMLOG_IOCTL_BASE, 1, struct memlog_stats)
#define MEMLOG_IOCTL_CLEAR	_IO(MEMLOG_IOCTL_BASE, 2)
#define MEMLOG_IOCTL_SET_RING_SIZE _IOW(MEMLOG_IOCTL_BASE, 3, __u32)
#define MEMLOG_IOCTL_FILTER_UID	_IOW(MEMLOG_IOCTL_BASE, 4, __u32)  /* 0 = all (cap-gated) */
#define MEMLOG_IOCTL_GET_VERSION _IOR(MEMLOG_IOCTL_BASE, 5, __u32)

/*
 * Each record stored in the ring is prefixed by this header.  Readers
 * consume <header><cbor blob> tuples; the kernel never inspects the blob.
 */
struct memlog_record_header {
	__u32 magic;		/* MEMLOG_RECORD_MAGIC */
	__u32 schema_version;
	__u32 length;		/* CBOR blob length following this header */
	__u32 _reserved;
	__u64 ts_ns;		/* ktime_get_real_ns at write */
	__u64 seq;		/* monotonic sequence number, kernel-issued */
	__u32 uid;
	__u32 pid;
	__u8  session_id[16];	/* agent_session_id if available; zeroes otherwise */
} __attribute__((packed));

#define MEMLOG_RECORD_MAGIC	0x4D4C4F47	/* 'MLOG' */

struct memlog_stats {
	__u64 total_writes;	/* lifetime since boot/load */
	__u64 total_evictions;
	__u64 records_in_ring;
	__u32 ring_bytes;
	__u32 ring_capacity;
	__u64 oldest_ts_ns;
	__u64 newest_ts_ns;
};

#endif /* _UAPI_LINUX_MEMLOG_H */

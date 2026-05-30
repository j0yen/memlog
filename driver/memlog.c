// SPDX-License-Identifier: GPL-2.0
/*
 * memlog — /dev/memlog character device.
 *
 * Per-uid circular log of LLM context-compaction records. Survives process
 * death (records live in kernel memory). Variable-length records, each ≤
 * MEMLOG_RECORD_MAX (64 KB). Ring capacity is sysctl-tunable
 * (kernel.memlog.ring_size, bytes); oldest records evict first when full.
 *
 * v0.1 covers PRD Phases 0+1. Tracepoint and AgentNS session-id binding
 * land in later phases. Userspace contract is include/uapi/linux/memlog.h.
 */

#include <linux/cdev.h>
#include <linux/capability.h>
#include <linux/cred.h>
#include <linux/fs.h>
#include <linux/init.h>
#include <linux/ktime.h>
#include <linux/list.h>
#include <linux/miscdevice.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/sysctl.h>
#include <linux/uaccess.h>
#include <linux/uidgid.h>
#include <linux/version.h>

#include "../include/uapi/linux/memlog.h"

#define MEMLOG_DRV_VERSION		"0.1"
#define MEMLOG_RING_DEFAULT		(4 * 1024 * 1024)	/* 4 MB */
#define MEMLOG_RING_MIN			(64 * 1024)		/* 64 KB */
#define MEMLOG_RING_MAX			(256 * 1024 * 1024)	/* 256 MB */

static int memlog_gid = -1;
module_param(memlog_gid, int, 0644);
MODULE_PARM_DESC(memlog_gid,
	"GID allowed to write /dev/memlog (default: -1 = CAP_SYS_ADMIN only)");

struct memlog_entry {
	struct list_head list;
	struct memlog_record_header header;
	u8 payload[];	/* header.length bytes */
};

static DEFINE_SPINLOCK(memlog_lock);
static LIST_HEAD(memlog_entries);
static u64 memlog_next_seq = 1;
static u64 memlog_total_writes;
static u64 memlog_total_evictions;
static size_t memlog_used_bytes;
static size_t memlog_capacity = MEMLOG_RING_DEFAULT;
/*
 * Sysctl exposes the capacity as int (compatibility with proc_dointvec_minmax).
 * Bytes — kept in sync with memlog_capacity under memlog_lock.
 */
static int memlog_capacity_sysctl = MEMLOG_RING_DEFAULT;
static const int memlog_capacity_min = MEMLOG_RING_MIN;
static const int memlog_capacity_max = MEMLOG_RING_MAX;

struct memlog_reader {
	u64 last_seq_read;	/* 0 = haven't read anything */
	u32 filter_uid;		/* the uid this fd sees; default = caller's */
	bool show_all;		/* CAP_SYS_ADMIN reader that requested uid=0 */
};

static size_t entry_total_size(const struct memlog_entry *e)
{
	return sizeof(*e) + e->header.length;
}

/* Caller holds memlog_lock. Evicts oldest entries until at least @want bytes free. */
static void memlog_evict_for(size_t want)
{
	while (memlog_used_bytes + want > memlog_capacity &&
	       !list_empty(&memlog_entries)) {
		struct memlog_entry *e = list_first_entry(&memlog_entries,
							  struct memlog_entry,
							  list);
		size_t sz = entry_total_size(e);
		list_del(&e->list);
		memlog_used_bytes -= sz;
		memlog_total_evictions++;
		kfree(e);
	}
}

/* Caller holds memlog_lock. Evicts until @bytes_target or empty. */
static void memlog_evict_to_capacity(size_t bytes_target)
{
	while (memlog_used_bytes > bytes_target &&
	       !list_empty(&memlog_entries)) {
		struct memlog_entry *e = list_first_entry(&memlog_entries,
							  struct memlog_entry,
							  list);
		size_t sz = entry_total_size(e);
		list_del(&e->list);
		memlog_used_bytes -= sz;
		memlog_total_evictions++;
		kfree(e);
	}
}

static bool memlog_writer_allowed(void)
{
	kgid_t gid;

	if (capable(CAP_SYS_ADMIN))
		return true;
	if (memlog_gid < 0)
		return false;
	gid = make_kgid(current_user_ns(), memlog_gid);
	if (!gid_valid(gid))
		return false;
	return in_group_p(gid);
}

static int memlog_open(struct inode *inode, struct file *filp)
{
	struct memlog_reader *r;

	r = kzalloc(sizeof(*r), GFP_KERNEL);
	if (!r)
		return -ENOMEM;
	r->filter_uid = from_kuid(&init_user_ns, current_uid());
	r->show_all = false;
	r->last_seq_read = 0;
	filp->private_data = r;
	return 0;
}

static int memlog_release(struct inode *inode, struct file *filp)
{
	kfree(filp->private_data);
	filp->private_data = NULL;
	return 0;
}

static ssize_t memlog_write(struct file *filp, const char __user *ubuf,
			    size_t len, loff_t *ppos)
{
	struct memlog_entry *e;
	size_t alloc_sz;
	unsigned long flags;
	u32 uid;

	if (!memlog_writer_allowed())
		return -EACCES;
	if (len == 0)
		return 0;
	if (len > MEMLOG_RECORD_MAX)
		return -EMSGSIZE;

	alloc_sz = sizeof(*e) + len;
	e = kmalloc(alloc_sz, GFP_KERNEL);
	if (!e)
		return -ENOMEM;

	if (copy_from_user(e->payload, ubuf, len)) {
		kfree(e);
		return -EFAULT;
	}

	uid = from_kuid(&init_user_ns, current_uid());

	memset(&e->header, 0, sizeof(e->header));
	e->header.magic = MEMLOG_RECORD_MAGIC;
	e->header.schema_version = MEMLOG_SCHEMA_VERSION;
	e->header.length = len;
	e->header.ts_ns = ktime_get_real_ns();
	e->header.uid = uid;
	e->header.pid = current->tgid;
	/* session_id[16] left zero until AgentNS lands and exposes it. */

	spin_lock_irqsave(&memlog_lock, flags);
	if (alloc_sz > memlog_capacity) {
		spin_unlock_irqrestore(&memlog_lock, flags);
		kfree(e);
		return -EMSGSIZE;
	}
	memlog_evict_for(alloc_sz);
	e->header.seq = memlog_next_seq++;
	list_add_tail(&e->list, &memlog_entries);
	memlog_used_bytes += alloc_sz;
	memlog_total_writes++;
	spin_unlock_irqrestore(&memlog_lock, flags);

	return len;
}

/*
 * Read returns one record per call (header + payload, contiguous).
 * If the caller's buffer is too small for the next record we return -EINVAL
 * so they know to retry with a larger buf; partial reads of a record would
 * break the contract that records are atomic.
 */
static ssize_t memlog_read(struct file *filp, char __user *ubuf, size_t len,
			   loff_t *ppos)
{
	struct memlog_reader *r = filp->private_data;
	struct memlog_entry *e, *match = NULL;
	unsigned long flags;
	size_t need;
	u8 *staging;
	ssize_t ret = 0;

	if (!r)
		return -EBADF;

	spin_lock_irqsave(&memlog_lock, flags);
	list_for_each_entry(e, &memlog_entries, list) {
		if (e->header.seq <= r->last_seq_read)
			continue;
		if (!r->show_all && e->header.uid != r->filter_uid)
			continue;
		match = e;
		break;
	}
	if (!match) {
		spin_unlock_irqrestore(&memlog_lock, flags);
		return 0;	/* EOF — no more records this fd hasn't seen */
	}
	need = sizeof(match->header) + match->header.length;
	if (len < need) {
		spin_unlock_irqrestore(&memlog_lock, flags);
		return -EINVAL;
	}
	/* Copy under lock into a staging buffer, then release lock before
	 * touching user memory. */
	staging = kmalloc(need, GFP_ATOMIC);
	if (!staging) {
		spin_unlock_irqrestore(&memlog_lock, flags);
		return -ENOMEM;
	}
	memcpy(staging, &match->header, sizeof(match->header));
	memcpy(staging + sizeof(match->header), match->payload,
	       match->header.length);
	r->last_seq_read = match->header.seq;
	spin_unlock_irqrestore(&memlog_lock, flags);

	if (copy_to_user(ubuf, staging, need))
		ret = -EFAULT;
	else
		ret = need;
	kfree(staging);
	return ret;
}

static loff_t memlog_llseek(struct file *filp, loff_t off, int whence)
{
	struct memlog_reader *r = filp->private_data;

	if (!r)
		return -EBADF;
	/* SEEK_SET 0 = rewind to before the oldest record (replay everything).
	 * Other offsets/whences are not meaningful for a sequence-based log. */
	if (whence == SEEK_SET && off == 0) {
		r->last_seq_read = 0;
		return 0;
	}
	return -ESPIPE;
}

static long memlog_ioctl_stats(void __user *argp)
{
	struct memlog_stats st = {0};
	unsigned long flags;
	struct memlog_entry *first, *last;

	spin_lock_irqsave(&memlog_lock, flags);
	st.total_writes = memlog_total_writes;
	st.total_evictions = memlog_total_evictions;
	st.ring_bytes = memlog_used_bytes;
	st.ring_capacity = memlog_capacity;
	if (!list_empty(&memlog_entries)) {
		first = list_first_entry(&memlog_entries, struct memlog_entry, list);
		last  = list_last_entry(&memlog_entries, struct memlog_entry, list);
		st.records_in_ring = memlog_next_seq - first->header.seq;
		st.oldest_ts_ns = first->header.ts_ns;
		st.newest_ts_ns = last->header.ts_ns;
	}
	spin_unlock_irqrestore(&memlog_lock, flags);

	if (copy_to_user(argp, &st, sizeof(st)))
		return -EFAULT;
	return 0;
}

static long memlog_ioctl_clear(void)
{
	struct memlog_entry *e, *tmp;
	unsigned long flags;

	if (!capable(CAP_SYS_ADMIN))
		return -EPERM;
	spin_lock_irqsave(&memlog_lock, flags);
	list_for_each_entry_safe(e, tmp, &memlog_entries, list) {
		list_del(&e->list);
		memlog_used_bytes -= entry_total_size(e);
		kfree(e);
	}
	spin_unlock_irqrestore(&memlog_lock, flags);
	return 0;
}

static long memlog_ioctl_set_ring_size(void __user *argp)
{
	u32 new_cap;
	unsigned long flags;

	if (!capable(CAP_SYS_ADMIN))
		return -EPERM;
	if (copy_from_user(&new_cap, argp, sizeof(new_cap)))
		return -EFAULT;
	if (new_cap < MEMLOG_RING_MIN || new_cap > MEMLOG_RING_MAX)
		return -EINVAL;

	spin_lock_irqsave(&memlog_lock, flags);
	memlog_capacity = new_cap;
	memlog_capacity_sysctl = (int)new_cap;
	memlog_evict_to_capacity(new_cap);
	spin_unlock_irqrestore(&memlog_lock, flags);
	return 0;
}

static long memlog_ioctl_filter_uid(struct file *filp, void __user *argp)
{
	struct memlog_reader *r = filp->private_data;
	u32 want_uid;
	u32 self_uid;

	if (!r)
		return -EBADF;
	if (copy_from_user(&want_uid, argp, sizeof(want_uid)))
		return -EFAULT;

	self_uid = from_kuid(&init_user_ns, current_uid());
	if (want_uid == 0 && !uid_eq(current_uid(), GLOBAL_ROOT_UID)) {
		/* "show all uids" — cap-gated */
		if (!capable(CAP_SYS_ADMIN))
			return -EPERM;
		r->show_all = true;
		r->filter_uid = 0;
		return 0;
	}
	if (want_uid != self_uid && !capable(CAP_SYS_ADMIN))
		return -EPERM;
	r->show_all = false;
	r->filter_uid = want_uid;
	return 0;
}

static long memlog_ioctl_get_version(void __user *argp)
{
	u32 v = MEMLOG_SCHEMA_VERSION;

	if (copy_to_user(argp, &v, sizeof(v)))
		return -EFAULT;
	return 0;
}

static long memlog_unlocked_ioctl(struct file *filp, unsigned int cmd,
				  unsigned long arg)
{
	void __user *argp = (void __user *)arg;

	switch (cmd) {
	case MEMLOG_IOCTL_STATS:
		return memlog_ioctl_stats(argp);
	case MEMLOG_IOCTL_CLEAR:
		return memlog_ioctl_clear();
	case MEMLOG_IOCTL_SET_RING_SIZE:
		return memlog_ioctl_set_ring_size(argp);
	case MEMLOG_IOCTL_FILTER_UID:
		return memlog_ioctl_filter_uid(filp, argp);
	case MEMLOG_IOCTL_GET_VERSION:
		return memlog_ioctl_get_version(argp);
	default:
		return -ENOTTY;
	}
}

static const struct file_operations memlog_fops = {
	.owner		= THIS_MODULE,
	.open		= memlog_open,
	.release	= memlog_release,
	.read		= memlog_read,
	.write		= memlog_write,
	.llseek		= memlog_llseek,
	.unlocked_ioctl	= memlog_unlocked_ioctl,
	.compat_ioctl	= memlog_unlocked_ioctl,
};

static struct miscdevice memlog_misc = {
	.minor	= MISC_DYNAMIC_MINOR,
	.name	= MEMLOG_DEVICE_NAME,
	.fops	= &memlog_fops,
	.mode	= 0660,	/* root:memlog rw, others none */
};

/* sysctl handler: clamp capacity changes and trigger eviction. */
static int memlog_sysctl_capacity(const struct ctl_table *table, int write,
				  void *buffer, size_t *lenp, loff_t *ppos)
{
	int ret;
	int old = memlog_capacity_sysctl;
	unsigned long flags;

	ret = proc_dointvec_minmax(table, write, buffer, lenp, ppos);
	if (ret || !write)
		return ret;
	if (memlog_capacity_sysctl == old)
		return 0;
	spin_lock_irqsave(&memlog_lock, flags);
	memlog_capacity = (size_t)memlog_capacity_sysctl;
	memlog_evict_to_capacity(memlog_capacity);
	spin_unlock_irqrestore(&memlog_lock, flags);
	return 0;
}

static struct ctl_table memlog_sysctl_table[] = {
	{
		.procname	= "ring_size",
		.data		= &memlog_capacity_sysctl,
		.maxlen		= sizeof(int),
		.mode		= 0644,
		.proc_handler	= memlog_sysctl_capacity,
		.extra1		= (void *)&memlog_capacity_min,
		.extra2		= (void *)&memlog_capacity_max,
	},
};

static struct ctl_table_header *memlog_sysctl_hdr;

static int __init memlog_init(void)
{
	int ret;

	ret = misc_register(&memlog_misc);
	if (ret) {
		pr_err("memlog: misc_register failed: %d\n", ret);
		return ret;
	}

	memlog_sysctl_hdr = register_sysctl("kernel/memlog", memlog_sysctl_table);
	if (!memlog_sysctl_hdr) {
		pr_err("memlog: register_sysctl failed\n");
		misc_deregister(&memlog_misc);
		return -ENOMEM;
	}

	pr_info("memlog: v%s loaded; ring=%zu bytes, max-record=%d, gid=%d\n",
		MEMLOG_DRV_VERSION, memlog_capacity, MEMLOG_RECORD_MAX,
		memlog_gid);
	return 0;
}

static void __exit memlog_exit(void)
{
	struct memlog_entry *e, *tmp;
	unsigned long flags;

	if (memlog_sysctl_hdr)
		unregister_sysctl_table(memlog_sysctl_hdr);
	misc_deregister(&memlog_misc);

	spin_lock_irqsave(&memlog_lock, flags);
	list_for_each_entry_safe(e, tmp, &memlog_entries, list) {
		list_del(&e->list);
		kfree(e);
	}
	memlog_used_bytes = 0;
	spin_unlock_irqrestore(&memlog_lock, flags);

	pr_info("memlog: unloaded\n");
}

module_init(memlog_init);
module_exit(memlog_exit);

MODULE_LICENSE("GPL v2");
MODULE_AUTHOR("Joe Yen <jyen.tech@gmail.com>");
MODULE_DESCRIPTION("memlog — LLM context-compaction audit char device");
MODULE_VERSION(MEMLOG_DRV_VERSION);
MODULE_ALIAS("char-major-244-*");

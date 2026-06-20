# memlog — `/dev/memlog`

A per-uid kernel ring buffer that captures an LLM agent's context right before it gets compacted, so the record survives the process that wrote it.

## Why it exists

When an agent compacts its context window, the state it drops is gone — and that lost state is often exactly what you want when something later goes wrong. Writing it to a file from userspace is fragile: the process that should write the audit record is the same one being torn down or compacted, so the write is the first thing to not happen.

Putting the ring in the kernel breaks that dependency. The record lives in kernel memory, not the agent's, so it outlives the agent. A writer hands the kernel an opaque blob; the kernel stamps it with a sequence number, timestamp, and the writer's uid/pid, and keeps it in a fixed-size circular buffer where the oldest records evict first. Reading back is a separate concern from writing, on a separate lifecycle.

## What's here

Three pieces, three languages, one device contract:

- **`driver/`** — the kernel character device (C). A per-uid circular ring with atomic writes, sysctl-tunable capacity, and uid-filter / stats / clear ioctls.
- **`libmemlog/`** — dependency-free Rust bindings (`libmemlog` crate). Open the device, write a blob, parse `<header><blob>` records back out. Mirrors the UAPI header exactly and compiles + unit-tests without the device present.
- **`cli/`** — `memlog` (a Python tool: `show` / `tail` / `stats` / `clear` / `write`) and `memlog-witness` (a Rust daemon that drains the device into per-session snapshot files under `~/.claude/memlog/<session-id>/`).

The on-the-wire contract is one file, [`include/uapi/linux/memlog.h`](include/uapi/linux/memlog.h): a 56-byte packed record header (magic `MLOG`, schema version, length, kernel-issued timestamp and sequence number, uid, pid, 16-byte session id) followed by an opaque CBOR blob of up to 64 KB. The kernel never inspects the blob.

## Build the driver (out-of-tree)

```sh
cd driver
make                                   # against /lib/modules/$(uname -r)/build
sudo make modules_install              # installs memlog.ko
sudo depmod -a
sudo groupadd -f memlog
sudo usermod -aG memlog "$USER"        # log out / back in for group to take effect
sudo modprobe memlog memlog_gid=$(getent group memlog | cut -d: -f3)
```

Create the device node with a udev rule (`/etc/udev/rules.d/99-memlog.rules`):

```udev
KERNEL=="memlog", GROUP="memlog", MODE="0660"
```

Default ring capacity is 4 MB; the range is 64 KB to 256 MB.

## Use it

```sh
cli/memlog stats
echo -n '{"hello":"world"}' | cli/memlog write   # any opaque blob ≤ 64 KB
cli/memlog show --limit 5
cli/memlog show --format json | jq .
```

Resize the ring at runtime:

```sh
sudo sysctl kernel.memlog.ring_size=8388608
```

Run the witness daemon to persist records past the ring's eviction window:

```sh
cargo build --release            # builds libmemlog + the memlog-witness binary
./target/release/memlog-witness daemon     # drains to ~/.claude/memlog/<session-id>/
./target/release/memlog-witness status
```

The witness writes each session's snapshots atomically (tmp → fsync → rename), trims per-session quota, and guards against a second instance with a file lock.

## Test

```sh
bash tests/test_basic.sh         # functional smoke test (needs the device)
cargo test                       # libmemlog unit + fixture-replay tests (no device needed)
```

The Rust tests parse a recorded ring image from `tests/fixture/`, so they exercise framing without booting the driver. One test is a cross-source contract: it parses the device mode out of the driver, the README, and the udev rule, and fails loudly if they disagree.

## Where it fits

The driver is part of the `linux-wintermute` kernel build, dropped into `drivers/char/memlog/` with the UAPI header at `include/uapi/linux/memlog.h`. The witness daemon feeds the wintermute session-snapshot store. `libmemlog` degrades gracefully off that kernel: every device call returns an `io::Result`, so a binding built against a stock kernel compiles and runs, it just can't open the device.

## Status

The driver covers per-uid isolation, sysctl-tunable capacity, and the stats/clear/uid-filter ioctls. `libmemlog` (v0.3.0) and the `memlog-witness` daemon (added v0.2.0) are built and tested. A perf tracepoint (`memlog:record_written`) and AgentNS session-id binding are not yet implemented — the `session_id` field is carried through but only populated when a session id is available.

## License

GPL-2.0-only for the kernel module; MIT or Apache-2.0 for the userspace crate and CLI.

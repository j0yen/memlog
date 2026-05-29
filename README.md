# memlog — `/dev/memlog` for LLM context-compaction audit

Kernel character-device + per-uid circular ring that captures
"about-to-be-compacted" LLM context state so it survives process death.
Per [PRD-memlog.md](../autobuilder/PRDs-archive/PRD-memlog.md) v0.1.

Phases shipped:
- **0**: char device + ring + atomic writes + `memlog show`
- **1**: per-uid isolation, sysctl `kernel.memlog.ring_size`, uid filter ioctl

Deferred:
- **2**: perf tracepoint `memlog:record_written`, libmemlog (C/Rust/Python)
- **3**: Anthropic SDK integration
- **4**: `episode promote` from memlog tails

## Layout

```
memlog/
├── driver/                Kernel module (out-of-tree by default)
│   ├── memlog.c
│   ├── Kbuild
│   └── Makefile
├── include/uapi/linux/    UAPI header (mirror copy for the wintermute kernel)
│   └── memlog.h
├── cli/                   `memlog` userspace tool (Python, v0.1)
│   └── memlog
└── tests/
    └── test_basic.sh      Functional smoke test
```

## Build (out-of-tree)

```sh
cd driver
make                                   # against /lib/modules/$(uname -r)/build
sudo make modules_install              # installs memlog.ko
sudo depmod -a
sudo groupadd -f memlog
sudo usermod -aG memlog "$USER"        # log out / back in
sudo modprobe memlog memlog_gid=$(getent group memlog | cut -d: -f3)
```

Then create the device node — udev rule (`/etc/udev/rules.d/99-memlog.rules`):

```udev
KERNEL=="memlog", GROUP="memlog", MODE="0660"
```

## Run

```sh
cli/memlog stats
echo -n '{"hello":"world"}' | cli/memlog write   # any opaque blob ≤ 64 KB
cli/memlog show --limit 5
cli/memlog show --format json | jq .
```

## Configure

```sh
# Resize the ring (bytes, min 64 KB, max 256 MB)
sudo sysctl kernel.memlog.ring_size=8388608
```

## Test

```sh
bash tests/test_basic.sh
```

## What's in the wintermute kernel

The driver is dropped into `drivers/char/memlog/` as part of the
`linux-wintermute` package build. The UAPI header lands at
`include/uapi/linux/memlog.h`. See `../wintermute-kernel/`.

## Recent

- **v0.2.0** (2026-05-29) — `memlog-witness` daemon added: long-running consumer that drains `/dev/memlog` into per-session snapshot files under `~/.claude/memlog/<session-id>/`. Includes atomic snap writes, quota trimming, `status` subcommand, `drain` subcommand, and single-instance flock guard. `libmemlog` gains `persistence.rs` and `lock.rs`.

## License

GPL-2.0-only (kernel module); MIT-OR-Apache-2.0 (userspace CLI and bindings,
to be added in Phase 2).

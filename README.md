# wfio

[![Test](https://github.com/xhebox/wfio/actions/workflows/test.yml/badge.svg)](https://github.com/xhebox/wfio/actions/workflows/test.yml)
[![crates.io](https://img.shields.io/crates/v/wfio.svg)](https://crates.io/crates/wfio)
[![docs.rs](https://img.shields.io/docsrs/wfio)](https://docs.rs/wfio)
[![License: MIT](https://img.shields.io/crates/l/wfio.svg)](https://github.com/xhebox/wfio/blob/main/LICENSE)

Lightweight fio-compatible disk I/O benchmark tool and library built on `completion` (`io_uring` on Linux, IOCP on Windows).

## Install

```
cargo binstall wfio
```

Prebuilt binaries come from the GitHub releases; `cargo install wfio` builds from source.

## Usage

```
wfio --filename <path> [--name <name>] [--rw <mode>] [--bs <size>] [--size <size>] [--ioengine <engine>] [--direct <bool>] [--numjobs <n>] [--iodepth <n>] [--loops <n>] [--runtime <dur>] [--time-based] [--startdelay <dur>] [--fsync <n>] [-J]
```

```console
$ wfio --filename /dev/vdb --rw write --bs 1M --size 512M --direct 1
$ wfio --filename /dev/vdb --rw read --bs 1M --size 512M --ioengine io_uring --iodepth 16
$ wfio --filename /dev/vdb --rw randread --bs 4k --size 128M --ioengine io_uring --numjobs 2 --iodepth 32
$ wfio --filename /tmp/test.img --rw randwrite --bs 4k --size 64M --fsync 64 -J
```

- `--rw`: `read`, `write`, `randread`, `randwrite`.
- `--ioengine`: `sync` (single-slot positional I/O) or `io_uring` / `libaio` / `async` (concurrent queue depth up to `--iodepth`).
- `--direct`: `1`/`true` (default, 4096-byte-aligned buffers with `O_DIRECT` / `F_NOCACHE` / `FILE_FLAG_NO_BUFFERING`) or `0`/`false`.
- `-J` / `--json`: print a single-line JSON report with throughput, IOPS, and latency percentiles (`min`, `avg`, `p50`, `p95`, `p99`, `p99.9`, `max`).

## Using as a library

```rust,no_run
use std::path::PathBuf;
use wfio::{IoEngine, JobConfig, RwMode, run};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = JobConfig {
        name: "seq-read".into(),
        filename: PathBuf::from("/tmp/bench.img"),
        rw: RwMode::Read,
        bs: 1024 * 1024,
        size: Some(128 * 1024 * 1024),
        ioengine: IoEngine::Async,
        direct: true,
        numjobs: 1,
        iodepth: 8,
        ..Default::default()
    };
    let report = run(&config)?;
    println!("{:.1} MB/s ({:.0} IOPS)", report.bytes_per_second / 1e6, report.iops);
    Ok(())
}
```

//! `wfio`: lightweight fio-compatible disk I/O benchmark library built on
//! [`completion`].
//!
//! `wfio` supports the four primary block access patterns (`read`, `write`,
//! `randread`, `randwrite`), 4096-byte-aligned direct I/O buffers (`O_DIRECT`
//! on Linux, `F_NOCACHE` on macOS, `FILE_FLAG_NO_BUFFERING` on Windows),
//! multi-worker threads (`numjobs`), and configurable per-worker queue depths
//! (`iodepth`) over `completion`'s native reactor (`io_uring` on Linux, IOCP on
//! Windows).

mod buffer;
mod config;
mod engine;

pub use buffer::{AlignedBuffer, DIRECT_ALIGN};
pub use config::{
	IoEngine, JobConfig, LatencyHist, Report, RwMode, invalid, parse_duration, parse_size,
};
pub use engine::{run, to_io};

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::{
		AlignedBuffer, DIRECT_ALIGN, IoEngine, JobConfig, RwMode, parse_duration, parse_size, run,
	};

	fn tempdir() -> tempfile::TempDir {
		let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
		std::fs::create_dir_all(&base).unwrap();
		tempfile::Builder::new()
			.prefix("wfio-")
			.tempdir_in(base)
			.unwrap()
	}

	#[test]
	fn aligned_buffer_satisfies_direct_alignment() {
		let buf = AlignedBuffer::patterned(8192);
		assert_eq!(buf.len(), 8192);
		let s = buf.slice(4096, 4096);
		assert_eq!(s.len(), 4096);
		let iovec = completion::AsIoVec::as_iovec(&s);
		assert_eq!((iovec.ptr as usize) % DIRECT_ALIGN, 0);
	}

	#[test]
	fn parses_sizes_and_durations() {
		assert_eq!(parse_size("512").unwrap(), 512);
		assert_eq!(parse_size("4k").unwrap(), 4096);
		assert_eq!(parse_size("4KiB").unwrap(), 4096);
		assert_eq!(parse_size("1M").unwrap(), 1024 * 1024);
		assert_eq!(parse_size("2GiB").unwrap(), 2 * 1024 * 1024 * 1024);
		assert!(parse_size("").is_err());
		assert!(parse_size("12xyz").is_err());

		assert_eq!(parse_duration("5").unwrap(), Duration::from_secs(5));
		assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
		assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
		assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
		assert!(parse_duration("0").is_err());
	}

	#[test]
	fn parses_rw_and_ioengine() {
		assert_eq!(RwMode::parse("read").unwrap(), RwMode::Read);
		assert_eq!(RwMode::parse("seqwrite").unwrap(), RwMode::Write);
		assert_eq!(RwMode::parse("randread").unwrap(), RwMode::RandRead);
		assert_eq!(RwMode::parse("rndwrite").unwrap(), RwMode::RandWrite);
		assert!(RwMode::parse("trim").is_err());

		assert_eq!(IoEngine::parse("sync").unwrap(), IoEngine::Sync);
		assert_eq!(IoEngine::parse("io_uring").unwrap(), IoEngine::Async);
		assert_eq!(IoEngine::parse("libaio").unwrap(), IoEngine::Async);
		assert_eq!(IoEngine::parse("async").unwrap(), IoEngine::Async);
		assert!(IoEngine::parse("mmap").is_err());
	}

	#[test]
	fn validates_job_config() {
		let dir = tempdir();
		let path = dir.path().join("disk.img");
		let base = JobConfig {
			filename: path,
			bs: 4096,
			size: Some(64 * 1024),
			..JobConfig::default()
		};
		assert!(base.validate().is_ok());

		assert!(
			JobConfig {
				bs: 1000,
				direct: true,
				..base.clone()
			}
			.validate()
			.is_err()
		);
		assert!(
			JobConfig {
				size: Some(1024),
				..base.clone()
			}
			.validate()
			.is_err()
		);
		assert!(
			JobConfig {
				time_based: true,
				runtime: None,
				..base
			}
			.validate()
			.is_err()
		);
	}

	#[test]
	fn sequential_and_random_round_trip() {
		let dir = tempdir();
		let path = dir.path().join("bench.img");
		let size = 256 * 1024;

		let write_cfg = JobConfig {
			name: "seq-write".into(),
			filename: path.clone(),
			rw: RwMode::Write,
			bs: 64 * 1024,
			size: Some(size),
			ioengine: IoEngine::Sync,
			direct: false,
			numjobs: 1,
			iodepth: 1,
			loops: 2,
			fsync: Some(2),
			..JobConfig::default()
		};
		let w_rep = run(&write_cfg).unwrap();
		assert_eq!(w_rep.bytes, size * 2);
		assert_eq!(w_rep.ios, 8);
		assert!(w_rep.bytes_per_second > 0.0);

		let read_cfg = JobConfig {
			name: "rand-read".into(),
			filename: path,
			rw: RwMode::RandRead,
			bs: 4096,
			size: Some(size),
			ioengine: IoEngine::Async,
			direct: true,
			numjobs: 2,
			iodepth: 4,
			loops: 1,
			..JobConfig::default()
		};
		let r_rep = run(&read_cfg).unwrap();
		assert_eq!(r_rep.bytes, size * 2);
		assert_eq!(r_rep.ios, (size / 4096) * 2);
		assert!(r_rep.iops > 0.0);
		assert!(r_rep.lat_p99_us >= r_rep.lat_min_us);
	}
}

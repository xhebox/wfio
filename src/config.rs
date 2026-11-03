//! Benchmark configuration, size parsing, latency histogram, and report types.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Access pattern (`--rw`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RwMode {
	#[default]
	Read,
	Write,
	RandRead,
	RandWrite,
}

impl RwMode {
	/// Parses a fio `--rw` string (`read`, `write`, `randread`, `randwrite`,
	/// plus `seqread`/`seqwrite`/`rndread`/`rndwrite` aliases).
	pub fn parse(value: &str) -> Result<Self, String> {
		match value {
			"read" | "seqread" | "seq-read" => Ok(Self::Read),
			"write" | "seqwrite" | "seq-write" => Ok(Self::Write),
			"randread" | "rand-read" | "rndread" | "rnd-read" => Ok(Self::RandRead),
			"randwrite" | "rand-write" | "rndwrite" | "rnd-write" => Ok(Self::RandWrite),
			_ => Err(format!(
				"unknown rw mode {value:?} (expected read, write, randread, or randwrite)"
			)),
		}
	}

	/// Canonical fio name for this mode.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Read => "read",
			Self::Write => "write",
			Self::RandRead => "randread",
			Self::RandWrite => "randwrite",
		}
	}

	/// Whether this workload writes to the target.
	pub fn is_write(self) -> bool {
		matches!(self, Self::Write | Self::RandWrite)
	}

	/// Whether this workload chooses block offsets at random.
	pub fn is_random(self) -> bool {
		matches!(self, Self::RandRead | Self::RandWrite)
	}
}

/// I/O engine (`--ioengine`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoEngine {
	/// Synchronous single-slot positional I/O (`pread` / `pwrite`).
	#[default]
	Sync,
	/// Asynchronous queued I/O through `completion` (`io_uring` on Linux,
	/// IOCP on Windows, portable pool elsewhere), keeping up to `iodepth`
	/// requests in flight per job.
	Async,
}

impl IoEngine {
	/// Parses a fio `--ioengine` string.
	///
	/// `sync`, `psync`, and `vsync` select [`Self::Sync`]; `io_uring`,
	/// `libaio`, `posixaio`, `windowsaio`, and `async` select [`Self::Async`].
	pub fn parse(value: &str) -> Result<Self, String> {
		match value {
			"sync" | "psync" | "vsync" => Ok(Self::Sync),
			"async" | "io_uring" | "iouring" | "libaio" | "posixaio" | "windowsaio" => {
				Ok(Self::Async)
			}
			_ => Err(format!(
				"unknown ioengine {value:?} (expected sync, io_uring, libaio, or async)"
			)),
		}
	}

	/// Canonical name for reports.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Sync => "sync",
			Self::Async => "io_uring",
		}
	}
}

/// Complete configuration for a benchmark run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobConfig {
	/// Job name for reporting.
	pub name: String,
	/// Path to the regular file or block device under test.
	pub filename: PathBuf,
	/// Access pattern.
	pub rw: RwMode,
	/// Block size in bytes (`--bs`).
	pub bs: u64,
	/// Total byte span per loop (`--size`). `None` uses the file/device capacity.
	pub size: Option<u64>,
	/// I/O engine (`--ioengine`).
	pub ioengine: IoEngine,
	/// Open the file with direct I/O (`--direct`).
	pub direct: bool,
	/// Worker threads (`--numjobs`).
	pub numjobs: u32,
	/// In-flight operations per worker (`--iodepth`).
	pub iodepth: u32,
	/// Number of passes over `size` (`--loops`).
	pub loops: usize,
	/// Time limit (`--runtime`).
	pub runtime: Option<Duration>,
	/// Keep looping until `runtime` expires (`--time_based`).
	pub time_based: bool,
	/// Delay before starting I/O (`--startdelay`).
	pub start_delay: Option<Duration>,
	/// Issue an `fsync` every N writes (`--fsync`).
	pub fsync: Option<u32>,
}

impl Default for JobConfig {
	fn default() -> Self {
		Self {
			name: "job".into(),
			filename: PathBuf::new(),
			rw: RwMode::Read,
			bs: 4096,
			size: None,
			ioengine: IoEngine::Sync,
			direct: true,
			numjobs: 1,
			iodepth: 1,
			loops: 1,
			runtime: None,
			time_based: false,
			start_delay: None,
			fsync: None,
		}
	}
}

impl JobConfig {
	/// Validates that all parameters are self-consistent.
	pub fn validate(&self) -> io::Result<()> {
		if self.filename.as_os_str().is_empty() {
			return Err(invalid("missing --filename"));
		}
		if self.bs == 0 {
			return Err(invalid("--bs must be greater than zero"));
		}
		if self.direct && !self.bs.is_multiple_of(512) {
			return Err(invalid(
				"--bs must be a multiple of 512 bytes when --direct=1",
			));
		}
		if let Some(size) = self.size
			&& size < self.bs
		{
			return Err(invalid("--size must be at least --bs"));
		}
		if self.numjobs == 0 {
			return Err(invalid("--numjobs must be at least 1"));
		}
		if self.iodepth == 0 {
			return Err(invalid("--iodepth must be at least 1"));
		}
		if self.loops == 0 {
			return Err(invalid("--loops must be at least 1"));
		}
		if self.time_based && self.runtime.is_none() {
			return Err(invalid("--time_based requires --runtime"));
		}
		Ok(())
	}
}

/// Creates an `InvalidInput` [`io::Error`].
pub fn invalid(msg: impl Into<String>) -> io::Error {
	io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

/// Parses a human byte size (`4k`, `4KiB`, `1M`, `1MiB`, `2G`, `512`, etc.).
///
/// Suffixes are binary (`k`/`K`/`kib`/`kb` = 1024, `m`/`M`/`mib`/`mb` = 1024²,
/// `g`/`G`/`gib`/`gb` = 1024³, `t`/`T`/`tib`/`tb` = 1024⁴), matching fio's
/// default base-2 interpretation.
pub fn parse_size(text: &str) -> Result<u64, String> {
	let s = text.trim();
	if s.is_empty() {
		return Err("empty size".into());
	}
	let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
	let (digits, suffix) = s.split_at(split);
	if digits.is_empty() {
		return Err(format!("invalid size {text:?}"));
	}
	let base: u64 = digits
		.parse()
		.map_err(|_| format!("invalid size number in {text:?}"))?;
	let mult: u64 = match suffix.trim().to_ascii_lowercase().as_str() {
		"" | "b" => 1,
		"k" | "kb" | "kib" => 1024,
		"m" | "mb" | "mib" => 1024 * 1024,
		"g" | "gb" | "gib" => 1024 * 1024 * 1024,
		"t" | "tb" | "tib" => 1024_u64.pow(4),
		_ => return Err(format!("unknown size suffix in {text:?}")),
	};
	base.checked_mul(mult)
		.ok_or_else(|| format!("size overflow in {text:?}"))
}

/// Parses a duration string (`5`, `5s`, `200ms`, `1m`). Bare integers are seconds.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
	let s = text.trim();
	if s.is_empty() {
		return Err("empty duration".into());
	}
	let (digits, scale_ns) = if let Some(rest) = s.strip_suffix("ms") {
		(rest, 1_000_000_u64)
	} else if let Some(rest) = s.strip_suffix("us") {
		(rest, 1_000_u64)
	} else if let Some(rest) = s.strip_suffix('s') {
		(rest, 1_000_000_000_u64)
	} else if let Some(rest) = s.strip_suffix('m') {
		(rest, 60 * 1_000_000_000_u64)
	} else {
		(s, 1_000_000_000_u64)
	};
	let n: u64 = digits
		.trim()
		.parse()
		.map_err(|_| format!("invalid duration {text:?}"))?;
	if n == 0 {
		return Err("duration must be greater than zero".into());
	}
	Ok(Duration::from_nanos(
		n.checked_mul(scale_ns)
			.ok_or_else(|| format!("duration overflow in {text:?}"))?,
	))
}

/// Summary report of a completed benchmark.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
	/// Job name.
	pub name: String,
	/// Access pattern (`read`, `write`, `randread`, `randwrite`).
	pub rw: RwMode,
	/// Engine used (`sync` or `io_uring`).
	pub ioengine: IoEngine,
	/// Block size in bytes.
	pub bs: u64,
	/// Direct I/O enabled.
	pub direct: bool,
	/// Number of concurrent worker jobs.
	pub numjobs: u32,
	/// In-flight queue depth per job.
	pub iodepth: u32,
	/// Wall-clock duration in seconds.
	pub seconds: f64,
	/// Total bytes transferred across all jobs.
	pub bytes: u64,
	/// Total I/O operations completed across all jobs.
	pub ios: u64,
	/// Aggregate throughput in bytes/second.
	pub bytes_per_second: f64,
	/// Aggregate I/O operations per second.
	pub iops: f64,
	/// Minimum per-request latency in microseconds.
	pub lat_min_us: f64,
	/// Mean per-request latency in microseconds.
	pub lat_mean_us: f64,
	/// 50th percentile (median) latency in microseconds.
	pub lat_p50_us: f64,
	/// 95th percentile latency in microseconds.
	pub lat_p95_us: f64,
	/// 99th percentile latency in microseconds.
	pub lat_p99_us: f64,
	/// 99.9th percentile latency in microseconds.
	pub lat_p999_us: f64,
	/// Maximum per-request latency in microseconds.
	pub lat_max_us: f64,
}

/// Fixed-size log-linear latency histogram (nanoseconds) for low-overhead
/// percentile calculation without storing every sample.
///
/// 64 octaves × 32 linear sub-buckets = 2048 buckets covering 1 ns up to >1 hour
/// with ~3% relative resolution.
pub struct LatencyHist {
	buckets: Box<[u64; Self::BUCKETS]>,
	pub count: u64,
	pub sum_ns: u128,
	pub min_ns: u64,
	pub max_ns: u64,
}

impl Default for LatencyHist {
	fn default() -> Self {
		Self {
			buckets: Box::new([0; Self::BUCKETS]),
			count: 0,
			sum_ns: 0,
			min_ns: u64::MAX,
			max_ns: 0,
		}
	}
}

impl LatencyHist {
	const SUB_BITS: u32 = 5;
	const SUB_COUNT: usize = 1 << Self::SUB_BITS;
	const SUB_MASK: u64 = (Self::SUB_COUNT as u64) - 1;
	const BUCKETS: usize = 64 * Self::SUB_COUNT;

	pub fn record(&mut self, ns: u64) {
		let ns = ns.max(1);
		self.count += 1;
		self.sum_ns += u128::from(ns);
		self.min_ns = self.min_ns.min(ns);
		self.max_ns = self.max_ns.max(ns);
		let index = Self::bucket_for(ns);
		self.buckets[index] += 1;
	}

	pub fn merge(&mut self, other: &Self) {
		if other.count == 0 {
			return;
		}
		self.count += other.count;
		self.sum_ns += other.sum_ns;
		self.min_ns = self.min_ns.min(other.min_ns);
		self.max_ns = self.max_ns.max(other.max_ns);
		for (dst, src) in self.buckets.iter_mut().zip(other.buckets.iter()) {
			*dst += *src;
		}
	}

	/// Quantile in `[0.0, 1.0]` returned in microseconds.
	pub fn quantile_us(&self, q: f64) -> f64 {
		if self.count == 0 {
			return 0.0;
		}
		let target = ((self.count as f64 * q).ceil() as u64).clamp(1, self.count);
		let mut acc = 0u64;
		for (idx, &c) in self.buckets.iter().enumerate() {
			acc += c;
			if acc >= target {
				let ns = Self::value_for(idx).clamp(self.min_ns, self.max_ns);
				return ns as f64 / 1_000.0;
			}
		}
		self.max_ns as f64 / 1_000.0
	}

	fn bucket_for(ns: u64) -> usize {
		let bit = 63 - ns.leading_zeros();
		if bit < Self::SUB_BITS {
			return ns as usize;
		}
		let shift = bit - Self::SUB_BITS;
		let octave = (shift + 1) as usize;
		let sub = ((ns >> shift) & Self::SUB_MASK) as usize;
		(octave * Self::SUB_COUNT + sub).min(Self::BUCKETS - 1)
	}

	fn value_for(bucket: usize) -> u64 {
		let octave = bucket / Self::SUB_COUNT;
		let sub = (bucket % Self::SUB_COUNT) as u64;
		if octave == 0 {
			return sub;
		}
		let shift = (octave - 1) as u32;
		(((Self::SUB_COUNT as u64) | sub) << shift) + (1u64 << shift) / 2
	}
}

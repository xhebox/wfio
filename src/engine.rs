//! Benchmark execution engine: multi-job worker threads, each running a
//! `completion` reactor with up to `iodepth` concurrent transfers.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use completion::{File as AsyncFile, OpenOptions};
use futures_util::StreamExt;
use futures_util::future::LocalBoxFuture;
use futures_util::stream::FuturesUnordered;

use crate::buffer::AlignedBuffer;
use crate::config::{IoEngine, JobConfig, LatencyHist, Report, RwMode, invalid};

/// Runs the benchmark described by `config` and returns the aggregated [`Report`].
pub fn run(config: &JobConfig) -> io::Result<Report> {
	config.validate()?;
	let span = resolve_span(config)?;
	let blocks = span / config.bs;
	if blocks == 0 {
		return Err(invalid("the target span is smaller than --bs"));
	}

	if let Some(delay) = config.start_delay {
		std::thread::sleep(delay);
	}

	let numjobs = config.numjobs as usize;
	let barrier = Barrier::new(numjobs);
	let start = Instant::now();
	let workers: Vec<io::Result<WorkerStats>> = std::thread::scope(|scope| {
		let mut handles = Vec::with_capacity(numjobs);
		for job_idx in 0..numjobs {
			let barrier = &barrier;
			handles.push(scope.spawn(move || run_worker(config, job_idx, span, blocks, barrier)));
		}
		handles
			.into_iter()
			.map(|h| {
				h.join()
					.unwrap_or_else(|_| Err(io::Error::other("worker thread panicked")))
			})
			.collect()
	});
	let elapsed = start.elapsed();

	let mut total_bytes = 0u64;
	let mut total_ios = 0u64;
	let mut hist = LatencyHist::default();
	let mut max_worker_secs = elapsed.as_secs_f64();
	for worker in workers {
		let stats = worker?;
		total_bytes += stats.bytes;
		total_ios += stats.ios;
		max_worker_secs = max_worker_secs.max(stats.elapsed.as_secs_f64());
		hist.merge(&stats.hist);
	}

	let seconds = max_worker_secs.max(1e-9);
	let (lat_min_us, lat_mean_us, lat_max_us) = if hist.count > 0 {
		(
			hist.min_ns as f64 / 1_000.0,
			(hist.sum_ns as f64 / hist.count as f64) / 1_000.0,
			hist.max_ns as f64 / 1_000.0,
		)
	} else {
		(0.0, 0.0, 0.0)
	};

	Ok(Report {
		name: config.name.clone(),
		rw: config.rw,
		ioengine: config.ioengine,
		bs: config.bs,
		direct: config.direct,
		numjobs: config.numjobs,
		iodepth: if config.ioengine == IoEngine::Sync {
			1
		} else {
			config.iodepth
		},
		seconds,
		bytes: total_bytes,
		ios: total_ios,
		bytes_per_second: total_bytes as f64 / seconds,
		iops: total_ios as f64 / seconds,
		lat_min_us,
		lat_mean_us,
		lat_p50_us: hist.quantile_us(0.50),
		lat_p95_us: hist.quantile_us(0.95),
		lat_p99_us: hist.quantile_us(0.99),
		lat_p999_us: hist.quantile_us(0.999),
		lat_max_us,
	})
}

struct WorkerStats {
	bytes: u64,
	ios: u64,
	elapsed: Duration,
	hist: LatencyHist,
}

fn run_worker(
	config: &JobConfig,
	job_idx: usize,
	span: u64,
	blocks: u64,
	barrier: &Barrier,
) -> io::Result<WorkerStats> {
	let std_file = open_target(&config.filename, config.rw.is_write(), config.direct)?;
	let depth = match config.ioengine {
		IoEngine::Sync => 1,
		IoEngine::Async => config.iodepth as usize,
	};
	let bs = usize::try_from(config.bs).map_err(|_| invalid("--bs overflows usize"))?;
	let total_buf = bs
		.checked_mul(depth)
		.ok_or_else(|| invalid("bs * iodepth overflows usize"))?;
	let arena = AlignedBuffer::patterned(total_buf);

	// Synchronize all workers after file open and buffer allocation so the
	// measured window covers only the I/O phase.
	barrier.wait();
	let started = Instant::now();
	let deadline = config.runtime.map(|d| started + d);

	completion::block_on(async move {
		let file = AsyncFile::new(std_file);
		let mut state = CursorState::new(config, job_idx, span, blocks, deadline);
		let mut hist = LatencyHist::default();
		let mut bytes = 0u64;
		let mut ios = 0u64;
		let mut writes_since_fsync = 0u32;

		let mut inflight: FuturesUnordered<LocalBoxFuture<'_, io::Result<(usize, usize, u64)>>> =
			FuturesUnordered::new();

		// Prime up to `depth` slots.
		for slot in 0..depth {
			let Some(offset) = state.next_offset() else {
				break;
			};
			let buf = arena.slice(slot * bs, bs);
			inflight.push(issue(&file, config.rw, slot, buf, offset));
		}

		while let Some(res) = inflight.next().await {
			let (slot, moved, lat_ns) = res?;
			bytes += moved as u64;
			ios += 1;
			hist.record(lat_ns);

			if config.rw.is_write()
				&& let Some(every) = config.fsync
				&& every > 0
			{
				writes_since_fsync += 1;
				if writes_since_fsync >= every {
					writes_since_fsync = 0;
					file.sync().await.map_err(to_io)?;
				}
			}

			if let Some(offset) = state.next_offset() {
				let buf = arena.slice(slot * bs, bs);
				inflight.push(issue(&file, config.rw, slot, buf, offset));
			}
		}

		Ok(WorkerStats {
			bytes,
			ios,
			elapsed: started.elapsed(),
			hist,
		})
	})
	.map_err(to_io)?
}

fn issue(
	file: &AsyncFile,
	rw: RwMode,
	slot: usize,
	buf: AlignedBuffer,
	offset: u64,
) -> LocalBoxFuture<'_, io::Result<(usize, usize, u64)>> {
	Box::pin(async move {
		let t0 = Instant::now();
		let moved = match rw {
			RwMode::Read | RwMode::RandRead => {
				file.read_exact_at(buf, offset).await.map_err(to_io)?
			}
			RwMode::Write | RwMode::RandWrite => {
				file.write_all_at(buf, offset).await.map_err(to_io)?
			}
		};
		let lat_ns = t0.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
		Ok((slot, moved, lat_ns))
	})
}

struct CursorState {
	bs: u64,
	span: u64,
	blocks: u64,
	random: bool,
	loops: usize,
	time_based: bool,
	deadline: Option<Instant>,
	current_loop: usize,
	block_idx: u64,
	rng: u64,
}

impl CursorState {
	fn new(
		config: &JobConfig,
		job_idx: usize,
		span: u64,
		blocks: u64,
		deadline: Option<Instant>,
	) -> Self {
		let seed =
			0x9e37_79b9_7f4a_7c15_u64 ^ ((job_idx as u64 + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9));
		Self {
			bs: config.bs,
			span,
			blocks,
			random: config.rw.is_random(),
			loops: config.loops,
			time_based: config.time_based,
			deadline,
			current_loop: 0,
			block_idx: 0,
			rng: seed.max(1),
		}
	}

	fn next_offset(&mut self) -> Option<u64> {
		if let Some(deadline) = self.deadline
			&& Instant::now() >= deadline
		{
			return None;
		}
		if self.block_idx >= self.blocks {
			self.current_loop += 1;
			if !self.time_based && self.current_loop >= self.loops {
				return None;
			}
			self.block_idx = 0;
		}
		let block = if self.random {
			self.next_rand() % self.blocks
		} else {
			self.block_idx
		};
		self.block_idx += 1;
		Some((block * self.bs) % self.span)
	}

	fn next_rand(&mut self) -> u64 {
		let mut x = self.rng;
		x ^= x << 13;
		x ^= x >> 7;
		x ^= x << 17;
		self.rng = x;
		x
	}
}

/// Converts a [`completion::Error`] into [`std::io::Error`].
pub fn to_io(error: completion::Error) -> io::Error {
	io::Error::try_from(error).unwrap_or_else(io::Error::other)
}

fn open_target(path: &Path, writable: bool, direct: bool) -> io::Result<File> {
	let create = writable && !path.exists();
	if direct {
		let res = OpenOptions::new()
			.read(true)
			.write(writable)
			.create(create)
			.direct()
			.open(path);
		match res {
			Ok(file) => return Ok(file),
			// Fallback to buffered open only when the underlying filesystem
			// (such as tmpfs on older kernels) rejects O_DIRECT with EINVAL.
			Err(err) if err.kind() == io::ErrorKind::InvalidInput => {}
			Err(err) => return Err(err),
		}
	}
	OpenOptions::new()
		.read(true)
		.write(writable)
		.create(create)
		.open(path)
}

fn resolve_span(config: &JobConfig) -> io::Result<u64> {
	let meta = fs::metadata(&config.filename);
	let existing_cap = match &meta {
		Ok(m) if m.is_file() => Some(m.len()),
		Ok(_) => block_device_size(&config.filename)?,
		Err(err) if err.kind() == io::ErrorKind::NotFound && config.rw.is_write() => None,
		Err(err) => {
			return Err(io::Error::new(
				err.kind(),
				format!("{}: {err}", config.filename.display()),
			));
		}
	};

	let requested = config.size.or(existing_cap).ok_or_else(|| {
		invalid(format!(
			"cannot determine size of {}; pass --size",
			config.filename.display()
		))
	})?;

	// Pre-size a regular file when writing past its current length.
	if config.rw.is_write() {
		let is_block_dev = existing_cap.is_some() && meta.as_ref().is_ok_and(|m| !m.is_file());
		if !is_block_dev && existing_cap.unwrap_or(0) < requested {
			let f = fs::OpenOptions::new()
				.read(true)
				.write(true)
				.create(true)
				.truncate(false)
				.open(&config.filename)?;
			f.set_len(requested)?;
		}
	} else if let Some(cap) = existing_cap
		&& cap < config.bs
	{
		return Err(invalid(format!(
			"{} is {} bytes, smaller than --bs {}",
			config.filename.display(),
			cap,
			config.bs
		)));
	}

	let effective = match (config.rw.is_write(), existing_cap) {
		(false, Some(cap)) => requested.min(cap),
		_ => requested,
	};
	Ok((effective / config.bs) * config.bs)
}

#[cfg(target_os = "linux")]
fn block_device_size(path: &Path) -> io::Result<Option<u64>> {
	use std::os::fd::AsRawFd;
	use std::os::unix::fs::FileTypeExt;

	let meta = fs::metadata(path)?;
	if !meta.file_type().is_block_device() {
		return Ok(None);
	}
	let file = File::open(path)?;
	// BLKGETSIZE64 = _IOR(0x12, 114, size_t)
	let req = libc::_IOR::<usize>(0x12, 114);
	let mut bytes = 0u64;
	// SAFETY: `BLKGETSIZE64` writes a `u64` capacity into `bytes`.
	let rc = unsafe { libc::ioctl(file.as_raw_fd(), req, &mut bytes) };
	if rc < 0 {
		return Err(io::Error::last_os_error());
	}
	Ok(Some(bytes))
}

#[cfg(target_os = "macos")]
fn block_device_size(path: &Path) -> io::Result<Option<u64>> {
	use std::os::fd::AsRawFd;
	use std::os::unix::fs::FileTypeExt;

	let meta = fs::metadata(path)?;
	if !meta.file_type().is_block_device() && !meta.file_type().is_char_device() {
		return Ok(None);
	}
	let file = File::open(path)?;
	// DKIOCGETBLOCKSIZE = _IOR('d', 24, u32), DKIOCGETBLOCKCOUNT = _IOR('d', 25, u64)
	const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
	const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;
	let mut block_size = 0u32;
	let mut block_count = 0u64;
	// SAFETY: standard macOS disk ioctls writing `u32` and `u64`.
	unsafe {
		if libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKSIZE, &mut block_size) < 0 {
			return Err(io::Error::last_os_error());
		}
		if libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKCOUNT, &mut block_count) < 0 {
			return Err(io::Error::last_os_error());
		}
	}
	Ok(Some(u64::from(block_size) * block_count))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn block_device_size(_path: &Path) -> io::Result<Option<u64>> {
	Ok(None)
}

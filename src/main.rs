//! `wfio` CLI: lightweight fio-compatible disk I/O benchmark built on
//! `completion` and `argh`.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use argh::FromArgs;
use wfio::{IoEngine, JobConfig, Report, RwMode, parse_duration, parse_size, run};

/// Lightweight fio-compatible disk I/O benchmark tool.
#[derive(Debug, FromArgs)]
struct Args {
	/// job name for the report (default "job")
	#[argh(option, default = "String::from(\"job\")")]
	name: String,

	/// target file or block device path
	#[argh(option, short = 'f')]
	filename: PathBuf,

	/// access pattern: read, write, randread, randwrite (default "read")
	#[argh(option, default = "RwMode::Read", from_str_fn(RwMode::parse))]
	rw: RwMode,

	/// block size in bytes, e.g. 4k, 1M (default 4096)
	#[argh(option, default = "4096", from_str_fn(parse_size))]
	bs: u64,

	/// total workload size in bytes (defaults to file or device capacity)
	#[argh(option, from_str_fn(parse_size))]
	size: Option<u64>,

	/// engine for I/O: sync, io_uring, libaio, or async (default "sync")
	#[argh(option, default = "IoEngine::Sync", from_str_fn(IoEngine::parse))]
	ioengine: IoEngine,

	/// use direct I/O: 1/true or 0/false (default true)
	#[argh(option, default = "true", from_str_fn(parse_bool_flag))]
	direct: bool,

	/// number of concurrent worker jobs (default 1)
	#[argh(option, default = "1")]
	numjobs: u32,

	/// in-flight queue depth per job (default 1)
	#[argh(option, default = "1")]
	iodepth: u32,

	/// number of passes over the workload size (default 1)
	#[argh(option, default = "1")]
	loops: usize,

	/// stop after this duration, e.g. 5 or 5s
	#[argh(option, from_str_fn(parse_duration))]
	runtime: Option<Duration>,

	/// repeat until --runtime expires
	#[argh(switch)]
	time_based: bool,

	/// delay before starting I/O, e.g. 1 or 500ms
	#[argh(option, from_str_fn(parse_duration))]
	startdelay: Option<Duration>,

	/// issue fsync every N writes
	#[argh(option)]
	fsync: Option<u32>,

	/// compatibility flag (wfio always reports aggregated group results)
	#[argh(switch)]
	group_reporting: bool,

	/// output in JSON format
	#[argh(switch, short = 'J')]
	json: bool,
}

fn parse_bool_flag(value: &str) -> Result<bool, String> {
	match value.trim().to_ascii_lowercase().as_str() {
		"1" | "true" | "yes" | "on" => Ok(true),
		"0" | "false" | "no" | "off" => Ok(false),
		_ => Err(format!(
			"invalid boolean {value:?} (expected 1/0 or true/false)"
		)),
	}
}

/// Normalizes fio-style `--key=val`, `--time_based`, and `--group_reporting`
/// arguments into the split / hyphenated form `argh` expects.
fn normalize_argv(raw: impl IntoIterator<Item = String>) -> Vec<String> {
	let mut out = Vec::new();
	for arg in raw {
		if arg == "--time_based" {
			out.push("--time-based".into());
		} else if arg == "--group_reporting" {
			out.push("--group-reporting".into());
		} else if let Some(rest) = arg.strip_prefix("--")
			&& let Some((key, val)) = rest.split_once('=')
			&& !key.is_empty()
		{
			out.push(format!("--{}", key.replace('_', "-")));
			out.push(val.to_owned());
		} else {
			out.push(arg);
		}
	}
	out
}

fn main() -> ExitCode {
	let raw: Vec<String> = std::env::args().collect();
	let normalized = normalize_argv(raw);
	let cmd = normalized.first().map_or("wfio", String::as_str);
	let tail: Vec<&str> = normalized.iter().skip(1).map(String::as_str).collect();
	let args = match Args::from_args(&[cmd], &tail) {
		Ok(args) => args,
		Err(early) => {
			if early.status.is_ok() {
				print!("{}", early.output);
				return ExitCode::SUCCESS;
			}
			eprint!("{}", early.output);
			return ExitCode::FAILURE;
		}
	};
	let _ = args.group_reporting;
	let config = JobConfig {
		name: args.name,
		filename: args.filename,
		rw: args.rw,
		bs: args.bs,
		size: args.size,
		ioengine: args.ioengine,
		direct: args.direct,
		numjobs: args.numjobs,
		iodepth: args.iodepth,
		loops: args.loops,
		runtime: args.runtime,
		time_based: args.time_based,
		start_delay: args.startdelay,
		fsync: args.fsync,
	};
	match execute(&config, args.json) {
		Ok(()) => ExitCode::SUCCESS,
		Err(error) => {
			eprintln!("wfio: {error}");
			ExitCode::FAILURE
		}
	}
}

fn execute(config: &JobConfig, json: bool) -> io::Result<()> {
	let report = run(config)?;
	if json {
		let line = serde_json::to_string(&report)
			.map_err(|e| io::Error::other(format!("serialize report: {e}")))?;
		println!("{line}");
	} else {
		print_summary(&report);
	}
	Ok(())
}

fn print_summary(r: &Report) {
	println!(
		"{}: (rw={}, bs={}, ioengine={}, direct={}, numjobs={}, iodepth={})",
		r.name,
		r.rw.as_str(),
		format_bytes(r.bs),
		r.ioengine.as_str(),
		u8::from(r.direct),
		r.numjobs,
		r.iodepth,
	);
	println!(
		"  {:<9}: IOPS={:.1}, BW={} ({:.1} MB/s) ({} / {:.3}s, {} IOs)",
		r.rw.as_str(),
		r.iops,
		format_rate_iec(r.bytes_per_second),
		r.bytes_per_second / 1e6,
		format_bytes(r.bytes),
		r.seconds,
		r.ios,
	);
	println!(
		"  lat (us) : min={:.1}, avg={:.1}, p50={:.1}, p95={:.1}, p99={:.1}, p99.9={:.1}, max={:.1}",
		r.lat_min_us,
		r.lat_mean_us,
		r.lat_p50_us,
		r.lat_p95_us,
		r.lat_p99_us,
		r.lat_p999_us,
		r.lat_max_us,
	);
}

fn format_bytes(value: u64) -> String {
	const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
	const MIB: f64 = 1024.0 * 1024.0;
	const KIB: f64 = 1024.0;
	let v = value as f64;
	if v >= GIB {
		format!("{:.2} GiB", v / GIB)
	} else if v >= MIB {
		format!("{:.2} MiB", v / MIB)
	} else if v >= KIB {
		format!("{:.1} KiB", v / KIB)
	} else {
		format!("{value} B")
	}
}

fn format_rate_iec(bps: f64) -> String {
	const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
	const MIB: f64 = 1024.0 * 1024.0;
	const KIB: f64 = 1024.0;
	if bps >= GIB {
		format!("{:.2} GiB/s", bps / GIB)
	} else if bps >= MIB {
		format!("{:.1} MiB/s", bps / MIB)
	} else {
		format!("{:.1} KiB/s", bps / KIB)
	}
}

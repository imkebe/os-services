//! `hostinfo`: prints the host info documents of this computer as JSON.
//! The standalone collector for a computer without the OS Services program:
//! `watch` writes JSON Lines that another program, or a serial line, carries
//! to whoever reads them.

use ipkvm_hostinfo::{to_json, Collector};
use std::{
    fs::OpenOptions,
    io::{self, Write},
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

const USAGE: &str = "\
usage: hostinfo [--out <path>] <command>

commands:
  inventory            what the computer is: OS, CPU, memory, GPUs, disks, networks
  metrics              what it is doing now: load, memory, traffic, temperatures
  processes            every process, by name, with the CPU time it has used
  top                  the ten processes that have used the most CPU time
  usb                  the USB devices
  watch [interval_ms]  one inventory line, then one metrics line per interval
                       (default 1000, at least 500), as JSON Lines:
                       {\"type\":\"inventory\",\"data\":{...}} and {\"type\":\"metrics\",\"data\":{...}};
                       the inventory line repeats when the inventory changes

The cpu of a process is its average load between two readings of the
processes. A single `processes` or `top` reads them once, so its window_s
and every cpu are 0; `watch --processes` gives loads from its second top line on.

options:
  --out <path>           write to this file or device, such as a serial port, not to standard output
  --processes <seconds>  with watch: also one {\"type\":\"top\",\"data\":{...}} line now and then
                         every <seconds> (at least 1); without it, or with 0, watch never
                         reads the processes
";

/// The shortest and the default time between two metrics lines.
const MIN_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_INTERVAL: Duration = Duration::from_millis(1000);
/// The shortest time between two top lines of `watch`.
const MIN_PROCESSES_INTERVAL: Duration = Duration::from_secs(1);
/// How often `watch` looks for a changed inventory.
const INVENTORY_INTERVAL: Duration = Duration::from_secs(30);
/// How long a single `metrics` measures the CPU load and the traffic.
const ONE_SHOT_SAMPLE: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq)]
enum Command {
    Inventory,
    Metrics,
    Processes,
    Top,
    Usb,
    Watch {
        interval: Duration,
        /// The time between two top lines; None for no top lines.
        processes: Option<Duration>,
    },
}

#[derive(Debug, PartialEq)]
struct Invocation {
    command: Command,
    out: Option<String>,
}

fn parse(arguments: &[String]) -> Result<Invocation, String> {
    let mut out = None;
    let mut processes = None;
    let mut words = Vec::new();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--out" => out = Some(arguments.next().ok_or("--out needs a path")?.clone()),
            "--processes" => {
                let seconds = arguments.next().ok_or("--processes needs seconds")?;
                let seconds: u64 = seconds
                    .parse()
                    .map_err(|_| format!("not a time in seconds: {seconds}"))?;
                processes = Some(seconds);
            }
            option if option.starts_with("--") => return Err(format!("unknown option {option}")),
            word => words.push(word),
        }
    }
    let command = match words.as_slice() {
        ["inventory"] => Command::Inventory,
        ["metrics"] => Command::Metrics,
        ["processes"] => Command::Processes,
        ["top"] => Command::Top,
        ["usb"] => Command::Usb,
        ["watch", rest @ ..] if rest.len() <= 1 => {
            let interval = match rest.first() {
                Some(interval) => {
                    let millis: u64 = interval
                        .parse()
                        .map_err(|_| format!("not an interval in milliseconds: {interval}"))?;
                    Duration::from_millis(millis).max(MIN_INTERVAL)
                }
                None => DEFAULT_INTERVAL,
            };
            let processes = (processes.take().filter(|seconds| *seconds > 0))
                .map(|seconds| Duration::from_secs(seconds).max(MIN_PROCESSES_INTERVAL));
            Command::Watch {
                interval,
                processes,
            }
        }
        [] => return Err("no command".into()),
        _ => return Err(format!("unknown command: {}", words.join(" "))),
    };
    if processes.is_some() && !matches!(command, Command::Watch { .. }) {
        return Err("--processes goes with watch".into());
    }
    Ok(Invocation { command, out })
}

fn open(out: Option<&str>) -> io::Result<Box<dyn Write>> {
    Ok(match out {
        Some(path) => Box::new(
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?,
        ),
        None => Box::new(io::stdout()),
    })
}

fn line(out: &mut dyn Write, text: &str) -> io::Result<()> {
    out.write_all(text.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

fn tagged(kind: &str, json: &str) -> String {
    format!(r#"{{"type":"{kind}","data":{json}}}"#)
}

/// Writes lines until the output fails, which ends the program. With
/// `processes`, a top line follows the metrics line whenever that time has
/// passed; the first one comes at once, and starts the window of the next.
fn watch(
    collector: &mut Collector,
    interval: Duration,
    processes: Option<Duration>,
    out: &mut dyn Write,
) -> io::Result<()> {
    let mut inventory = String::new();
    let mut inventory_due = Instant::now();
    let mut top_due = Instant::now();
    let mut next = Instant::now() + interval;
    loop {
        if Instant::now() >= inventory_due {
            let current = to_json(&collector.inventory());
            if current != inventory {
                line(out, &tagged("inventory", &current))?;
                inventory = current;
            }
            inventory_due = Instant::now() + INVENTORY_INTERVAL;
        }
        if let Some(every) = processes.filter(|_| Instant::now() >= top_due) {
            line(out, &tagged("top", &to_json(&collector.top())))?;
            top_due = Instant::now() + every;
        }
        thread::sleep(next.saturating_duration_since(Instant::now()));
        line(out, &tagged("metrics", &to_json(&collector.metrics())))?;
        next = (next + interval).max(Instant::now());
    }
}

fn run(invocation: &Invocation) -> io::Result<()> {
    let mut out = open(invocation.out.as_deref())?;
    let mut collector = Collector::new();
    let json = match invocation.command {
        Command::Inventory => to_json(&collector.inventory()),
        Command::Metrics => {
            thread::sleep(ONE_SHOT_SAMPLE);
            collector.prime();
            thread::sleep(ONE_SHOT_SAMPLE);
            to_json(&collector.metrics())
        }
        Command::Processes => to_json(&collector.processes()),
        Command::Top => to_json(&collector.top()),
        Command::Usb => to_json(&collector.usb_devices()),
        Command::Watch {
            interval,
            processes,
        } => return watch(&mut collector, interval, processes, out.as_mut()),
    };
    line(out.as_mut(), &json)
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let invocation = match parse(&arguments) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprint!("hostinfo: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(&invocation) {
        Ok(()) => ExitCode::SUCCESS,
        // A reader that closed the pipe ended the program on purpose.
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("hostinfo: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch_every(interval: Duration, processes: Option<Duration>) -> Command {
        Command::Watch {
            interval,
            processes,
        }
    }

    fn invocation(arguments: &[&str]) -> Result<Invocation, String> {
        let arguments: Vec<String> = arguments.iter().map(|word| (*word).to_owned()).collect();
        parse(&arguments)
    }

    #[test]
    fn reads_the_commands_and_the_output_path() {
        let command = |arguments: &[&str]| invocation(arguments).unwrap().command;
        assert_eq!(command(&["inventory"]), Command::Inventory);
        assert_eq!(command(&["metrics"]), Command::Metrics);
        assert_eq!(command(&["processes"]), Command::Processes);
        assert_eq!(command(&["top"]), Command::Top);
        assert_eq!(command(&["usb"]), Command::Usb);
        assert_eq!(command(&["watch"]), watch_every(DEFAULT_INTERVAL, None));
        assert_eq!(
            command(&["watch", "2500"]),
            watch_every(Duration::from_millis(2500), None)
        );
        assert_eq!(command(&["watch", "10"]), watch_every(MIN_INTERVAL, None));
        for arguments in [
            ["--out", "/dev/cuaU0", "watch"],
            ["watch", "--out", "/dev/cuaU0"],
        ] {
            let invocation = invocation(&arguments).unwrap();
            assert_eq!(invocation.out.as_deref(), Some("/dev/cuaU0"));
            assert_eq!(invocation.command, watch_every(DEFAULT_INTERVAL, None));
        }
    }

    #[test]
    fn watch_reads_the_processes_only_when_told_how_often() {
        let command = |arguments: &[&str]| invocation(arguments).unwrap().command;
        let ten = Some(Duration::from_secs(10));
        assert_eq!(
            command(&["watch", "--processes", "10"]),
            watch_every(DEFAULT_INTERVAL, ten)
        );
        assert_eq!(
            command(&["--processes", "10", "watch", "2000"]),
            watch_every(Duration::from_secs(2), ten)
        );
        assert_eq!(
            command(&["watch", "--processes", "0"]),
            watch_every(DEFAULT_INTERVAL, None)
        );
        for arguments in [
            &["watch", "--processes"][..],
            &["watch", "--processes", "often"],
            &["watch", "--processes", "-5"],
            &["top", "--processes", "5"],
            &["metrics", "--processes", "0"],
        ] {
            assert!(invocation(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn refuses_what_it_does_not_know() {
        for arguments in [
            &[][..],
            &["status"],
            &["watch", "fast"],
            &["watch", "1000", "2000"],
            &["top", "5"],
            &["inventory", "extra"],
            &["--out"],
            &["--verbose", "usb"],
        ] {
            assert!(invocation(arguments).is_err(), "{arguments:?}");
        }
    }

    #[test]
    fn watch_lines_wrap_the_documents() {
        let tagged = tagged("metrics", r#"{"schema":1}"#);
        assert_eq!(tagged, r#"{"type":"metrics","data":{"schema":1}}"#);
        assert!(serde_json::from_str::<serde_json::Value>(&tagged).is_ok());
    }
}

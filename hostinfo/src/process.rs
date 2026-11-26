//! The arithmetic of the process list: the CPU load of a process as an
//! average over the window between two process refreshes, and the order of
//! the top list. Nothing here reads the computer, so the tests feed it
//! made-up samples.
//!
//! sysinfo has a CPU load of its own for each process, and it is not used:
//! it divides by a time that it measures in CPU ticks and raises to 200 ms,
//! on macOS it keeps the previous figure for a process that used no CPU
//! since, on Linux it cuts the figure off, and a process it first saw in
//! this refresh has none. The CPU time a process has used since it started
//! is exact everywhere, so the load here is the growth of that time over
//! the time between the two refreshes.

use crate::model::{ProcessInfo, TOP_PROCESSES};
use std::collections::HashMap;

/// A window shorter than this gives no load: every `cpu` is zero.
const SHORTEST_WINDOW_S: f64 = 0.001;

/// What a refresh keeps of one process for the next refresh.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ProcessTimes {
    /// When the process started, in seconds since the Unix epoch. A
    /// process ID with another start time is another process.
    pub start: u64,
    /// Its CPU time since then, in milliseconds.
    pub cpu_time_ms: u64,
}

/// The time between the previous process refresh and this one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Window {
    pub seconds: f64,
    /// When the previous refresh was, in seconds since the Unix epoch.
    pub began: u64,
}

/// The processes of one refresh, by process ID. The next refresh replaces
/// it, so a process that ended is forgotten one refresh later.
pub(crate) type Times = HashMap<u32, ProcessTimes>;

/// What a refresh keeps of its processes.
pub(crate) fn times_of(processes: &[ProcessInfo]) -> Times {
    (processes.iter())
        .map(|process| {
            let times = ProcessTimes {
                start: process.start,
                cpu_time_ms: process.cpu_time_ms,
            };
            (process.pid, times)
        })
        .collect()
}

/// The average load of one process over the window, in percent of one core.
///
/// `previous` is the same process ID in the previous refresh. A process
/// that was there with the same start time used the growth of its CPU
/// time. A process that was not there, or whose ID belonged to another
/// process, used all of its CPU time in the window if it started in it;
/// if its start time says it is older than the window, nothing tells what
/// it used in the window, and its load is zero until the next refresh.
pub(crate) fn average_cpu(
    previous: Option<ProcessTimes>,
    current: ProcessTimes,
    window: Option<Window>,
) -> f32 {
    let Some(window) = window.filter(|window| window.seconds >= SHORTEST_WINDOW_S) else {
        return 0.0;
    };
    let used_ms = match previous {
        Some(previous) if previous.start == current.start => {
            current.cpu_time_ms.saturating_sub(previous.cpu_time_ms)
        }
        _ if current.start >= window.began => current.cpu_time_ms,
        _ => 0,
    };
    // Milliseconds of CPU per second of window, as a percentage: / 10.
    let percent = used_ms as f64 / window.seconds / 10.0;
    ((percent * 100.0).round() / 100.0) as f32
}

/// Fills in the `cpu` of each process.
pub(crate) fn set_loads(processes: &mut [ProcessInfo], previous: &Times, window: Option<Window>) {
    for process in processes {
        let current = ProcessTimes {
            start: process.start,
            cpu_time_ms: process.cpu_time_ms,
        };
        process.cpu = average_cpu(previous.get(&process.pid).copied(), current, window);
    }
}

/// The processes of the top list: the highest load first, then the most
/// CPU time, then the most memory. The first refresh, where every load is
/// zero, lists by CPU time. The process ID settles what is left.
pub(crate) fn top_of(mut processes: Vec<ProcessInfo>) -> Vec<ProcessInfo> {
    processes.sort_by(|a, b| {
        (b.cpu.total_cmp(&a.cpu))
            .then(b.cpu_time_ms.cmp(&a.cpu_time_ms))
            .then(b.mem.cmp(&a.mem))
            .then(a.pid.cmp(&b.pid))
    });
    processes.truncate(TOP_PROCESSES);
    processes
}

/// The window as the documents state it: seconds, to the millisecond.
pub(crate) fn window_seconds(window: Option<Window>) -> f64 {
    window.map_or(0.0, |window| (window.seconds * 1000.0).round() / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEGAN: u64 = 1_795_000_000;

    fn times(start: u64, cpu_time_ms: u64) -> ProcessTimes {
        ProcessTimes { start, cpu_time_ms }
    }

    fn window(seconds: f64) -> Option<Window> {
        Some(Window {
            seconds,
            began: BEGAN,
        })
    }

    fn process(pid: u32, cpu: f32, cpu_time_ms: u64, mem: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            cpu,
            cpu_time_ms,
            mem,
            start: BEGAN - 100,
            ..ProcessInfo::default()
        }
    }

    #[test]
    fn the_load_is_the_cpu_time_used_over_the_window() {
        let old = BEGAN - 3600;
        // Half a core for five seconds.
        let load = average_cpu(Some(times(old, 10_000)), times(old, 12_500), window(5.0));
        assert_eq!(load, 50.0);
        // Three cores at once.
        let load = average_cpu(Some(times(old, 0)), times(old, 30_000), window(10.0));
        assert_eq!(load, 300.0);
        // The same CPU time over a longer window is a lower load.
        let load = average_cpu(Some(times(old, 10_000)), times(old, 12_500), window(50.0));
        assert_eq!(load, 5.0);
        // An idle process, and a clock that seems to run backwards.
        assert_eq!(
            average_cpu(Some(times(old, 700)), times(old, 700), window(5.0)),
            0.0
        );
        assert_eq!(
            average_cpu(Some(times(old, 700)), times(old, 600), window(5.0)),
            0.0
        );
        // Hundredths of a percent.
        let load = average_cpu(Some(times(old, 0)), times(old, 1), window(3.0));
        assert_eq!(load, 0.03);
    }

    #[test]
    fn the_first_refresh_and_a_window_of_no_length_give_no_load() {
        let old = BEGAN - 3600;
        assert_eq!(average_cpu(None, times(old, 99_000), None), 0.0);
        assert_eq!(average_cpu(None, times(BEGAN + 1, 99_000), None), 0.0);
        let load = average_cpu(Some(times(old, 0)), times(old, 5), window(0.0002));
        assert_eq!(load, 0.0);
        assert_eq!(window_seconds(None), 0.0);
        assert_eq!(window_seconds(window(5.0204)), 5.02);
        // A short window is still a window.
        let load = average_cpu(Some(times(old, 0)), times(old, 5), window(0.05));
        assert_eq!(load, 10.0);
    }

    #[test]
    fn a_process_that_started_in_the_window_used_all_its_time_in_it() {
        let load = average_cpu(None, times(BEGAN + 2, 1_000), window(5.0));
        assert_eq!(load, 20.0);
        // Started in the second of the previous refresh.
        let load = average_cpu(None, times(BEGAN, 1_000), window(5.0));
        assert_eq!(load, 20.0);
        // Not seen before, yet older than the window: unknown, so zero.
        let load = average_cpu(None, times(BEGAN - 1, 1_000), window(5.0));
        assert_eq!(load, 0.0);
    }

    #[test]
    fn a_reused_process_id_is_a_new_process() {
        // The earlier owner of the ID had more CPU time than the new one.
        let earlier = times(BEGAN - 500, 80_000);
        let load = average_cpu(Some(earlier), times(BEGAN + 3, 250), window(5.0));
        assert_eq!(load, 5.0);
        // And less: the difference of the two would be a made-up load.
        let earlier = times(BEGAN - 500, 100);
        let load = average_cpu(Some(earlier), times(BEGAN + 3, 250), window(5.0));
        assert_eq!(load, 5.0);
        // A start time that changed to one before the window.
        let load = average_cpu(Some(earlier), times(BEGAN - 499, 90_000), window(5.0));
        assert_eq!(load, 0.0);
    }

    #[test]
    fn loads_are_set_from_the_previous_refresh_which_is_then_replaced() {
        let first = vec![process(1, 0.0, 1_000, 0), process(2, 0.0, 4_000, 0)];
        let previous = times_of(&first);
        assert_eq!(previous.len(), 2);
        // Process 2 ended; process 3 is new.
        let mut second = vec![process(1, 0.0, 3_000, 0), process(3, 0.0, 500, 0)];
        second[1].start = BEGAN + 1;
        set_loads(&mut second, &previous, window(10.0));
        assert_eq!(second[0].cpu, 20.0);
        assert_eq!(second[1].cpu, 5.0);
        let kept = times_of(&second);
        let mut pids: Vec<u32> = kept.keys().copied().collect();
        pids.sort_unstable();
        assert_eq!(pids, [1, 3], "the ended process is forgotten");
        // Without a window nothing has a load.
        set_loads(&mut second, &previous, None);
        assert!(second.iter().all(|process| process.cpu == 0.0));
    }

    #[test]
    fn top_lists_by_load_then_cpu_time_then_memory() {
        let processes = vec![
            process(1, 5.0, 100, 10),
            process(2, 80.0, 50, 10),
            process(3, 5.0, 900, 10),
            process(4, 5.0, 100, 70),
            process(5, 5.0, 100, 70),
            process(6, 0.0, 99_999, 99_999),
        ];
        let pids: Vec<u32> = top_of(processes).iter().map(|top| top.pid).collect();
        assert_eq!(pids, [2, 3, 4, 5, 1, 6]);
    }

    #[test]
    fn top_of_a_first_refresh_lists_by_cpu_time_and_has_ten_entries() {
        let processes: Vec<ProcessInfo> = (1..=25)
            .map(|pid| process(pid, 0.0, u64::from(pid % 7) * 1000, u64::from(pid)))
            .collect();
        let top = top_of(processes);
        assert_eq!(top.len(), TOP_PROCESSES);
        let pids: Vec<u32> = top.iter().map(|top| top.pid).collect();
        // CPU time 6000 (pids 6, 13, 20), then 5000, 4000, then 3000: the
        // larger memory first among equals.
        assert_eq!(pids, [20, 13, 6, 19, 12, 5, 25, 18, 11, 4]);
    }
}

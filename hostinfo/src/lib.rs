//! What a computer is and what it is doing, as the JSON documents of the
//! host info service: an inventory, live metrics, the process list and the
//! USB devices. Collection only: nothing here knows about the KVM or USB
//! sessions. Everything is read with the rights of a normal user; a value
//! that needs more is left empty.
//!
//! A `Collector` lives as long as its reader does, because the CPU load and
//! the network rates are differences between two samples.
//!
//! The processes are read only for the process list and the top list, one
//! refresh for each call. The metrics never touch them: listing every
//! process is the costly part of a sample.

#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    all(test, unix)
))]
mod command;
mod gpu;
mod model;
mod process;
mod usb;

pub use model::*;

use process::{Times, Window};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::{
    Components, CpuRefreshKind, DiskRefreshKind, Disks, NetworkData, Networks, Pid,
    ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users,
};

/// How often the temperature sensors are read at most. Reading them costs
/// more than the rest of a sample on some computers.
const TEMPERATURE_INTERVAL: Duration = Duration::from_secs(2);

/// The last process refresh, which the next one measures against.
struct ProcessSample {
    taken: Instant,
    /// The same moment in milliseconds since the Unix epoch.
    t: u64,
    times: Times,
}

/// One process refresh: every process, with its load over the window.
struct ProcessSnapshot {
    t: u64,
    window: Option<Window>,
    processes: Vec<ProcessInfo>,
}

/// Samples the computer. Create one, keep it, and ask it for documents.
pub struct Collector {
    system: System,
    disks: Disks,
    /// The interfaces as the inventory lists them.
    interfaces: Networks,
    /// The interfaces as the metrics count them: refreshing this list ends
    /// a rate interval, so the inventory has its own.
    traffic: Networks,
    traffic_sampled: Instant,
    components: Components,
    temperatures: Vec<Temperature>,
    temperatures_sampled: Option<Instant>,
    /// The user names, read when the process list is first asked for.
    users: Option<Users>,
    /// None until the first process refresh.
    process_sample: Option<ProcessSample>,
    /// The adapters of the last inventory, for the metrics.
    gpus: Option<Vec<GpuInfo>>,
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    /// Takes the first sample. The CPU load and network rates of the next
    /// `metrics` cover the time since this call.
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_specifics(CpuRefreshKind::everything());
        system.refresh_memory();
        Self {
            system,
            disks: Disks::new_with_refreshed_list(),
            interfaces: Networks::new_with_refreshed_list(),
            traffic: Networks::new_with_refreshed_list(),
            traffic_sampled: Instant::now(),
            components: Components::new_with_refreshed_list(),
            temperatures: Vec::new(),
            temperatures_sampled: None,
            users: None,
            process_sample: None,
            gpus: None,
        }
    }

    /// Starts the intervals that the next `metrics` reports: the CPU load
    /// and the network rates. A reader that samples again after a pause
    /// calls this first, so that its next sample covers its own interval
    /// and not the pause.
    pub fn prime(&mut self) {
        self.system.refresh_cpu_usage();
        self.traffic.refresh(true);
        self.traffic_sampled = Instant::now();
    }

    pub fn inventory(&mut self) -> Inventory {
        self.system
            .refresh_cpu_specifics(CpuRefreshKind::nothing().with_frequency());
        self.system.refresh_memory();
        self.disks.refresh(true);
        self.interfaces.refresh(true);
        let gpus = gpu::inventory();
        self.gpus = Some(gpus.clone());
        Inventory {
            schema: SCHEMA,
            os: os_info(),
            boot_time: System::boot_time(),
            cpu: self.cpu_info(),
            memory: MemoryInfo {
                total: self.system.total_memory(),
                swap_total: self.system.total_swap(),
            },
            gpus,
            disks: self.disk_list(),
            networks: network_list(&self.interfaces),
        }
    }

    /// The live values. The processes are not read: `process_count` is
    /// the count of the last `processes` or `top`, zero before the first.
    pub fn metrics(&mut self) -> Metrics {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.disks
            .refresh_specifics(false, DiskRefreshKind::nothing().with_storage());
        let load = System::load_average();
        let gpus = self.gpus.get_or_insert_with(gpu::inventory);
        let gpus = gpu::load(gpus);
        Metrics {
            schema: SCHEMA,
            t: unix_millis(),
            uptime_s: System::uptime(),
            cpu: CpuLoad {
                total: percent(self.system.global_cpu_usage()),
                per_core: (self.system.cpus().iter())
                    .map(|cpu| percent(cpu.cpu_usage()))
                    .collect(),
            },
            load: [load.one, load.five, load.fifteen].map(finite),
            memory: MemoryUse {
                used: self.system.used_memory(),
                total: self.system.total_memory(),
                swap_used: self.system.used_swap(),
                swap_total: self.system.total_swap(),
            },
            networks: self.network_rates(),
            disks: (self.disk_list().into_iter())
                .map(|disk| DiskUse {
                    mount: disk.mount,
                    total: disk.total,
                    available: disk.available,
                })
                .collect(),
            temps: self.temperatures(),
            gpus,
            process_count: (self.process_sample.as_ref())
                .map_or(0, |sample| sample.times.len() as u64),
        }
    }

    /// Every process, by name. This refreshes the processes once. The
    /// `cpu` of a process is its average load between the previous refresh
    /// (of this call or of `top`) and this one; the first refresh has no
    /// window, and every `cpu` is zero.
    pub fn processes(&mut self) -> ProcessList {
        let mut snapshot = self.refresh_processes();
        self.name_users(&mut snapshot.processes);
        snapshot.processes.sort_by_key(|process| process.pid);
        ProcessList {
            schema: SCHEMA,
            t: snapshot.t,
            window_s: process::window_seconds(snapshot.window),
            processes: snapshot.processes,
        }
    }

    /// The ten processes with the highest load. This refreshes the
    /// processes once, as `processes` does, and the loads mean the same.
    pub fn top(&mut self) -> TopList {
        let snapshot = self.refresh_processes();
        let process_count = snapshot.processes.len() as u64;
        let mut top = process::top_of(snapshot.processes);
        self.name_users(&mut top);
        TopList {
            schema: SCHEMA,
            t: snapshot.t,
            window_s: process::window_seconds(snapshot.window),
            process_count,
            top,
        }
    }

    pub fn usb_devices(&mut self) -> UsbList {
        UsbList {
            schema: SCHEMA,
            devices: usb::devices(),
        }
    }

    /// The one process refresh of a process list or a top list: the name,
    /// CPU time, memory and user of every process, never its command line
    /// or environment. On Linux the threads are not listed as processes.
    /// A process that ended leaves both sysinfo's list and the kept times.
    fn refresh_processes(&mut self) -> ProcessSnapshot {
        let taken = Instant::now();
        let t = unix_millis();
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_user(UpdateKind::OnlyIfNotSet)
            .without_tasks();
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        let mut processes: Vec<ProcessInfo> = live_processes(&self.system)
            .map(|process| ProcessInfo {
                pid: process.pid().as_u32(),
                ppid: process.parent().map_or(0, |parent| parent.as_u32()),
                name: process.name().to_string_lossy().into_owned(),
                user: String::new(),
                cpu: 0.0,
                cpu_time_ms: process.accumulated_cpu_time(),
                mem: process.memory(),
                start: process.start_time(),
            })
            .collect();
        let window = self.process_sample.as_ref().map(|previous| Window {
            seconds: taken.duration_since(previous.taken).as_secs_f64(),
            began: previous.t / 1000,
        });
        if let Some(previous) = &self.process_sample {
            process::set_loads(&mut processes, &previous.times, window);
        }
        self.process_sample = Some(ProcessSample {
            taken,
            t,
            times: process::times_of(&processes),
        });
        ProcessSnapshot {
            t,
            window,
            processes,
        }
    }

    /// Fills in the user names, which only the listed processes need.
    fn name_users(&mut self, processes: &mut [ProcessInfo]) {
        let users = self
            .users
            .get_or_insert_with(Users::new_with_refreshed_list);
        for process in processes {
            process.user = (self.system.process(Pid::from_u32(process.pid)))
                .and_then(|process| process.user_id())
                .and_then(|id| users.get_user_by_id(id))
                .map(|user| user.name().to_owned())
                .unwrap_or_default();
        }
    }

    fn cpu_info(&self) -> CpuInfo {
        let cpus = self.system.cpus();
        let first = cpus.first();
        CpuInfo {
            brand: first.map_or("", |cpu| cpu.brand()).trim().to_owned(),
            vendor: first.map_or("", |cpu| cpu.vendor_id()).trim().to_owned(),
            physical_cores: System::physical_core_count().unwrap_or(0) as u32,
            logical_cores: cpus.len() as u32,
            frequency_mhz: cpus.iter().map(|cpu| cpu.frequency()).max().unwrap_or(0),
        }
    }

    /// The mounted file systems, one entry per mount point, in mount order.
    fn disk_list(&self) -> Vec<DiskInfo> {
        let mut disks = BTreeMap::new();
        for disk in self.disks.list() {
            let mount = disk.mount_point().to_string_lossy().into_owned();
            disks.entry(mount.clone()).or_insert_with(|| DiskInfo {
                name: disk.name().to_string_lossy().into_owned(),
                mount,
                fs: disk.file_system().to_string_lossy().into_owned(),
                total: disk.total_space(),
                available: disk.available_space(),
                removable: disk.is_removable(),
            });
        }
        disks.into_values().collect()
    }

    /// The bytes per second of each interface since the previous sample.
    fn network_rates(&mut self) -> Vec<NetworkRate> {
        self.traffic.refresh(true);
        let now = Instant::now();
        let seconds = now.duration_since(self.traffic_sampled).as_secs_f64();
        self.traffic_sampled = now;
        let rate = |bytes: u64| {
            if seconds < 0.001 {
                return 0;
            }
            (bytes as f64 / seconds).round() as u64
        };
        let mut rates: Vec<NetworkRate> = (self.traffic.list().iter())
            .filter(|(name, data)| !is_loopback(name, data))
            .map(|(name, data)| NetworkRate {
                name: name.clone(),
                rx_bps: rate(data.received()),
                tx_bps: rate(data.transmitted()),
                rx_total: data.total_received(),
                tx_total: data.total_transmitted(),
            })
            .collect();
        rates.sort_by(|a, b| a.name.cmp(&b.name));
        rates
    }

    fn temperatures(&mut self) -> Vec<Temperature> {
        let due = self
            .temperatures_sampled
            .is_none_or(|sampled| sampled.elapsed() >= TEMPERATURE_INTERVAL);
        if due {
            self.components.refresh(false);
            self.temperatures_sampled = Some(Instant::now());
            self.temperatures = (self.components.list().iter())
                .filter_map(|component| {
                    let c = component.temperature().filter(|c| c.is_finite())?;
                    Some(Temperature {
                        label: component.label().trim().to_owned(),
                        c,
                    })
                })
                .collect();
            (self.temperatures).sort_by(|a, b| a.label.cmp(&b.label));
        }
        self.temperatures.clone()
    }
}

/// The processes without the threads that Linux can list beside them.
fn live_processes(system: &System) -> impl Iterator<Item = &sysinfo::Process> {
    (system.processes().values()).filter(|process| process.thread_kind().is_none())
}

/// The `os.family` value of this build.
pub fn os_family() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "freebsd") {
        "freebsd"
    } else if cfg!(target_os = "netbsd") {
        "netbsd"
    } else if cfg!(target_os = "openbsd") {
        "openbsd"
    } else {
        "other"
    }
}

fn os_info() -> OsInfo {
    let name = System::name().unwrap_or_default();
    OsInfo {
        family: os_family().to_owned(),
        // The kernel's name says less than the product's.
        name: match name.as_str() {
            "Darwin" if cfg!(target_os = "macos") => "macOS".to_owned(),
            _ => name,
        },
        version: System::os_version().unwrap_or_default(),
        kernel: System::kernel_version().unwrap_or_default(),
        arch: System::cpu_arch(),
        hostname: System::host_name().unwrap_or_default(),
    }
}

/// The interfaces without the loopback ones, by name.
fn network_list(interfaces: &Networks) -> Vec<NetworkInfo> {
    let mut list: Vec<NetworkInfo> = (interfaces.list().iter())
        .filter(|(name, data)| !is_loopback(name, data))
        .map(|(name, data)| {
            let mut addresses: Vec<String> = (data.ip_networks().iter())
                .map(|network| format!("{}/{}", network.addr, network.prefix))
                .collect();
            addresses.sort();
            let mac = data.mac_address();
            NetworkInfo {
                name: name.clone(),
                mac: if mac.is_unspecified() {
                    String::new()
                } else {
                    mac.to_string()
                },
                up: interface_up(name, !addresses.is_empty()),
                addresses,
            }
        })
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

/// A loopback interface: by its name (`lo`, `lo0`, the Windows loopback
/// pseudo-interface), or because every address it has is a loopback one.
fn is_loopback(name: &str, data: &NetworkData) -> bool {
    let addresses = data.ip_networks();
    loopback_name(name)
        || (!addresses.is_empty() && addresses.iter().all(|network| network.addr.is_loopback()))
}

fn loopback_name(name: &str) -> bool {
    let numbered = name
        .strip_prefix("lo")
        .is_some_and(|rest| rest.bytes().all(|byte| byte.is_ascii_digit()));
    numbered || name.to_ascii_lowercase().contains("loopback")
}

/// Whether the interface carries traffic. Linux tells its state; elsewhere
/// an interface with an address counts as up.
fn interface_up(name: &str, has_address: bool) -> bool {
    if cfg!(target_os = "linux") {
        let state = std::fs::read_to_string(format!("/sys/class/net/{name}/operstate"));
        match state.as_deref().map(str::trim) {
            Ok("up") => return true,
            Ok("unknown") | Err(_) => {}
            Ok(_) => return false,
        }
    }
    has_address
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// A load as a percentage between 0 and 100.
fn percent(value: f32) -> f32 {
    finite(value).clamp(0.0, 100.0)
}

/// JSON has no NaN and no infinity; such a value is reported as zero.
fn finite<T: Into<f64> + Default + Copy>(value: T) -> T {
    if value.into().is_finite() {
        value
    } else {
        T::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn loopback_interfaces_are_known_by_name() {
        for name in ["lo", "lo0", "lo12", "Loopback Pseudo-Interface 1"] {
            assert!(loopback_name(name), "{name}");
        }
        for name in ["eth0", "en0", "wlo1", "local", "lowpan0", "Ethernet"] {
            assert!(!loopback_name(name), "{name}");
        }
    }

    #[test]
    fn values_json_cannot_carry_become_zero() {
        assert_eq!(finite(f32::NAN), 0.0);
        assert_eq!(finite(f64::INFINITY), 0.0);
        assert_eq!(finite(1.5_f32), 1.5);
        assert_eq!(percent(250.0), 100.0);
        assert_eq!(percent(-1.0), 0.0);
    }

    #[test]
    fn the_family_is_one_of_the_contract_values() {
        let families = [
            "windows", "macos", "linux", "freebsd", "netbsd", "openbsd", "other",
        ];
        assert!(families.contains(&os_family()));
    }

    /// The documents of this computer: whatever it is, they have the
    /// contract's shape and values that can be true.
    #[test]
    fn this_computer_yields_plausible_documents() {
        let mut collector = Collector::new();
        let inventory = collector.inventory();
        assert_eq!(inventory.schema, SCHEMA);
        assert_eq!(inventory.os.family, os_family());
        assert!(inventory.cpu.logical_cores > 0);
        assert!(inventory.memory.total > 0);
        assert!(inventory
            .networks
            .iter()
            .all(|network| network.name != "lo"));
        thread::sleep(Duration::from_millis(250));
        let metrics = collector.metrics();
        assert_eq!(
            metrics.cpu.per_core.len() as u32,
            inventory.cpu.logical_cores
        );
        assert!((0.0..=100.0).contains(&metrics.cpu.total));
        assert!(metrics.memory.used <= metrics.memory.total);
        assert_eq!(metrics.process_count, 0, "the metrics read no process");
        assert!(collector.process_sample.is_none());
        let mounts: Vec<String> = metrics.disks.into_iter().map(|disk| disk.mount).collect();
        let listed: Vec<String> = (inventory.disks.into_iter())
            .map(|disk| disk.mount)
            .collect();
        assert_eq!(mounts, listed);
    }

    #[test]
    fn this_computer_lists_its_processes_by_name() {
        let mut collector = Collector::new();
        let first = collector.processes();
        assert!(first.t > 0);
        assert_eq!(first.window_s, 0.0);
        assert!(first.processes.iter().all(|process| process.cpu == 0.0));
        let own = |list: &ProcessList| {
            (list.processes.iter())
                .find(|process| process.pid == std::process::id())
                .cloned()
                .expect("this process is listed")
        };
        let before = own(&first);
        assert!(before.start > 0);
        let count = first.processes.len() as u64;
        assert_eq!(collector.metrics().process_count, count);

        // Work for a while, so that the next refresh has a load to show.
        let busy = Instant::now();
        while busy.elapsed() < Duration::from_millis(300) {
            std::hint::black_box(busy.elapsed());
        }
        let second = collector.processes();
        assert!(
            (0.25..5.0).contains(&second.window_s),
            "{}",
            second.window_s
        );
        assert!(second.t >= first.t);
        let after = own(&second);
        assert!(after.cpu_time_ms >= before.cpu_time_ms);
        if after.cpu_time_ms > before.cpu_time_ms {
            assert!(after.cpu > 0.0, "{after:?}");
        }
        // The kept times are those of the last refresh and no more.
        let kept = collector.process_sample.as_ref().unwrap().times.len();
        assert_eq!(kept, second.processes.len());
        assert_eq!(collector.usb_devices().schema, SCHEMA);
    }

    #[test]
    fn this_computer_lists_its_top_processes() {
        let mut collector = Collector::new();
        let first = collector.top();
        assert_eq!(first.window_s, 0.0);
        assert!(!first.top.is_empty() && first.top.len() <= TOP_PROCESSES);
        assert!(first.process_count >= first.top.len() as u64);
        assert!(first.top.iter().all(|process| process.cpu == 0.0));
        assert!(first
            .top
            .windows(2)
            .all(|pair| pair[0].cpu_time_ms >= pair[1].cpu_time_ms));
        thread::sleep(Duration::from_millis(250));
        // The window runs from the previous refresh, whichever list it was for.
        let list = collector.processes();
        assert!(list.window_s >= 0.25);
        thread::sleep(Duration::from_millis(250));
        let second = collector.top();
        assert!(
            (0.25..5.0).contains(&second.window_s),
            "{}",
            second.window_s
        );
        assert!(second.top.windows(2).all(|pair| pair[0].cpu >= pair[1].cpu));
        assert_eq!(collector.metrics().process_count, second.process_count);
    }
}

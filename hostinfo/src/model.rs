//! The JSON documents of the host info service, `schema` 1. The field names
//! and units are the wire contract: PROTOCOL.md describes them, and the KVM
//! and its web UI read them. Sizes are bytes, rates are bytes per second,
//! percentages are 0 to 100, times are seconds since the Unix epoch unless a
//! field says otherwise. A value the computer does not tell is an empty
//! string, zero, an empty list, or `null` where the field allows it.

use serde::Serialize;

/// The schema number every document carries.
pub const SCHEMA: u32 = 1;

/// The most processes a `TopList` lists.
pub const TOP_PROCESSES: usize = 10;

/// What the computer is: it changes rarely.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Inventory {
    pub schema: u32,
    pub os: OsInfo,
    pub boot_time: u64,
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub gpus: Vec<GpuInfo>,
    pub disks: Vec<DiskInfo>,
    pub networks: Vec<NetworkInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct OsInfo {
    /// One of windows, macos, linux, freebsd, netbsd, openbsd, other.
    pub family: String,
    pub name: String,
    pub version: String,
    pub kernel: String,
    pub arch: String,
    pub hostname: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CpuInfo {
    pub brand: String,
    pub vendor: String,
    pub physical_cores: u32,
    pub logical_cores: u32,
    pub frequency_mhz: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MemoryInfo {
    pub total: u64,
    pub swap_total: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: String,
    /// Dedicated video memory; zero when the adapter shares the main memory
    /// or does not tell.
    pub vram: u64,
    pub driver: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DiskInfo {
    pub name: String,
    pub mount: String,
    pub fs: String,
    pub total: u64,
    pub available: u64,
    pub removable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct NetworkInfo {
    pub name: String,
    /// Lower-case hex pairs with colons; empty when the interface has none.
    pub mac: String,
    /// Each address with its prefix length, such as 192.168.1.2/24.
    pub addresses: Vec<String>,
    pub up: bool,
}

/// What the computer is doing now.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Metrics {
    pub schema: u32,
    /// Milliseconds since the Unix epoch.
    pub t: u64,
    pub uptime_s: u64,
    pub cpu: CpuLoad,
    /// The load averages over 1, 5 and 15 minutes; zeros on Windows.
    pub load: [f64; 3],
    pub memory: MemoryUse,
    pub networks: Vec<NetworkRate>,
    pub disks: Vec<DiskUse>,
    pub temps: Vec<Temperature>,
    pub gpus: Vec<GpuLoad>,
    /// The processes the last process list or top list counted; zero
    /// before the first one. The metrics never look at the processes.
    pub process_count: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CpuLoad {
    pub total: f32,
    pub per_core: Vec<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MemoryUse {
    pub used: u64,
    pub total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct NetworkRate {
    pub name: String,
    /// Bytes per second since the previous sample.
    pub rx_bps: u64,
    pub tx_bps: u64,
    pub rx_total: u64,
    pub tx_total: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DiskUse {
    pub mount: String,
    pub total: u64,
    pub available: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Temperature {
    pub label: String,
    /// Degrees Celsius.
    pub c: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GpuLoad {
    pub name: String,
    /// Percent busy; `null` where no unprivileged source tells.
    pub load: Option<f32>,
    pub vram_used: Option<u64>,
}

/// Every process, by name only: no command line and no environment.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProcessList {
    pub schema: u32,
    /// When the processes were read, in milliseconds since the Unix epoch.
    pub t: u64,
    /// The seconds between the previous process refresh and this one, which
    /// every `cpu` is an average over. Zero for the first refresh, which
    /// has no window: every `cpu` is zero then.
    pub window_s: f64,
    pub processes: Vec<ProcessInfo>,
}

/// The processes that used the most CPU over the window.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TopList {
    pub schema: u32,
    /// As in `ProcessList`.
    pub t: u64,
    pub window_s: f64,
    /// All the processes, not only the listed ones.
    pub process_count: u64,
    /// At most `TOP_PROCESSES`: the highest `cpu` first, then the highest
    /// `cpu_time_ms`, then the highest `mem`.
    pub top: Vec<ProcessInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub user: String,
    /// The average percent of one core over the window; more than 100 for
    /// a process on several cores.
    pub cpu: f32,
    /// The CPU time, user and system, since the process started, in
    /// milliseconds. Zero for a process whose time needs more rights.
    pub cpu_time_ms: u64,
    /// Resident memory.
    pub mem: u64,
    pub start: u64,
}

/// The USB devices attached to the computer.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct UsbList {
    pub schema: u32,
    pub devices: Vec<UsbDevice>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct UsbDevice {
    pub bus: String,
    pub address: u32,
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: String,
    pub product: String,
    pub class: u8,
    /// One of low, full, high, super, super+, or empty.
    pub speed: String,
}

impl Inventory {
    /// The document as JSON of at most `limit` bytes. A computer with very
    /// many mounts or interfaces loses the tail of its longest list until
    /// the document fits; the document stays valid JSON.
    pub fn to_json_within(&self, limit: usize) -> String {
        let mut inventory = self.clone();
        loop {
            let json = to_json(&inventory);
            if json.len() <= limit {
                return json;
            }
            let lists = [
                inventory.disks.len(),
                inventory.networks.len(),
                inventory.gpus.len(),
            ];
            match longest(&lists) {
                Some(0) => halve(&mut inventory.disks),
                Some(1) => halve(&mut inventory.networks),
                Some(_) => halve(&mut inventory.gpus),
                None => return json,
            }
        }
    }
}

impl Metrics {
    /// The document as JSON of at most `limit` bytes; see
    /// `Inventory::to_json_within`.
    pub fn to_json_within(&self, limit: usize) -> String {
        let mut metrics = self.clone();
        loop {
            let json = to_json(&metrics);
            if json.len() <= limit {
                return json;
            }
            let lists = [
                metrics.disks.len(),
                metrics.networks.len(),
                metrics.temps.len(),
                metrics.cpu.per_core.len(),
                metrics.gpus.len(),
            ];
            match longest(&lists) {
                Some(0) => halve(&mut metrics.disks),
                Some(1) => halve(&mut metrics.networks),
                Some(2) => halve(&mut metrics.temps),
                Some(3) => halve(&mut metrics.cpu.per_core),
                Some(_) => halve(&mut metrics.gpus),
                None => return json,
            }
        }
    }
}

/// Any document as JSON. The documents hold only strings, numbers, booleans
/// and lists, so serializing them cannot fail.
pub fn to_json<T: Serialize>(document: &T) -> String {
    serde_json::to_string(document).unwrap_or_else(|_| "{}".to_owned())
}

/// The index of the longest list, or None when every list is empty.
fn longest(lengths: &[usize]) -> Option<usize> {
    let (index, length) = lengths
        .iter()
        .enumerate()
        .max_by_key(|(index, length)| (**length, usize::MAX - index))?;
    (*length > 0).then_some(index)
}

fn halve<T>(list: &mut Vec<T>) {
    list.truncate(list.len() / 2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn keys(value: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    fn sample_inventory() -> Inventory {
        Inventory {
            schema: SCHEMA,
            os: OsInfo {
                family: "linux".into(),
                name: "Ubuntu".into(),
                version: "24.04".into(),
                kernel: "6.8.0-45-generic".into(),
                arch: "x86_64".into(),
                hostname: "box".into(),
            },
            boot_time: 1_795_000_000,
            cpu: CpuInfo {
                brand: "CPU".into(),
                vendor: "GenuineIntel".into(),
                physical_cores: 8,
                logical_cores: 16,
                frequency_mhz: 3600,
            },
            memory: MemoryInfo {
                total: 1,
                swap_total: 2,
            },
            gpus: vec![GpuInfo::default()],
            disks: vec![DiskInfo::default()],
            networks: vec![NetworkInfo {
                name: "eth0".into(),
                mac: "00:11:22:33:44:55".into(),
                addresses: vec!["192.168.1.2/24".into()],
                up: true,
            }],
        }
    }

    #[test]
    fn inventory_has_the_contract_keys() {
        let value = serde_json::to_value(sample_inventory()).unwrap();
        assert_eq!(
            keys(&value),
            [
                "boot_time",
                "cpu",
                "disks",
                "gpus",
                "memory",
                "networks",
                "os",
                "schema"
            ]
        );
        assert_eq!(value["schema"], 1);
        assert_eq!(
            keys(&value["os"]),
            ["arch", "family", "hostname", "kernel", "name", "version"]
        );
        assert_eq!(
            keys(&value["cpu"]),
            [
                "brand",
                "frequency_mhz",
                "logical_cores",
                "physical_cores",
                "vendor"
            ]
        );
        assert_eq!(keys(&value["memory"]), ["swap_total", "total"]);
        assert_eq!(
            keys(&value["gpus"][0]),
            ["driver", "name", "vendor", "vram"]
        );
        assert_eq!(
            keys(&value["disks"][0]),
            ["available", "fs", "mount", "name", "removable", "total"]
        );
        assert_eq!(
            value["networks"][0],
            json!({"name": "eth0", "mac": "00:11:22:33:44:55",
                   "addresses": ["192.168.1.2/24"], "up": true})
        );
    }

    fn sample_metrics() -> Metrics {
        Metrics {
            schema: SCHEMA,
            t: 1_795_000_000_000,
            uptime_s: 5,
            cpu: CpuLoad {
                total: 12.5,
                per_core: vec![10.0, 15.0],
            },
            load: [0.5, 0.25, 0.0],
            memory: MemoryUse::default(),
            networks: vec![NetworkRate::default()],
            disks: vec![DiskUse::default()],
            temps: vec![Temperature {
                label: "cpu".into(),
                c: 45.0,
            }],
            gpus: vec![GpuLoad {
                name: "gpu".into(),
                load: None,
                vram_used: Some(7),
            }],
            process_count: 3,
        }
    }

    #[test]
    fn metrics_have_the_contract_keys() {
        let value = serde_json::to_value(sample_metrics()).unwrap();
        assert_eq!(
            keys(&value),
            [
                "cpu",
                "disks",
                "gpus",
                "load",
                "memory",
                "networks",
                "process_count",
                "schema",
                "t",
                "temps",
                "uptime_s"
            ]
        );
        assert!(value.get("top").is_none());
        assert_eq!(
            value["cpu"],
            json!({"total": 12.5, "per_core": [10.0, 15.0]})
        );
        assert_eq!(value["load"], json!([0.5, 0.25, 0.0]));
        assert_eq!(
            keys(&value["memory"]),
            ["swap_total", "swap_used", "total", "used"]
        );
        assert_eq!(
            keys(&value["networks"][0]),
            ["name", "rx_bps", "rx_total", "tx_bps", "tx_total"]
        );
        assert_eq!(keys(&value["disks"][0]), ["available", "mount", "total"]);
        assert_eq!(value["temps"][0], json!({"label": "cpu", "c": 45.0}));
        assert_eq!(
            value["gpus"][0],
            json!({"name": "gpu", "load": null, "vram_used": 7})
        );
    }

    #[test]
    fn process_and_usb_lists_have_the_contract_keys() {
        let process = ProcessInfo {
            pid: 2,
            ppid: 1,
            name: "sh".into(),
            user: "root".into(),
            cpu: 0.5,
            cpu_time_ms: 1250,
            mem: 10,
            start: 1_795_000_000,
        };
        let process_json = json!({"pid": 2, "ppid": 1, "name": "sh", "user": "root",
            "cpu": 0.5, "cpu_time_ms": 1250, "mem": 10, "start": 1_795_000_000});
        let processes = ProcessList {
            schema: SCHEMA,
            t: 1_795_000_000_000,
            window_s: 5.02,
            processes: vec![process.clone()],
        };
        assert_eq!(
            serde_json::to_value(processes).unwrap(),
            json!({"schema": 1, "t": 1_795_000_000_000_u64, "window_s": 5.02,
                "processes": [process_json.clone()]})
        );
        let top = TopList {
            schema: SCHEMA,
            t: 1_795_000_000_000,
            window_s: 10.0,
            process_count: 950,
            top: vec![process],
        };
        assert_eq!(
            serde_json::to_value(top).unwrap(),
            json!({"schema": 1, "t": 1_795_000_000_000_u64, "window_s": 10.0,
                "process_count": 950, "top": [process_json]})
        );
        let usb = UsbList {
            schema: SCHEMA,
            devices: vec![UsbDevice {
                bus: "1".into(),
                address: 4,
                vid: 7531,
                pid: 2,
                manufacturer: "Linux".into(),
                product: "Hub".into(),
                class: 9,
                speed: "high".into(),
            }],
        };
        assert_eq!(
            serde_json::to_value(usb).unwrap(),
            json!({"schema": 1, "devices": [{"bus": "1", "address": 4, "vid": 7531,
                "pid": 2, "manufacturer": "Linux", "product": "Hub", "class": 9,
                "speed": "high"}]})
        );
    }

    #[test]
    fn an_oversized_document_loses_list_entries_and_stays_json() {
        let mut inventory = sample_inventory();
        inventory.disks = (0..2000)
            .map(|index| DiskInfo {
                mount: format!("/snap/package/{index}"),
                ..DiskInfo::default()
            })
            .collect();
        let whole = to_json(&inventory);
        assert!(whole.len() > 64 << 10);
        let cut = inventory.to_json_within(64 << 10);
        assert!(cut.len() <= 64 << 10);
        let value: Value = serde_json::from_str(&cut).unwrap();
        let disks = value["disks"].as_array().unwrap();
        assert!(!disks.is_empty() && disks.len() < 2000);
        assert_eq!(value["networks"].as_array().unwrap().len(), 1);
        assert_eq!(inventory.to_json_within(1 << 20), whole);

        let mut metrics = sample_metrics();
        metrics.cpu.per_core = vec![1.0; 50_000];
        let cut = metrics.to_json_within(64 << 10);
        assert!(cut.len() <= 64 << 10);
        assert!(serde_json::from_str::<Value>(&cut).is_ok());
    }
}

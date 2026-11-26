//! Display adapters on Linux: one per `/sys/class/drm/card<N>`. The kernel
//! tells the PCI IDs and the driver; the names come from the PCI ID list
//! when the computer has one.
//!
//! The load: amdgpu publishes its load and memory in sysfs; the GPU of a
//! system on a chip (Mali and the like) publishes its load through devfreq
//! when its driver uses it; the NVIDIA driver tells both through
//! `nvidia-smi`. Intel's drivers tell the load only to a user who may read
//! the performance counters, so it stays unknown.

use super::Measured;
use crate::model::GpuInfo;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
pub const DRM: &str = "/sys/class/drm";

/// Where distributions keep the PCI ID list.
#[cfg(target_os = "linux")]
const PCI_IDS: [&str; 3] = [
    "/usr/share/hwdata/pci.ids",
    "/usr/share/misc/pci.ids",
    "/usr/share/pci.ids",
];

/// The adapters, in card order.
#[cfg(target_os = "linux")]
pub fn inventory(drm: &Path) -> Vec<GpuInfo> {
    let ids = PCI_IDS
        .iter()
        .find_map(|path| fs::read_to_string(path).ok())
        .unwrap_or_default();
    inventory_with(drm, &ids)
}

pub fn inventory_with(drm: &Path, pci_ids: &str) -> Vec<GpuInfo> {
    cards(drm)
        .iter()
        .map(|device| adapter(device, pci_ids))
        .collect()
}

/// The load in percent and the video memory in use of each adapter, in
/// card order.
#[cfg(target_os = "linux")]
pub fn load(drm: &Path) -> Vec<Measured> {
    load_with(drm, nvidia::samples)
}

/// `load`, with the samples of `nvidia-smi` from `nvidia`, which runs only
/// when a card has the NVIDIA driver.
pub fn load_with(drm: &Path, nvidia: impl FnOnce() -> Vec<NvidiaSample>) -> Vec<Measured> {
    let cards = cards(drm);
    let has_nvidia = cards.iter().any(|device| driver(device) == "nvidia");
    let nvidia = if has_nvidia { nvidia() } else { Vec::new() };
    cards
        .iter()
        .map(|device| {
            let sysfs = Measured {
                load: number::<f32>(&device.join("gpu_busy_percent"))
                    .or_else(|| devfreq_load(device))
                    .filter(|busy| (0.0..=100.0).contains(busy)),
                vram_used: number::<u64>(&device.join("mem_info_vram_used")),
            };
            let slot = pci_slot(device);
            let smi = (nvidia.iter())
                .find(|sample| slot.is_some() && sample.slot == slot)
                .map(|sample| sample.measured)
                .unwrap_or_default();
            Measured {
                load: sysfs.load.or(smi.load),
                vram_used: sysfs.vram_used.or(smi.vram_used),
            }
        })
        .collect()
}

/// The load a devfreq governor measured for the device, as `45@800000000Hz`
/// or `45`: the device's own `devfreq/<name>/load`. Mali and other GPUs of
/// systems on a chip publish it, depending on the kernel.
fn devfreq_load(device: &Path) -> Option<f32> {
    let entries = fs::read_dir(device.join("devfreq")).ok()?;
    entries.flatten().find_map(|entry| {
        let text = text(&entry.path().join("load"))?;
        text.split('@').next()?.trim().parse().ok()
    })
}

/// The PCI address of a device, such as `0000:01:00.0`, from its uevent.
fn pci_slot(device: &Path) -> Option<PciSlot> {
    let uevent = fs::read_to_string(device.join("uevent")).ok()?;
    let slot = uevent
        .lines()
        .find_map(|line| line.strip_prefix("PCI_SLOT_NAME="))?;
    PciSlot::parse(slot)
}

/// A PCI address: domain, bus, device and function. `nvidia-smi` writes
/// the domain with eight digits, the kernel with four.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciSlot(u32, u8, u8, u8);

impl PciSlot {
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split([':', '.']);
        let mut next = || parts.next();
        let domain = u32::from_str_radix(next()?, 16).ok()?;
        let bus = u8::from_str_radix(next()?, 16).ok()?;
        let device = u8::from_str_radix(next()?, 16).ok()?;
        let function = u8::from_str_radix(next()?, 16).ok()?;
        Some(Self(domain, bus, device, function))
    }
}

/// One adapter in what `nvidia-smi` prints.
#[derive(Clone, Debug, PartialEq)]
pub struct NvidiaSample {
    slot: Option<PciSlot>,
    measured: Measured,
}

/// The lines of `nvidia-smi --query-gpu=pci.bus_id,utilization.gpu,
/// memory.used --format=csv,noheader,nounits`: `00000000:01:00.0, 37,
/// 1234`, the memory in MiB. A value the card does not report reads
/// `[N/A]` or `[Not Supported]`.
pub fn parse_nvidia(text: &str) -> Vec<NvidiaSample> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(',').map(str::trim);
            let slot = PciSlot::parse(fields.next()?);
            let load = fields.next()?.parse::<f32>().ok();
            let used = fields.next()?.parse::<u64>().ok();
            Some(NvidiaSample {
                slot,
                measured: Measured {
                    load: load.filter(|load| (0.0..=100.0).contains(load)),
                    vram_used: used.map(|mib| mib << 20),
                },
            })
        })
        .collect()
}

/// `nvidia-smi`, where the driver installed it. It takes tens of
/// milliseconds, so it runs on its own thread, at most every two seconds;
/// a sample takes the latest result.
#[cfg(target_os = "linux")]
mod nvidia {
    use super::{parse_nvidia, NvidiaSample};
    use crate::gpu::background::Background;
    use std::{
        path::{Path, PathBuf},
        sync::OnceLock,
        time::Duration,
    };

    const EVERY: Duration = Duration::from_secs(2);
    /// A wedged driver can hang the tool: it is killed after this.
    const LIMIT: Duration = Duration::from_secs(5);

    pub fn samples() -> Vec<NvidiaSample> {
        static LATEST: Background<Vec<NvidiaSample>> = Background::new();
        if program().is_none() {
            return Vec::new();
        }
        LATEST.get(EVERY, run).unwrap_or_default()
    }

    fn run() -> Option<Vec<NvidiaSample>> {
        let arguments = [
            "--query-gpu=pci.bus_id,utilization.gpu,memory.used",
            "--format=csv,noheader,nounits",
        ];
        let text = crate::command::output(program()?.to_str()?, &arguments, LIMIT)?;
        Some(parse_nvidia(&text))
    }

    /// The program on the PATH, or where the driver packages put it. A
    /// relative PATH entry would find a program in the working folder.
    fn program() -> Option<&'static PathBuf> {
        static PROGRAM: OnceLock<Option<PathBuf>> = OnceLock::new();
        PROGRAM
            .get_or_init(|| {
                let path = std::env::var_os("PATH").unwrap_or_default();
                std::env::split_paths(&path)
                    .filter(|dir| dir.is_absolute())
                    .chain([PathBuf::from("/usr/bin"), PathBuf::from("/bin")])
                    .map(|dir| dir.join("nvidia-smi"))
                    .find(|program| is_program(program))
            })
            .as_ref()
    }

    fn is_program(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
}

/// The device directory of every card. `card0-HDMI-A-1` and the like are
/// connectors, not cards; a card without a device is not an adapter.
fn cards(drm: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(drm) else {
        return Vec::new();
    };
    let mut cards: Vec<(u32, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let index = name.to_str()?.strip_prefix("card")?.parse().ok()?;
            let device = entry.path().join("device");
            device.is_dir().then_some((index, device))
        })
        .collect();
    cards.sort();
    cards.into_iter().map(|(_, device)| device).collect()
}

fn adapter(device: &Path, pci_ids: &str) -> GpuInfo {
    let vendor_id = hex_id(&device.join("vendor"));
    let device_id = hex_id(&device.join("device"));
    let driver = driver(device);
    let listed = vendor_id.and_then(|vendor| lookup(pci_ids, vendor, device_id));
    let (listed_vendor, listed_device) = listed.unwrap_or_default();
    let vendor = match vendor_id.and_then(vendor_name) {
        Some(known) => known.to_owned(),
        None => listed_vendor,
    };
    let name = [
        text(&device.join("product_name")),
        text(&device.join("label")),
    ]
    .into_iter()
    .flatten()
    .next()
    .or(listed_device.map(|model| format!("{vendor} {model}").trim().to_owned()))
    .unwrap_or_else(|| fallback_name(&vendor, vendor_id, device_id, &driver));
    GpuInfo {
        name,
        vendor,
        vram: number(&device.join("mem_info_vram_total")).unwrap_or(0),
        driver,
    }
}

/// A name for an adapter the PCI ID list does not know, or one that is not
/// a PCI device: the vendor and the IDs, or the driver.
fn fallback_name(
    vendor: &str,
    vendor_id: Option<u16>,
    device_id: Option<u16>,
    driver: &str,
) -> String {
    match (vendor_id, device_id) {
        (Some(vendor_id), Some(device_id)) if !vendor.is_empty() => {
            format!("{vendor} {vendor_id:04x}:{device_id:04x}")
        }
        (Some(vendor_id), Some(device_id)) => format!("{vendor_id:04x}:{device_id:04x}"),
        _ => driver.to_owned(),
    }
}

/// The driver bound to the device: the target of its `driver` link, or the
/// DRIVER line of its uevent.
fn driver(device: &Path) -> String {
    let linked = fs::read_link(device.join("driver"))
        .ok()
        .and_then(|target| Some(target.file_name()?.to_str()?.to_owned()));
    linked
        .or_else(|| {
            let uevent = fs::read_to_string(device.join("uevent")).ok()?;
            let line = uevent
                .lines()
                .find_map(|line| line.strip_prefix("DRIVER="))?;
            Some(line.trim().to_owned())
        })
        .unwrap_or_default()
}

/// The vendors of nearly every adapter, by the names people know them by.
fn vendor_name(vendor: u16) -> Option<&'static str> {
    Some(match vendor {
        0x10de => "NVIDIA",
        0x1002 | 0x1022 => "AMD",
        0x8086 => "Intel",
        0x106b => "Apple",
        0x1414 => "Microsoft",
        0x15ad => "VMware",
        0x1af4 => "Red Hat (virtio)",
        0x1b36 => "Red Hat (QXL)",
        0x1234 => "QEMU",
        0x80ee => "VirtualBox",
        0x102b => "Matrox",
        0x1a03 => "ASPEED",
        0x5143 => "Qualcomm",
        0x13b5 => "ARM",
        _ => return None,
    })
}

/// The vendor's name and, when listed, the device's name from the text of
/// a PCI ID list. A vendor starts its line with four hex digits; its
/// devices follow, each after one tab; subsystems, after two tabs, do not
/// count.
pub fn lookup(pci_ids: &str, vendor: u16, device: Option<u16>) -> Option<(String, Option<String>)> {
    let vendor_key = format!("{vendor:04x}");
    let mut lines = pci_ids.lines();
    let vendor_name = lines.by_ref().find_map(|line| entry(line, &vendor_key))?;
    let device_key = device.map(|device| format!("{device:04x}"));
    let device_name = device_key.and_then(|key| {
        lines
            .take_while(|line| line.starts_with('\t') || line.starts_with('#') || line.is_empty())
            .filter(|line| !line.starts_with("\t\t"))
            .find_map(|line| entry(line.strip_prefix('\t')?, &key))
    });
    Some((vendor_name, device_name))
}

/// The name on a line that starts with `key` and two spaces.
fn entry(line: &str, key: &str) -> Option<String> {
    let name = line.strip_prefix(key)?.strip_prefix("  ")?;
    Some(name.trim().to_owned())
}

fn text(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn number<T: std::str::FromStr>(path: &Path) -> Option<T> {
    text(path)?.parse().ok()
}

/// A sysfs ID such as `0x10de`.
fn hex_id(path: &Path) -> Option<u16> {
    let text = text(path)?;
    u16::from_str_radix(text.trim_start_matches("0x"), 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PCI_IDS: &str = "# comment\n\
        1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\
        \t73bf  Navi 21 [Radeon RX 6800/6800 XT / 6900 XT]\n\
        \t\t1002 0e3a  Radeon RX 6900 XT\n\
        \t744c  Navi 31 [Radeon RX 7900 XT/7900 XTX]\n\
        10de  NVIDIA Corporation\n\
        \t2684  GA102 [GeForce RTX 4090]\n\
        \t744c  Not the AMD card\n\
        ffff  Illegal Vendor ID\n";

    #[test]
    fn finds_names_in_the_pci_id_list() {
        assert_eq!(
            lookup(PCI_IDS, 0x1002, Some(0x744c)),
            Some((
                "Advanced Micro Devices, Inc. [AMD/ATI]".into(),
                Some("Navi 31 [Radeon RX 7900 XT/7900 XTX]".into())
            ))
        );
        assert_eq!(
            lookup(PCI_IDS, 0x10de, Some(0x2684)),
            Some((
                "NVIDIA Corporation".into(),
                Some("GA102 [GeForce RTX 4090]".into())
            ))
        );
        // A subsystem line and another vendor's device do not match.
        assert_eq!(
            lookup(PCI_IDS, 0x1002, Some(0x0e3a)),
            Some(("Advanced Micro Devices, Inc. [AMD/ATI]".into(), None))
        );
        assert_eq!(lookup(PCI_IDS, 0x1002, Some(0x2684)).unwrap().1, None);
        assert_eq!(lookup(PCI_IDS, 0x8086, Some(1)), None);
        assert_eq!(lookup("", 0x8086, None), None);
    }

    /// A sysfs tree with an amdgpu card, a card that is not a PCI device,
    /// a connector and a card without a device.
    fn tree(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ipkvm-hostinfo-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write("card1/device/vendor", "0x1002\n");
        write("card1/device/device", "0x744c\n");
        write("card1/device/uevent", "DRIVER=amdgpu\nPCI_ID=1002:744C\n");
        write("card1/device/mem_info_vram_total", "25753026560\n");
        write("card1/device/mem_info_vram_used", "1073741824\n");
        write("card1/device/gpu_busy_percent", "37\n");
        write("card0/device/devfreq/ff400000.gpu/load", "45@800000000Hz\n");
        write(
            "card3/device/uevent",
            "DRIVER=nvidia\nPCI_SLOT_NAME=0000:01:00.0\n",
        );
        write("card3/device/vendor", "0x10de\n");
        write("card3/device/device", "0x2684\n");
        write("card0/device/uevent", "DRIVER=vc4-drm\nOF_NAME=gpu\n");
        write("card1-HDMI-A-1/device/vendor", "0x1002\n");
        write("card2/dev", "226:2\n");
        write("version", "drm 1.1.0\n");
        root
    }

    #[test]
    fn reads_the_adapters_from_sysfs() {
        let root = tree("inventory");
        let adapters = inventory_with(&root, PCI_IDS);
        assert_eq!(
            adapters,
            [
                GpuInfo {
                    name: "vc4-drm".into(),
                    vendor: String::new(),
                    vram: 0,
                    driver: "vc4-drm".into(),
                },
                GpuInfo {
                    name: "AMD Navi 31 [Radeon RX 7900 XT/7900 XTX]".into(),
                    vendor: "AMD".into(),
                    vram: 25_753_026_560,
                    driver: "amdgpu".into(),
                },
                GpuInfo {
                    name: "NVIDIA GA102 [GeForce RTX 4090]".into(),
                    vendor: "NVIDIA".into(),
                    vram: 0,
                    driver: "nvidia".into(),
                },
            ]
        );
        let unlisted = inventory_with(&root, "");
        assert_eq!(unlisted[1].name, "AMD 1002:744c");
        fs::remove_dir_all(root).unwrap();
    }

    fn measured(load: Option<f32>, vram_used: Option<u64>) -> Measured {
        Measured { load, vram_used }
    }

    #[test]
    fn reads_the_load_from_sysfs_devfreq_and_nvidia_smi() {
        let root = tree("load");
        let smi = "00000000:01:00.0, 12, 2048\n00000000:02:00.0, [N/A], [N/A]\n";
        assert_eq!(
            load_with(&root, || parse_nvidia(smi)),
            [
                measured(Some(45.0), None),
                measured(Some(37.0), Some(1_073_741_824)),
                measured(Some(12.0), Some(2048 << 20)),
            ]
        );
        // nvidia-smi is not asked without an NVIDIA card.
        fs::remove_dir_all(root.join("card3")).unwrap();
        let loads = load_with(&root, || panic!("nvidia-smi ran"));
        assert_eq!(loads.len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_what_nvidia_smi_prints() {
        let samples = parse_nvidia(
            "00000000:0A:00.0, 99, 512\nnot a line\n0000:0b:00.0, [Not Supported], 7\n",
        );
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].slot, Some(PciSlot(0, 0x0a, 0, 0)));
        assert_eq!(samples[0].measured, measured(Some(99.0), Some(512 << 20)));
        assert_eq!(samples[1].slot, Some(PciSlot(0, 0x0b, 0, 0)));
        assert_eq!(samples[1].measured, measured(None, Some(7 << 20)));
        assert!(parse_nvidia("").is_empty());
    }

    #[test]
    fn a_missing_tree_has_no_adapters() {
        let missing = Path::new("/nonexistent/ipkvm-hostinfo/drm");
        assert!(inventory_with(missing, PCI_IDS).is_empty());
        assert!(load_with(missing, Vec::new).is_empty());
    }
}

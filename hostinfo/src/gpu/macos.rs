//! Display adapters on macOS, from `system_profiler SPDisplaysDataType`.
//! The tool takes about half a second, so it runs once per process: the
//! adapters of a Mac do not change while it runs, apart from an external
//! GPU, which the next start finds.
//!
//! The load comes from IOKit: each `IOAccelerator` object, one per GPU,
//! has a `PerformanceStatistics` dictionary that a normal user may read,
//! as Activity Monitor does. Reading it takes well under a millisecond.

use super::Measured;
use crate::model::GpuInfo;
use serde_json::Value;

#[cfg(target_os = "macos")]
const SYSTEM_PROFILER: &str = "/usr/sbin/system_profiler";
#[cfg(target_os = "macos")]
const LIMIT: std::time::Duration = std::time::Duration::from_secs(8);

#[cfg(target_os = "macos")]
pub fn inventory() -> Vec<GpuInfo> {
    static ADAPTERS: std::sync::OnceLock<Vec<GpuInfo>> = std::sync::OnceLock::new();
    ADAPTERS
        .get_or_init(|| {
            let arguments = ["SPDisplaysDataType", "-json"];
            crate::command::output(SYSTEM_PROFILER, &arguments, LIMIT)
                .map(|json| parse(&json))
                .unwrap_or_default()
        })
        .clone()
}

/// The load of the adapters, in the order of `adapters`.
#[cfg(target_os = "macos")]
pub fn load(adapters: &[GpuInfo]) -> Vec<Measured> {
    super::by_name(adapters, &iokit::accelerators())
}

/// The keys of `PerformanceStatistics` that tell the load in percent:
/// Apple silicon, AMD and Intel GPUs write the first, older drivers the
/// second.
const LOAD_KEYS: [&str; 2] = ["Device Utilization %", "GPU Activity(%)"];
/// The keys that tell the memory in use, in bytes: the dedicated memory of
/// an AMD or NVIDIA GPU first, then the main memory that Apple silicon and
/// Intel GPUs use.
const MEMORY_KEYS: [&str; 4] = [
    "vramUsedBytes",
    "inUseVidMemoryBytes",
    "In use system memory",
    "inUseSystemMemory",
];

/// The load and memory in a `PerformanceStatistics` dictionary, whose
/// numbers `number` looks up by key.
pub fn measure(number: impl Fn(&str) -> Option<f64>) -> Measured {
    let load = LOAD_KEYS.iter().find_map(|key| number(key));
    let used = MEMORY_KEYS.iter().find_map(|key| number(key));
    Measured {
        load: load
            .filter(|load| (0.0..=100.0).contains(load))
            .map(|load| load as f32),
        vram_used: used.filter(|used| *used >= 0.0).map(|used| used as u64),
    }
}

/// The IOKit and Core Foundation calls that read the accelerators. The
/// crate links no binding of them, and needs only these few.
#[cfg(target_os = "macos")]
mod iokit {
    use super::{measure, Measured};
    use std::ffi::{c_char, c_void, CStr, CString};

    type CfType = *const c_void;
    type CfIndex = isize;
    type IoObject = u32;

    const UTF8: u32 = 0x0800_0100;
    const NUMBER_SINT64: CfIndex = 4;
    const NUMBER_FLOAT64: CfIndex = 6;
    /// `kIOMainPortDefault`.
    const MAIN_PORT: u32 = 0;
    const SUCCESS: i32 = 0;
    /// More accelerators than any Mac has; ends the loop in any case.
    const MAX_ACCELERATORS: usize = 16;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            allocator: CfType,
            text: *const c_char,
            encoding: u32,
        ) -> CfType;
        fn CFDictionaryGetValue(dictionary: CfType, key: CfType) -> CfType;
        fn CFGetTypeID(object: CfType) -> usize;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFDataGetTypeID() -> usize;
        fn CFNumberIsFloatType(number: CfType) -> u8;
        fn CFNumberGetValue(number: CfType, kind: CfIndex, value: *mut c_void) -> u8;
        fn CFStringGetCString(
            text: CfType,
            buffer: *mut c_char,
            size: CfIndex,
            encoding: u32,
        ) -> u8;
        fn CFDataGetLength(data: CfType) -> CfIndex;
        fn CFDataGetBytePtr(data: CfType) -> *const u8;
        fn CFRelease(object: CfType);
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceMatching(class: *const c_char) -> CfType;
        fn IOServiceGetMatchingServices(
            port: u32,
            matching: CfType,
            iterator: *mut IoObject,
        ) -> i32;
        fn IOIteratorNext(iterator: IoObject) -> IoObject;
        fn IOObjectRelease(object: IoObject) -> i32;
        fn IORegistryEntryCreateCFProperty(
            entry: IoObject,
            key: CfType,
            allocator: CfType,
            options: u32,
        ) -> CfType;
        fn IORegistryEntryGetParentEntry(
            entry: IoObject,
            plane: *const c_char,
            parent: *mut IoObject,
        ) -> i32;
    }

    /// A Core Foundation object this code owns, released when dropped.
    struct Owned(CfType);

    impl Owned {
        fn new(object: CfType) -> Option<Self> {
            (!object.is_null()).then_some(Self(object))
        }

        fn string(text: &str) -> Option<Self> {
            let text = CString::new(text).ok()?;
            // SAFETY: the text is a valid C string; the result is owned.
            Self::new(unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), UTF8) })
        }
    }

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the object came from a Create or Copy call.
            unsafe { CFRelease(self.0) };
        }
    }

    /// Every accelerator: the name of its GPU, as `system_profiler` tells
    /// it, and its load.
    pub fn accelerators() -> Vec<(String, Measured)> {
        let mut found = Vec::new();
        // SAFETY: IOServiceGetMatchingServices consumes the matching
        // dictionary; every object from the iterator, and the iterator, is
        // released.
        unsafe {
            let matching = IOServiceMatching(c"IOAccelerator".as_ptr());
            if matching.is_null() {
                return found;
            }
            let mut iterator: IoObject = 0;
            if IOServiceGetMatchingServices(MAIN_PORT, matching, &mut iterator) != SUCCESS {
                return found;
            }
            while found.len() < MAX_ACCELERATORS {
                let entry = IOIteratorNext(iterator);
                if entry == 0 {
                    break;
                }
                found.push(accelerator(entry));
                IOObjectRelease(entry);
            }
            IOObjectRelease(iterator);
        }
        found
    }

    /// # Safety
    /// `entry` must be a live registry entry.
    unsafe fn accelerator(entry: IoObject) -> (String, Measured) {
        let name = model(entry).unwrap_or_else(|| {
            let mut parent: IoObject = 0;
            let plane = c"IOService".as_ptr();
            if IORegistryEntryGetParentEntry(entry, plane, &mut parent) != SUCCESS {
                return String::new();
            }
            let name = model(parent).unwrap_or_default();
            IOObjectRelease(parent);
            name
        });
        let measured = property(entry, "PerformanceStatistics")
            .filter(|stats| CFGetTypeID(stats.0) == CFDictionaryGetTypeID())
            .map(|stats| measure(|key| number(&stats, key)))
            .unwrap_or_default();
        (name, measured)
    }

    /// # Safety
    /// `entry` must be a live registry entry.
    unsafe fn property(entry: IoObject, key: &str) -> Option<Owned> {
        let key = Owned::string(key)?;
        Owned::new(IORegistryEntryCreateCFProperty(
            entry,
            key.0,
            std::ptr::null(),
            0,
        ))
    }

    /// The `model` property: a string on an Apple silicon accelerator, data
    /// on a PCI device.
    ///
    /// # Safety
    /// `entry` must be a live registry entry.
    unsafe fn model(entry: IoObject) -> Option<String> {
        let model = property(entry, "model")?;
        let kind = CFGetTypeID(model.0);
        let text = if kind == CFStringGetTypeID() {
            let mut buffer = [0 as c_char; 256];
            let size = buffer.len() as CfIndex;
            if CFStringGetCString(model.0, buffer.as_mut_ptr(), size, UTF8) == 0 {
                return None;
            }
            CStr::from_ptr(buffer.as_ptr())
                .to_string_lossy()
                .into_owned()
        } else if kind == CFDataGetTypeID() {
            let length = usize::try_from(CFDataGetLength(model.0)).ok()?;
            let bytes = std::slice::from_raw_parts(CFDataGetBytePtr(model.0), length);
            let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(length);
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        } else {
            return None;
        };
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// A number in a dictionary.
    ///
    /// # Safety
    /// `dictionary` must be a CFDictionary.
    unsafe fn number(dictionary: &Owned, key: &str) -> Option<f64> {
        let key = Owned::string(key)?;
        let value = CFDictionaryGetValue(dictionary.0, key.0);
        if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
            return None;
        }
        if CFNumberIsFloatType(value) != 0 {
            let mut float = 0f64;
            let read = CFNumberGetValue(value, NUMBER_FLOAT64, (&mut float as *mut f64).cast());
            (read != 0).then_some(float)
        } else {
            let mut integer = 0i64;
            let read = CFNumberGetValue(value, NUMBER_SINT64, (&mut integer as *mut i64).cast());
            (read != 0).then_some(integer as f64)
        }
    }

    #[cfg(test)]
    mod tests {
        /// Prints this Mac's GPU load: `cargo test -p ipkvm-hostinfo
        /// live_sample -- --nocapture`.
        #[test]
        fn live_sample() {
            for (name, measured) in super::accelerators() {
                println!("accelerator {name:?}: {measured:?}");
            }
        }
    }
}

/// The adapters in the tool's JSON.
pub fn parse(json: &str) -> Vec<GpuInfo> {
    let Ok(report) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    let Some(adapters) = report["SPDisplaysDataType"].as_array() else {
        return Vec::new();
    };
    adapters
        .iter()
        .map(|adapter| {
            let field = |key: &str| adapter[key].as_str().unwrap_or_default();
            let name = [field("sppci_model"), field("_name")]
                .into_iter()
                .find(|name| !name.is_empty())
                .unwrap_or_default();
            // Dedicated memory only: an integrated adapter reports the main
            // memory it may borrow as `spdisplays_vram_shared`.
            let vram = [field("spdisplays_vram"), field("_spdisplays_vram")]
                .into_iter()
                .find_map(size)
                .unwrap_or(0);
            GpuInfo {
                name: name.to_owned(),
                vendor: vendor(field("spdisplays_vendor")),
                vram,
                driver: String::new(),
            }
        })
        .collect()
}

/// `sppci_vendor_Apple` and `NVIDIA (0x10de)` both name a vendor.
fn vendor(text: &str) -> String {
    let text = text.strip_prefix("sppci_vendor_").unwrap_or(text);
    let text = text.split(" (0x").next().unwrap_or(text).trim();
    match text.to_ascii_lowercase().as_str() {
        "amd" | "ati" => "AMD".to_owned(),
        "nvidia" => "NVIDIA".to_owned(),
        "intel" => "Intel".to_owned(),
        _ => text.to_owned(),
    }
}

/// A size such as `8 GB` or `1536 MB`, in bytes.
fn size(text: &str) -> Option<u64> {
    let mut words = text.split_whitespace();
    let amount: f64 = words.next()?.parse().ok()?;
    let unit: u64 = match words.next()?.to_ascii_uppercase().as_str() {
        "KB" => 1 << 10,
        "MB" => 1 << 20,
        "GB" => 1 << 30,
        "TB" => 1 << 40,
        _ => return None,
    };
    (amount >= 0.0).then_some((amount * unit as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_apple_silicon_adapter() {
        let json = r#"{"SPDisplaysDataType": [{
            "_name": "Apple M2 Pro",
            "spdisplays_mtlgpufamilysupport": "spdisplays_metal4",
            "spdisplays_ndrvs": [{"_name": "Color LCD"}],
            "spdisplays_vendor": "sppci_vendor_Apple",
            "sppci_bus": "spdisplays_builtin",
            "sppci_cores": "19",
            "sppci_device_type": "spdisplays_gpu",
            "sppci_model": "Apple M2 Pro"}]}"#;
        assert_eq!(
            parse(json),
            [GpuInfo {
                name: "Apple M2 Pro".into(),
                vendor: "Apple".into(),
                vram: 0,
                driver: String::new(),
            }]
        );
    }

    #[test]
    fn reads_an_intel_mac_with_two_adapters() {
        let json = r#"{"SPDisplaysDataType": [
            {"_name": "kHW_IntelUHDGraphics630Item", "sppci_model": "Intel UHD Graphics 630",
             "spdisplays_vendor": "Intel", "spdisplays_vram_shared": "1536 MB"},
            {"_name": "kHW_AMDRadeonPro5500MItem", "sppci_model": "AMD Radeon Pro 5500M",
             "spdisplays_vendor": "sppci_vendor_amd", "spdisplays_vram": "8 GB"},
            {"_name": "Old card", "spdisplays_vendor": "NVIDIA (0x10de)",
             "_spdisplays_vram": "512 MB"}]}"#;
        let adapters = parse(json);
        let summary: Vec<_> = adapters
            .iter()
            .map(|adapter| (adapter.name.as_str(), adapter.vendor.as_str(), adapter.vram))
            .collect();
        assert_eq!(
            summary,
            [
                ("Intel UHD Graphics 630", "Intel", 0),
                ("AMD Radeon Pro 5500M", "AMD", 8 << 30),
                ("Old card", "NVIDIA", 512 << 20),
            ]
        );
    }

    #[test]
    fn reads_the_performance_statistics() {
        let apple = [
            ("Device Utilization %", 33.0),
            ("Renderer Utilization %", 32.0),
            ("In use system memory", 1_122_549_760.0),
            ("Alloc system memory", 5_906_546_688.0),
        ];
        fn lookup(pairs: &[(&str, f64)], key: &str) -> Option<f64> {
            let found = pairs.iter().find(|(name, _)| *name == key);
            found.map(|(_, value)| *value)
        }
        let measured = measure(|key| lookup(&apple, key));
        assert_eq!(measured.load, Some(33.0));
        assert_eq!(measured.vram_used, Some(1_122_549_760));
        let amd = [
            ("GPU Activity(%)", 5.0),
            ("vramUsedBytes", 1024.0),
            ("inUseSystemMemory", 7.0),
        ];
        assert_eq!(
            measure(|key| lookup(&amd, key)),
            Measured {
                load: Some(5.0),
                vram_used: Some(1024)
            }
        );
        let odd = [("Device Utilization %", 250.0)];
        assert_eq!(measure(|key| lookup(&odd, key)), Measured::default());
    }

    #[test]
    fn output_that_is_not_the_report_has_no_adapters() {
        assert!(parse("").is_empty());
        assert!(parse("{}").is_empty());
        assert!(parse(r#"{"SPDisplaysDataType": "none"}"#).is_empty());
    }
}

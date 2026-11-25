//! Display adapters on Windows, from DXGI: the adapters Direct3D can use,
//! with their names and dedicated memory. DXGI needs no rights beyond a
//! normal user's. It does not tell the driver.
//!
//! The load and the memory in use come from the GPU performance counters
//! that Task Manager shows (`GPU Engine`, `GPU Adapter Memory`, Windows 10
//! 1709 and later), which a normal user may read. Their instances name the
//! adapter by its LUID, which DXGI tells. DXGI's own memory query covers
//! only the calling process, so it is no use here.

use super::{engines::Luid, Measured};
use crate::model::GpuInfo;
use std::{
    ptr,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};
use winapi::{
    shared::{
        dxgi::{
            CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_DESC1,
            DXGI_ADAPTER_FLAG_SOFTWARE,
        },
        winerror::SUCCEEDED,
    },
    Interface,
};

/// More adapters than any computer has; ends the loop if DXGI never
/// reports the end of its list.
const MAX_ADAPTERS: u32 = 64;
/// How long the load uses one list of adapters and their LUIDs. A driver
/// update gives an adapter a new LUID.
const ADAPTERS_FOR: Duration = Duration::from_secs(10);
/// An adapter with less dedicated memory than this shares the main memory:
/// its memory in use is the dedicated and the shared memory it uses, as
/// Task Manager counts it. A discrete GPU's is its dedicated memory only.
const DISCRETE_VRAM: u64 = 512 << 20;

struct Adapter {
    info: GpuInfo,
    luid: Luid,
}

pub fn inventory() -> Vec<GpuInfo> {
    adapters().into_iter().map(|adapter| adapter.info).collect()
}

/// The load of the adapters, in the order of `inventory`.
pub fn load(inventory: &[GpuInfo]) -> Vec<Measured> {
    static ADAPTERS: Mutex<Option<(Instant, Vec<Adapter>)>> = Mutex::new(None);
    let counters = counters::sample();
    let mut cached = ADAPTERS.lock().unwrap_or_else(PoisonError::into_inner);
    let fresh = cached
        .as_ref()
        .is_some_and(|(read, _)| read.elapsed() < ADAPTERS_FOR);
    if !fresh {
        *cached = Some((Instant::now(), adapters()));
    }
    let listed = cached.as_ref().map(|(_, adapters)| adapters.as_slice());
    let samples: Vec<(String, Measured)> = (listed.unwrap_or_default().iter())
        .map(|adapter| (adapter.info.name.clone(), measure(adapter, &counters)))
        .collect();
    super::by_name(inventory, &samples)
}

fn measure(adapter: &Adapter, counters: &counters::Sample) -> Measured {
    let luid = &adapter.luid;
    let dedicated = (counters.dedicated.as_ref()).map(|map| map.get(luid).copied().unwrap_or(0));
    let shared = (counters.shared.as_ref()).map(|map| map.get(luid).copied().unwrap_or(0));
    let vram_used = if adapter.info.vram >= DISCRETE_VRAM {
        dedicated
    } else {
        match (dedicated, shared) {
            (None, None) => None,
            (dedicated, shared) => Some(dedicated.unwrap_or(0) + shared.unwrap_or(0)),
        }
    };
    Measured {
        // The counters have an instance for every process that uses an
        // engine: an adapter without one is idle.
        load: (counters.engines.as_ref()).map(|map| map.get(luid).copied().unwrap_or(0.0)),
        vram_used,
    }
}

fn adapters() -> Vec<Adapter> {
    let mut factory: *mut IDXGIFactory1 = ptr::null_mut();
    // SAFETY: CreateDXGIFactory1 writes one interface pointer, which is
    // released below.
    let created = unsafe {
        CreateDXGIFactory1(
            &IDXGIFactory1::uuidof(),
            (&mut factory as *mut *mut IDXGIFactory1).cast(),
        )
    };
    if !SUCCEEDED(created) || factory.is_null() {
        return Vec::new();
    }
    let mut adapters = Vec::new();
    for index in 0..MAX_ADAPTERS {
        let mut adapter: *mut IDXGIAdapter1 = ptr::null_mut();
        // SAFETY: the factory is live; EnumAdapters1 writes one interface
        // pointer or fails at the end of the list.
        let found = unsafe { (*factory).EnumAdapters1(index, &mut adapter) };
        if !SUCCEEDED(found) || adapter.is_null() {
            break;
        }
        // SAFETY: the adapter is live and is released here.
        unsafe {
            if let Some(info) = describe(adapter) {
                adapters.push(info);
            }
            (*adapter).Release();
        }
    }
    // SAFETY: the factory came from CreateDXGIFactory1 above.
    unsafe { (*factory).Release() };
    adapters
}

/// The adapter's description, or None for a software adapter such as the
/// Microsoft Basic Render Driver.
///
/// # Safety
/// `adapter` must be a live IDXGIAdapter1.
unsafe fn describe(adapter: *mut IDXGIAdapter1) -> Option<Adapter> {
    let mut description: DXGI_ADAPTER_DESC1 = std::mem::zeroed();
    if !SUCCEEDED((*adapter).GetDesc1(&mut description)) {
        return None;
    }
    if description.Flags & DXGI_ADAPTER_FLAG_SOFTWARE != 0 {
        return None;
    }
    let length = description
        .Description
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(description.Description.len());
    let info = GpuInfo {
        name: String::from_utf16_lossy(&description.Description[..length])
            .trim()
            .to_owned(),
        vendor: vendor_name(description.VendorId).to_owned(),
        vram: description.DedicatedVideoMemory as u64,
        driver: String::new(),
    };
    let luid = Luid {
        high: description.AdapterLuid.HighPart as u32,
        low: description.AdapterLuid.LowPart,
    };
    Some(Adapter { info, luid })
}

fn vendor_name(vendor: u32) -> &'static str {
    match vendor {
        0x10de => "NVIDIA",
        0x1002 | 0x1022 => "AMD",
        0x8086 => "Intel",
        0x1414 => "Microsoft",
        0x5143 | 0x4d4f_4351 => "Qualcomm",
        0x15ad => "VMware",
        0x1af4 => "Red Hat (virtio)",
        0x80ee => "VirtualBox",
        _ => "",
    }
}

/// One PDH query with the three GPU counters, opened at the first sample
/// and kept: a rate such as the utilization needs two collections, and
/// each later sample covers the time since the one before.
mod counters {
    use crate::gpu::engines::{self, Luid};
    use std::{
        collections::HashMap,
        ptr,
        sync::{Mutex, PoisonError},
    };
    use winapi::{
        shared::minwindef::DWORD,
        um::pdh::{
            PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData,
            PdhGetFormattedCounterArrayW, PdhOpenQueryW, PDH_FMT_COUNTERVALUE,
            PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
        },
    };

    const ENGINES: &str = r"\GPU Engine(*)\Utilization Percentage";
    const DEDICATED: &str = r"\GPU Adapter Memory(*)\Dedicated Usage";
    const SHARED: &str = r"\GPU Adapter Memory(*)\Shared Usage";
    const SUCCESS: i32 = 0;
    const MORE_DATA: i32 = 0x8000_07D2_u32 as i32;
    const VALID_DATA: DWORD = 0;
    const NEW_DATA: DWORD = 1;
    /// Tries at reading a counter whose instances grow between the call
    /// that sizes the buffer and the call that fills it.
    const TRIES: usize = 3;

    /// What one collection tells, per adapter. A counter that is missing
    /// or has no data yet is None.
    #[derive(Default)]
    pub struct Sample {
        pub engines: Option<HashMap<Luid, f32>>,
        pub dedicated: Option<HashMap<Luid, u64>>,
        pub shared: Option<HashMap<Luid, u64>>,
    }

    struct Query {
        query: PDH_HQUERY,
        engines: Option<PDH_HCOUNTER>,
        dedicated: Option<PDH_HCOUNTER>,
        shared: Option<PDH_HCOUNTER>,
        collected: bool,
    }

    // SAFETY: PDH handles may be used from any thread; the mutex below
    // keeps one user at a time.
    unsafe impl Send for Query {}

    impl Drop for Query {
        fn drop(&mut self) {
            // SAFETY: the query came from PdhOpenQueryW; closing it frees
            // its counters too.
            unsafe { PdhCloseQuery(self.query) };
        }
    }

    /// The counters now. The query opens at the first call; if Windows
    /// has none of the counters, no later call tries again.
    pub fn sample() -> Sample {
        static QUERY: Mutex<Option<Option<Query>>> = Mutex::new(None);
        let mut query = QUERY.lock().unwrap_or_else(PoisonError::into_inner);
        match query.get_or_insert_with(Query::open) {
            Some(query) => query.collect(),
            None => Sample::default(),
        }
    }

    impl Query {
        fn open() -> Option<Self> {
            let mut query: PDH_HQUERY = ptr::null_mut();
            // SAFETY: PdhOpenQueryW writes one handle, closed by Drop.
            if unsafe { PdhOpenQueryW(ptr::null(), 0, &mut query) } != SUCCESS {
                return None;
            }
            let opened = Self {
                query,
                engines: counter(query, ENGINES),
                dedicated: counter(query, DEDICATED),
                shared: counter(query, SHARED),
                collected: false,
            };
            let any = opened.engines.or(opened.dedicated).or(opened.shared);
            any.is_some().then_some(opened)
        }

        fn collect(&mut self) -> Sample {
            // SAFETY: the query is open.
            if unsafe { PdhCollectQueryData(self.query) } != SUCCESS {
                return Sample::default();
            }
            let first = !self.collected;
            self.collected = true;
            let engines = self.engines.filter(|_| !first).and_then(|counter| {
                let items = array(counter, PDH_FMT_DOUBLE)?;
                // SAFETY: the values were formatted as doubles.
                let busy = items
                    .iter()
                    .map(|(name, value)| unsafe { (name.as_str(), *value.u.doubleValue()) });
                Some(engines::utilization(busy))
            });
            let bytes = |counter: Option<PDH_HCOUNTER>| {
                let items = array(counter?, PDH_FMT_LARGE)?;
                // SAFETY: the values were formatted as 64-bit integers.
                let bytes = items
                    .iter()
                    .map(|(name, value)| unsafe { (name.as_str(), *value.u.largeValue()) });
                Some(engines::memory(bytes))
            };
            Sample {
                engines,
                dedicated: bytes(self.dedicated),
                shared: bytes(self.shared),
            }
        }
    }

    fn counter(query: PDH_HQUERY, path: &str) -> Option<PDH_HCOUNTER> {
        let path: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let mut counter: PDH_HCOUNTER = ptr::null_mut();
        // SAFETY: the path ends in a zero; PdhAddEnglishCounterW writes
        // one handle, which belongs to the query.
        let added = unsafe { PdhAddEnglishCounterW(query, path.as_ptr(), 0, &mut counter) };
        (added == SUCCESS && !counter.is_null()).then_some(counter)
    }

    /// The valid values of every instance of a wildcard counter.
    fn array(counter: PDH_HCOUNTER, format: DWORD) -> Option<Vec<(String, PDH_FMT_COUNTERVALUE)>> {
        for _ in 0..TRIES {
            let mut size: DWORD = 0;
            let mut count: DWORD = 0;
            // SAFETY: with a size of zero, the call only tells the size.
            let sized = unsafe {
                PdhGetFormattedCounterArrayW(
                    counter,
                    format,
                    &mut size,
                    &mut count,
                    ptr::null_mut(),
                )
            };
            if sized == SUCCESS {
                return Some(Vec::new());
            }
            if sized != MORE_DATA {
                return None;
            }
            // Eight-byte units keep the items aligned; the names follow
            // them in the same buffer.
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            // SAFETY: the buffer holds `size` bytes.
            let filled = unsafe {
                PdhGetFormattedCounterArrayW(
                    counter,
                    format,
                    &mut size,
                    &mut count,
                    buffer.as_mut_ptr().cast(),
                )
            };
            if filled == MORE_DATA {
                continue;
            }
            if filled != SUCCESS {
                return None;
            }
            // SAFETY: PDH wrote `count` items at the start of the buffer,
            // whose names point into the same buffer.
            let items = unsafe {
                std::slice::from_raw_parts(
                    buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                    count as usize,
                )
            };
            let valid = items
                .iter()
                .filter(|item| matches!(item.FmtValue.CStatus, VALID_DATA | NEW_DATA))
                // SAFETY: each name is a zero-terminated string in the buffer.
                .map(|item| (unsafe { wide(item.szName) }, item.FmtValue))
                .collect();
            return Some(valid);
        }
        None
    }

    /// # Safety
    /// `text` must be null or a zero-terminated UTF-16 string.
    unsafe fn wide(text: *const u16) -> String {
        if text.is_null() {
            return String::new();
        }
        let length = (0..).take_while(|at| *text.add(*at) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
    }
}

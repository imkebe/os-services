//! The display adapters, from what each OS tells a normal user: sysfs on
//! Linux, `system_profiler` on macOS, DXGI on Windows. Other systems list
//! none.
//!
//! The load and the video memory in use come from sources a normal user
//! may read as well: sysfs and `nvidia-smi` on Linux, the IOKit
//! accelerators' performance statistics on macOS, the GPU performance
//! counters on Windows. Each costs well under a millisecond per sample,
//! apart from `nvidia-smi`, which runs on its own thread at most every two
//! seconds.

use crate::model::{GpuInfo, GpuLoad};

#[cfg(any(target_os = "linux", test))]
mod background;
#[cfg(any(windows, test))]
mod engines;
#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;
#[cfg(windows)]
mod windows;

/// What one adapter tells of its load: percent busy and video memory in
/// use, in bytes. Either is None when no unprivileged source tells it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measured {
    pub load: Option<f32>,
    pub vram_used: Option<u64>,
}

/// The adapters for the inventory.
pub fn inventory() -> Vec<GpuInfo> {
    #[cfg(target_os = "linux")]
    return linux::inventory(std::path::Path::new(linux::DRM));
    #[cfg(target_os = "macos")]
    return macos::inventory();
    #[cfg(windows)]
    return windows::inventory();
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    Vec::new()
}

/// The adapters' load for the metrics: the adapters of `inventory`, in its
/// order. The load and the memory in use are `null` where the system does
/// not tell them.
pub fn load(adapters: &[GpuInfo]) -> Vec<GpuLoad> {
    #[cfg(target_os = "linux")]
    let measured = linux::load(std::path::Path::new(linux::DRM));
    #[cfg(target_os = "macos")]
    let measured = macos::load(adapters);
    #[cfg(windows)]
    let measured = windows::load(adapters);
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let measured: Vec<Measured> = Vec::new();
    adapters
        .iter()
        .enumerate()
        .map(|(index, adapter)| {
            let measured = measured.get(index).copied().unwrap_or_default();
            GpuLoad {
                name: adapter.name.clone(),
                load: measured.load,
                vram_used: measured.vram_used,
            }
        })
        .collect()
}

/// The samples of the adapters, in the order of `adapters`. A sample
/// belongs to the adapter of the same name; two adapters of one name take
/// their samples in order. A sample without a name takes the adapter at its
/// own position, and a single sample always belongs to a single adapter.
#[cfg(any(target_os = "macos", windows, test))]
pub fn by_name(adapters: &[GpuInfo], samples: &[(String, Measured)]) -> Vec<Measured> {
    if let ([_], [(_, only)]) = (adapters, samples) {
        return vec![*only];
    }
    let mut taken = vec![false; samples.len()];
    adapters
        .iter()
        .enumerate()
        .map(|(index, adapter)| {
            let wanted = adapter.name.trim();
            let named = samples.iter().enumerate().position(|(at, (name, _))| {
                !taken[at] && !wanted.is_empty() && name.trim().eq_ignore_ascii_case(wanted)
            });
            let found = named.or_else(|| {
                let (name, _) = samples.get(index)?;
                (!taken[index] && name.trim().is_empty()).then_some(index)
            });
            found.map_or_else(Measured::default, |at| {
                taken[at] = true;
                samples[at].1
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(name: &str) -> GpuInfo {
        GpuInfo {
            name: name.into(),
            ..GpuInfo::default()
        }
    }

    fn sample(name: &str, load: f32) -> (String, Measured) {
        let measured = Measured {
            load: Some(load),
            vram_used: None,
        };
        (name.into(), measured)
    }

    #[test]
    fn samples_follow_the_adapters_by_name() {
        let adapters = [adapter("Intel UHD 630"), adapter("AMD Radeon Pro 5500M")];
        let samples = [sample("amd radeon pro 5500m", 40.0), sample("Other", 1.0)];
        let loads: Vec<_> = (by_name(&adapters, &samples).iter())
            .map(|measured| measured.load)
            .collect();
        assert_eq!(loads, [None, Some(40.0)]);
    }

    #[test]
    fn twin_adapters_take_their_samples_in_order() {
        let adapters = [adapter("RTX 4090"), adapter("RTX 4090")];
        let samples = [sample("RTX 4090", 10.0), sample("RTX 4090", 90.0)];
        let loads: Vec<_> = (by_name(&adapters, &samples).iter())
            .map(|measured| measured.load)
            .collect();
        assert_eq!(loads, [Some(10.0), Some(90.0)]);
    }

    #[test]
    fn unnamed_samples_go_by_position_and_one_fits_one() {
        let adapters = [adapter("A"), adapter("B")];
        let samples = [sample("", 5.0), sample("", 6.0)];
        let loads: Vec<_> = (by_name(&adapters, &samples).iter())
            .map(|measured| measured.load)
            .collect();
        assert_eq!(loads, [Some(5.0), Some(6.0)]);
        let one = by_name(&[adapter("Apple M2 Pro")], &[sample("AGX", 3.0)]);
        assert_eq!(one[0].load, Some(3.0));
        assert!(by_name(&[], &[sample("AGX", 3.0)]).is_empty());
    }
}

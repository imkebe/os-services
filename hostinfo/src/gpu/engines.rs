//! The instances of the Windows GPU performance counters, and the sums that
//! Task Manager shows. `GPU Engine` has one instance per process and engine,
//! such as `pid_1234_luid_0x00000000_0x0000D1F2_phys_0_eng_3_engtype_3D`;
//! `GPU Adapter Memory` one per adapter, such as
//! `luid_0x00000000_0x0000D1F2_phys_0`. The LUID names the adapter, as DXGI
//! does.

use std::collections::HashMap;

/// An adapter's LUID, as the counters write it: the high part first.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct Luid {
    pub high: u32,
    pub low: u32,
}

/// The LUID in an instance name.
pub fn luid(instance: &str) -> Option<Luid> {
    let at = instance.find("luid_")?;
    let mut parts = instance[at + "luid_".len()..].split('_');
    let high = hex(parts.next()?)?;
    let low = hex(parts.next()?)?;
    Some(Luid { high, low })
}

fn hex(text: &str) -> Option<u32> {
    let digits = text.strip_prefix("0x").or(text.strip_prefix("0X"))?;
    u32::from_str_radix(digits, 16).ok()
}

/// The adapter and the engine, such as `phys_0_eng_3`, of a `GPU Engine`
/// instance.
fn engine(instance: &str) -> Option<(Luid, &str)> {
    let luid = luid(instance)?;
    let start = instance.find("_phys_")? + 1;
    let end = (instance[start..].find("_engtype")).map_or(instance.len(), |end| start + end);
    Some((luid, &instance[start..end]))
}

/// The load of each adapter from the `Utilization Percentage` of every
/// `GPU Engine` instance: each engine's use summed over the processes, and
/// the adapter as busy as its busiest engine, as in Task Manager.
pub fn utilization<'a>(items: impl IntoIterator<Item = (&'a str, f64)>) -> HashMap<Luid, f32> {
    let mut engines: HashMap<(Luid, &str), f64> = HashMap::new();
    for (instance, busy) in items {
        if let Some(key) = engine(instance).filter(|_| busy.is_finite() && busy >= 0.0) {
            *engines.entry(key).or_default() += busy;
        }
    }
    let mut adapters: HashMap<Luid, f32> = HashMap::new();
    for ((luid, _), busy) in engines {
        let busy = busy.min(100.0) as f32;
        let adapter = adapters.entry(luid).or_default();
        *adapter = adapter.max(busy);
    }
    adapters
}

/// The bytes of each adapter from a `GPU Adapter Memory` counter, summed
/// over the adapter's physical parts.
pub fn memory<'a>(items: impl IntoIterator<Item = (&'a str, i64)>) -> HashMap<Luid, u64> {
    let mut adapters: HashMap<Luid, u64> = HashMap::new();
    for (instance, bytes) in items {
        if let (Some(luid), Ok(bytes)) = (luid(instance), u64::try_from(bytes)) {
            *adapters.entry(luid).or_default() += bytes;
        }
    }
    adapters
}

#[cfg(test)]
mod tests {
    use super::*;

    const DGPU: Luid = Luid {
        high: 0,
        low: 0xd1f2,
    };
    const IGPU: Luid = Luid {
        high: 1,
        low: 0xabc,
    };

    #[test]
    fn reads_the_luid_of_an_instance() {
        assert_eq!(luid("luid_0x00000000_0x0000D1F2_phys_0"), Some(DGPU));
        let instance = "pid_42_luid_0x00000001_0x00000ABC_phys_0_eng_0_engtype_3D";
        assert_eq!(luid(instance), Some(IGPU));
        assert_eq!(luid("pid_42_eng_0"), None);
        assert_eq!(luid("luid_12_34"), None);
    }

    #[test]
    fn an_adapter_is_as_busy_as_its_busiest_engine() {
        let items = [
            (
                "pid_1_luid_0x00000000_0x0000D1F2_phys_0_eng_0_engtype_3D",
                30.0,
            ),
            (
                "pid_2_luid_0x00000000_0x0000D1F2_phys_0_eng_0_engtype_3D",
                25.0,
            ),
            (
                "pid_2_luid_0x00000000_0x0000D1F2_phys_0_eng_5_engtype_VideoDecode",
                40.0,
            ),
            (
                "pid_3_luid_0x00000001_0x00000ABC_phys_0_eng_1_engtype_Copy",
                80.0,
            ),
            (
                "pid_4_luid_0x00000001_0x00000ABC_phys_0_eng_1_engtype_Copy",
                70.0,
            ),
            (
                "pid_5_luid_0x00000001_0x00000ABC_phys_0_eng_0_engtype_3D",
                f64::NAN,
            ),
            ("not an instance", 99.0),
        ];
        let adapters = utilization(items);
        assert_eq!(adapters.len(), 2);
        assert_eq!(adapters[&DGPU], 55.0);
        assert_eq!(adapters[&IGPU], 100.0);
    }

    #[test]
    fn sums_the_memory_of_an_adapter() {
        let items = [
            ("luid_0x00000000_0x0000D1F2_phys_0", 1024),
            ("luid_0x00000000_0x0000D1F2_phys_1", 1024),
            ("luid_0x00000001_0x00000ABC_phys_0", -1),
        ];
        let adapters = memory(items);
        assert_eq!(adapters.get(&DGPU), Some(&2048));
        assert_eq!(adapters.get(&IGPU), None);
    }
}

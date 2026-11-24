//! How much linear memory each plugin store holds, readable without locking
//! the plugin.
//!
//! A plugin's `RunningPlugin` mutex is held for each whole `update` and
//! `render` call, and a worker's store lives inside its task, so reading their
//! memory directly would wait on a busy plugin. Instead, the store's resource
//! limiter, which wasmi asks before it allocates or grows a memory, keeps a
//! gauge that diagnostics read without a lock.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use wasmi::{ResourceLimiter, StoreLimits};
use wasmi_core::LimiterError;

use super::PluginId;

/// A store's resource limits, plus a gauge of the linear memory it holds.
pub struct TrackedStoreLimits {
    limits: StoreLimits,
    linear_memory: Arc<AtomicU64>,
    /// The growth approved last. wasmi can still fail a growth after the
    /// limiter allows it, and then calls `memory_grow_failed`.
    approved_growth: u64,
}

impl TrackedStoreLimits {
    pub fn new(limits: StoreLimits) -> Self {
        TrackedStoreLimits {
            limits,
            linear_memory: Arc::new(AtomicU64::new(0)),
            approved_growth: 0,
        }
    }

    /// A handle on the gauge that stops reading once the store is dropped.
    pub fn gauge(&self) -> LinearMemoryGauge {
        LinearMemoryGauge(Arc::downgrade(&self.linear_memory))
    }
}

impl ResourceLimiter for TrackedStoreLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, LimiterError> {
        let allowed = self.limits.memory_growing(current, desired, maximum)?;
        if allowed {
            self.approved_growth = desired.saturating_sub(current) as u64;
            self.linear_memory
                .fetch_add(self.approved_growth, Ordering::Relaxed);
        }
        Ok(allowed)
    }

    fn memory_grow_failed(&mut self, error: &LimiterError) {
        self.linear_memory
            .fetch_sub(self.approved_growth, Ordering::Relaxed);
        self.approved_growth = 0;
        self.limits.memory_grow_failed(error);
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, LimiterError> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn table_grow_failed(&mut self, error: &LimiterError) {
        self.limits.table_grow_failed(error);
    }

    fn instances(&self) -> usize {
        self.limits.instances()
    }

    fn tables(&self) -> usize {
        self.limits.tables()
    }

    fn memories(&self) -> usize {
        self.limits.memories()
    }
}

/// Reads one store's linear memory.
#[derive(Debug, Clone)]
pub struct LinearMemoryGauge(Weak<AtomicU64>);

impl LinearMemoryGauge {
    /// `None` once the store is gone.
    pub fn bytes(&self) -> Option<u64> {
        self.0
            .upgrade()
            .map(|linear_memory| linear_memory.load(Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreRole {
    /// The plugin itself, for one client or shared by all.
    Instance,
    Worker,
}

/// Linear memory of one plugin, across its stores.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginLinearMemory {
    pub instances: u32,
    pub workers: u32,
    pub total_bytes: u64,
    pub largest_bytes: u64,
}

/// The gauges of every live plugin store, by plugin. A store's entry goes
/// away by itself once the store is dropped, so unloading a plugin needs no
/// bookkeeping here.
#[derive(Debug, Default)]
pub struct LinearMemoryGauges {
    gauges: Vec<(PluginId, StoreRole, LinearMemoryGauge)>,
}

impl LinearMemoryGauges {
    pub fn register(&mut self, plugin_id: PluginId, role: StoreRole, gauge: LinearMemoryGauge) {
        self.prune();
        self.gauges.push((plugin_id, role, gauge));
    }

    fn prune(&mut self) {
        self.gauges.retain(|(_, _, gauge)| gauge.bytes().is_some());
    }

    pub fn by_plugin(&mut self) -> BTreeMap<PluginId, PluginLinearMemory> {
        self.prune();
        let mut by_plugin: BTreeMap<PluginId, PluginLinearMemory> = BTreeMap::new();
        for (plugin_id, role, gauge) in &self.gauges {
            let Some(bytes) = gauge.bytes() else {
                continue;
            };
            let plugin = by_plugin.entry(*plugin_id).or_default();
            match role {
                StoreRole::Instance => plugin.instances += 1,
                StoreRole::Worker => plugin.workers += 1,
            }
            plugin.total_bytes += bytes;
            plugin.largest_bytes = plugin.largest_bytes.max(bytes);
        }
        by_plugin
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmi::StoreLimitsBuilder;

    const PAGE: usize = 64 * 1024;

    fn limited_to(bytes: usize) -> TrackedStoreLimits {
        TrackedStoreLimits::new(StoreLimitsBuilder::new().memory_size(bytes).build())
    }

    #[test]
    fn the_gauge_follows_allowed_growth() {
        let mut limits = limited_to(16 * PAGE);
        let gauge = limits.gauge();
        assert!(limits.memory_growing(0, 2 * PAGE, None).unwrap());
        assert!(limits.memory_growing(2 * PAGE, 5 * PAGE, None).unwrap());
        assert_eq!(gauge.bytes(), Some(5 * PAGE as u64));
    }

    #[test]
    fn denied_growth_is_not_counted() {
        let mut limits = limited_to(4 * PAGE);
        let gauge = limits.gauge();
        assert!(limits.memory_growing(0, 2 * PAGE, None).unwrap());
        assert!(!limits.memory_growing(2 * PAGE, 8 * PAGE, None).unwrap());
        assert_eq!(gauge.bytes(), Some(2 * PAGE as u64));
    }

    #[test]
    fn growth_that_fails_after_approval_is_taken_back() {
        let mut limits = limited_to(16 * PAGE);
        let gauge = limits.gauge();
        assert!(limits.memory_growing(0, 2 * PAGE, None).unwrap());
        assert!(limits.memory_growing(2 * PAGE, 6 * PAGE, None).unwrap());
        limits.memory_grow_failed(&LimiterError::OutOfSystemMemory);
        assert_eq!(gauge.bytes(), Some(2 * PAGE as u64));
    }

    #[test]
    fn a_real_store_reports_its_memory() {
        use wasmi::{Engine, Linker, Module, Store};
        // (module (memory 3) (func (export "grow") (drop (memory.grow (i32.const 2)))))
        let wasm: &[u8] = &[
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // type: () -> ()
            0x03, 0x02, 0x01, 0x00, // func 0 has type 0
            0x05, 0x03, 0x01, 0x00, 0x03, // memory: min 3 pages
            0x07, 0x08, 0x01, 0x04, b'g', b'r', b'o', b'w', 0x00, 0x00, // export "grow"
            0x0a, 0x09, 0x01, 0x07, 0x00, 0x41, 0x02, 0x40, 0x00, 0x1a, 0x0b, // code
        ];
        let engine = Engine::default();
        let module = Module::new(&engine, wasm).expect("valid module");
        let mut store = Store::new(&engine, limited_to(16 * PAGE));
        store.limiter(|limits| limits);
        let gauge = store.data().gauge();
        let instance = Linker::new(&engine)
            .instantiate_and_start(&mut store, &module)
            .expect("instantiates");
        assert_eq!(gauge.bytes(), Some(3 * PAGE as u64));
        instance
            .get_typed_func::<(), ()>(&store, "grow")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        assert_eq!(gauge.bytes(), Some(5 * PAGE as u64));
        drop(store);
        assert_eq!(gauge.bytes(), None, "the store is gone");
    }

    #[test]
    fn gauges_are_summed_by_plugin_and_forgotten_with_their_store() {
        let mut gauges = LinearMemoryGauges::default();
        let mut instance = limited_to(16 * PAGE);
        let mut worker = limited_to(16 * PAGE);
        let mut other_client = limited_to(16 * PAGE);
        instance.memory_growing(0, 4 * PAGE, None).unwrap();
        worker.memory_growing(0, 2 * PAGE, None).unwrap();
        other_client.memory_growing(0, 3 * PAGE, None).unwrap();
        gauges.register(1, StoreRole::Instance, instance.gauge());
        gauges.register(1, StoreRole::Worker, worker.gauge());
        gauges.register(1, StoreRole::Instance, other_client.gauge());
        assert_eq!(
            gauges.by_plugin().get(&1),
            Some(&PluginLinearMemory {
                instances: 2,
                workers: 1,
                total_bytes: 9 * PAGE as u64,
                largest_bytes: 4 * PAGE as u64,
            })
        );
        drop(instance);
        drop(worker);
        drop(other_client);
        assert!(gauges.by_plugin().is_empty());
        assert!(gauges.gauges.is_empty(), "dead gauges are pruned");
    }
}

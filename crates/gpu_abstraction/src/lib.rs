//! SHER GPU Abstraction Layer
//!
//! Extends `hal::HardwareDriver` with GPU-specific operations: VRAM
//! allocation, command submission, DMA mapping, and fault reporting.
//! Implemented either by native `gpu_driver`-backed drivers (Phase B of the
//! migration strategy) or by an LKI-hosted Linux kernel driver behind a
//! DRM-ioctl-compatible shim (Phase A) — see ARCHITECTURE.md section 8.

use graphics_api::{MemoryClass, WorkloadClass};
use hal::{DeviceInfo, DeviceType, HardwareDriver};
use sher_common::{Error, ObjectId, Result};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct GpuAllocation {
    pub id: ObjectId,
    pub device: ObjectId,
    pub class: MemoryClass,
    pub size: usize,
    pub device_address: u64,
}

#[derive(Debug, Clone)]
pub struct DmaMapping {
    pub allocation: ObjectId,
    pub iova: u64,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct GpuFault {
    pub device: ObjectId,
    pub address: u64,
    pub description: String,
}

/// A single driver-facing command. Narrower than `graphics_api::GraphicsOp`
/// on purpose: `graphics_runtime` translates the app-facing vocabulary into
/// this shape once, so driver backends never need to understand pipeline
/// binding, resource handles, etc. — only what to execute.
#[derive(Debug, Clone)]
pub enum DriverOp {
    Draw {
        vertex_count: u32,
        instance_count: u32,
    },
    Dispatch {
        x: u32,
        y: u32,
        z: u32,
    },
    Copy {
        src: ObjectId,
        dst: ObjectId,
    },
    Barrier,
}

pub trait GpuDriver: HardwareDriver {
    fn allocate_vram(
        &mut self,
        device: &ObjectId,
        size: usize,
        class: MemoryClass,
    ) -> Result<GpuAllocation>;
    fn free_vram(&mut self, allocation: &ObjectId) -> Result<()>;
    fn map_dma(&mut self, allocation: &ObjectId) -> Result<DmaMapping>;
    fn submit(&mut self, device: &ObjectId, class: WorkloadClass, ops: &[DriverOp]) -> Result<()>;
    fn fault_status(&self, device: &ObjectId) -> Result<Option<GpuFault>>;
    fn vram_available(&self, device: &ObjectId) -> Result<usize>;
    /// Clears fault state and makes the device submittable again. Per
    /// ARCHITECTURE.md section 18, recovery isolates and restarts the
    /// faulting context rather than resetting the whole GPU where avoidable
    /// — this method is the driver-level half of that; `graphics_runtime`
    /// pairs it with tearing down the runtime-level `GraphicsDevice`.
    fn reset_device(&mut self, device: &ObjectId) -> Result<()>;

    /// Record that `device` has faulted, with `description` explaining why.
    /// `graphics_runtime::submit` calls this when a driver operation panics
    /// (caught via `catch_unwind` rather than left to unwind through the
    /// caller — see that module's doc comment on driver crash containment)
    /// instead of a hardware-reported fault, so a caught driver panic and a
    /// real fault surface through the exact same `fault_status`/
    /// `reset_device`/`recover_device` path: callers don't need a separate
    /// "did it panic vs. did it report a fault" case.
    fn mark_faulted(&mut self, device: &ObjectId, description: String);
}

struct DeviceState {
    info: DeviceInfo,
    initialized: bool,
    vram_total: usize,
    vram_available: usize,
    fault: Option<GpuFault>,
}

/// Reference, hardware-independent implementation. Plays the same role for
/// SHER Graphics that `llvmpipe`/`lavapipe` play for Mesa: a correctness
/// baseline that exercises the full stack — `graphics_api` →
/// `graphics_runtime` → `gpu_abstraction` — with no real GPU behind it.
/// See ARCHITECTURE.md section 21, "what's realistically implementable
/// first."
///
/// Supports multiple independent synthetic devices (ARCHITECTURE.md section
/// 17, multi-GPU): each has its own VRAM pool and fault state, keyed by the
/// `ObjectId` `hal`-style device discovery would assign it. Nothing in
/// `graphics_runtime` or `graphics_api` assumes a single device — that
/// assumption lived only here, in the reference driver's original
/// single-`DeviceInfo` field, now replaced by a device table.
pub struct SoftwareGpuDriver {
    devices: HashMap<ObjectId, DeviceState>,
    allocations: HashMap<ObjectId, GpuAllocation>,
    registers: HashMap<usize, u32>,
}

impl SoftwareGpuDriver {
    /// Single-device convenience constructor.
    pub fn new(vram_bytes: usize) -> Self {
        Self::with_devices(1, vram_bytes)
    }

    /// `count` independent devices, each with its own `vram_bytes_per_device`
    /// pool. Allocating on one device never affects another's VRAM, and a
    /// fault on one never blocks submission on another.
    pub fn with_devices(count: usize, vram_bytes_per_device: usize) -> Self {
        let mut devices = HashMap::new();
        for i in 0..count {
            let info = DeviceInfo {
                id: ObjectId::new(),
                device_type: DeviceType::Gpu,
                name: format!("SHER Software GPU {i}"),
                vendor_id: 0x0000,
                device_id: 0x0000,
                capabilities: vec![
                    "graphics".to_string(),
                    "compute".to_string(),
                    "transfer".to_string(),
                ],
                mmio_base: None,
                mmio_size: None,
                irq: None,
            };
            devices.insert(
                info.id,
                DeviceState {
                    info,
                    initialized: false,
                    vram_total: vram_bytes_per_device,
                    vram_available: vram_bytes_per_device,
                    fault: None,
                },
            );
        }
        Self {
            devices,
            allocations: HashMap::new(),
            registers: HashMap::new(),
        }
    }

    /// The first device's id. Convenience for the common single-device case
    /// (tests, the `triangle` example); use `device_ids()` when there is
    /// more than one. Panics if constructed with zero devices.
    pub fn device_id(&self) -> ObjectId {
        *self
            .devices
            .keys()
            .next()
            .expect("SoftwareGpuDriver constructed with zero devices")
    }

    pub fn device_ids(&self) -> Vec<ObjectId> {
        self.devices.keys().copied().collect()
    }

    /// Test/diagnostic hook for exercising fault recovery (ARCHITECTURE.md
    /// section 18) without needing real hardware to actually hang.
    pub fn inject_fault(
        &mut self,
        device: &ObjectId,
        address: u64,
        description: impl Into<String>,
    ) {
        if let Some(state) = self.devices.get_mut(device) {
            state.fault = Some(GpuFault {
                device: *device,
                address,
                description: description.into(),
            });
        }
    }

    pub fn clear_fault(&mut self, device: &ObjectId) {
        if let Some(state) = self.devices.get_mut(device) {
            state.fault = None;
        }
    }

    fn state(&self, device_id: &ObjectId) -> Result<&DeviceState> {
        self.devices
            .get(device_id)
            .ok_or_else(|| Error::Device("unknown GPU device".to_string()))
    }

    fn state_mut(&mut self, device_id: &ObjectId) -> Result<&mut DeviceState> {
        self.devices
            .get_mut(device_id)
            .ok_or_else(|| Error::Device("unknown GPU device".to_string()))
    }
}

impl HardwareDriver for SoftwareGpuDriver {
    fn probe(&self) -> Result<Vec<DeviceInfo>> {
        Ok(self.devices.values().map(|s| s.info.clone()).collect())
    }

    fn initialize(&mut self, device: &DeviceInfo) -> Result<()> {
        self.state_mut(&device.id)?.initialized = true;
        Ok(())
    }

    fn shutdown(&mut self, device_id: &ObjectId) -> Result<()> {
        self.state_mut(device_id)?.initialized = false;
        Ok(())
    }

    fn get_capabilities(&self, device_id: &ObjectId) -> Result<Vec<String>> {
        Ok(self.state(device_id)?.info.capabilities.clone())
    }

    fn read_register(&self, device_id: &ObjectId, offset: usize) -> Result<u32> {
        self.state(device_id)?;
        Ok(*self.registers.get(&offset).unwrap_or(&0))
    }

    fn write_register(&self, device_id: &ObjectId, _offset: usize, _value: u32) -> Result<()> {
        self.state(device_id)?;
        // Registers are conceptually mutable hardware state; this reference
        // driver has none to persist, so writes are accepted and discarded.
        Ok(())
    }
}

impl GpuDriver for SoftwareGpuDriver {
    fn allocate_vram(
        &mut self,
        device: &ObjectId,
        size: usize,
        class: MemoryClass,
    ) -> Result<GpuAllocation> {
        let state = self.state_mut(device)?;
        if size > state.vram_available {
            return Err(Error::Memory("insufficient VRAM".to_string()));
        }
        let allocation = GpuAllocation {
            id: ObjectId::new(),
            device: *device,
            class,
            size,
            device_address: (state.vram_total - state.vram_available) as u64,
        };
        state.vram_available -= size;
        self.allocations.insert(allocation.id, allocation.clone());
        Ok(allocation)
    }

    fn free_vram(&mut self, allocation: &ObjectId) -> Result<()> {
        let allocation = self
            .allocations
            .remove(allocation)
            .ok_or_else(|| Error::Memory("unknown allocation".to_string()))?;
        self.state_mut(&allocation.device)?.vram_available += allocation.size;
        Ok(())
    }

    fn map_dma(&mut self, allocation: &ObjectId) -> Result<DmaMapping> {
        let allocation = self
            .allocations
            .get(allocation)
            .ok_or_else(|| Error::Memory("unknown allocation".to_string()))?;
        Ok(DmaMapping {
            allocation: allocation.id,
            iova: allocation.device_address,
            size: allocation.size,
        })
    }

    fn submit(&mut self, device: &ObjectId, _class: WorkloadClass, ops: &[DriverOp]) -> Result<()> {
        let state = self.state(device)?;
        if let Some(fault) = &state.fault {
            return Err(Error::Device(format!(
                "device faulted at {:#x}: {}",
                fault.address, fault.description
            )));
        }
        // Reference driver: validate and accept. A real backend would
        // translate `ops` into hardware ring-buffer commands here.
        let _ = ops;
        Ok(())
    }

    fn fault_status(&self, device: &ObjectId) -> Result<Option<GpuFault>> {
        Ok(self.state(device)?.fault.clone())
    }

    fn vram_available(&self, device: &ObjectId) -> Result<usize> {
        Ok(self.state(device)?.vram_available)
    }

    fn reset_device(&mut self, device: &ObjectId) -> Result<()> {
        self.state_mut(device)?.fault = None;
        Ok(())
    }

    fn mark_faulted(&mut self, device: &ObjectId, description: String) {
        self.inject_fault(device, 0, description);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_single_synthetic_device() {
        let driver = SoftwareGpuDriver::new(256 * 1024 * 1024);
        let devices = driver.probe().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_type, DeviceType::Gpu);
    }

    #[test]
    fn initialize_and_shutdown() {
        let mut driver = SoftwareGpuDriver::new(1024);
        let device = driver.probe().unwrap().remove(0);
        assert!(driver.initialize(&device).is_ok());
        assert!(driver.shutdown(&device.id).is_ok());
    }

    #[test]
    fn allocate_and_free_vram() {
        let mut driver = SoftwareGpuDriver::new(1024);
        let device_id = driver.device_id();
        let alloc = driver
            .allocate_vram(&device_id, 512, MemoryClass::DeviceLocal)
            .unwrap();
        assert_eq!(driver.vram_available(&device_id).unwrap(), 512);

        driver.free_vram(&alloc.id).unwrap();
        assert_eq!(driver.vram_available(&device_id).unwrap(), 1024);
    }

    #[test]
    fn allocation_beyond_vram_fails() {
        let mut driver = SoftwareGpuDriver::new(256);
        let device_id = driver.device_id();
        let result = driver.allocate_vram(&device_id, 512, MemoryClass::DeviceLocal);
        assert!(result.is_err());
    }

    #[test]
    fn map_dma_reflects_allocation() {
        let mut driver = SoftwareGpuDriver::new(4096);
        let device_id = driver.device_id();
        let alloc = driver
            .allocate_vram(&device_id, 1024, MemoryClass::HostVisible)
            .unwrap();
        let mapping = driver.map_dma(&alloc.id).unwrap();
        assert_eq!(mapping.size, 1024);
        assert_eq!(mapping.allocation, alloc.id);
    }

    #[test]
    fn submit_succeeds_without_fault() {
        let mut driver = SoftwareGpuDriver::new(4096);
        let device_id = driver.device_id();
        let ops = vec![DriverOp::Draw {
            vertex_count: 3,
            instance_count: 1,
        }];
        assert!(driver
            .submit(&device_id, WorkloadClass::Graphics, &ops)
            .is_ok());
    }

    #[test]
    fn submit_rejected_while_faulted() {
        let mut driver = SoftwareGpuDriver::new(4096);
        let device_id = driver.device_id();
        driver.inject_fault(&device_id, 0xdead_beef, "test hang");
        let ops = vec![DriverOp::Barrier];
        assert!(driver
            .submit(&device_id, WorkloadClass::Compute, &ops)
            .is_err());

        driver.clear_fault(&device_id);
        assert!(driver
            .submit(&device_id, WorkloadClass::Compute, &ops)
            .is_ok());
    }

    #[test]
    fn fault_status_reports_injected_fault() {
        let mut driver = SoftwareGpuDriver::new(4096);
        let device_id = driver.device_id();
        assert!(driver.fault_status(&device_id).unwrap().is_none());

        driver.inject_fault(&device_id, 0x1000, "page fault");
        let fault = driver.fault_status(&device_id).unwrap().unwrap();
        assert_eq!(fault.address, 0x1000);
    }

    #[test]
    fn reset_device_clears_fault_and_allows_submit() {
        let mut driver = SoftwareGpuDriver::new(4096);
        let device_id = driver.device_id();
        driver.inject_fault(&device_id, 0xbad, "hang");
        assert!(driver
            .submit(&device_id, WorkloadClass::Graphics, &[])
            .is_err());

        driver.reset_device(&device_id).unwrap();
        assert!(driver.fault_status(&device_id).unwrap().is_none());
        assert!(driver
            .submit(&device_id, WorkloadClass::Graphics, &[])
            .is_ok());
    }

    #[test]
    fn reset_device_rejects_unknown_device() {
        let mut driver = SoftwareGpuDriver::new(4096);
        assert!(driver.reset_device(&ObjectId::new()).is_err());
    }

    #[test]
    fn unknown_device_rejected() {
        let driver = SoftwareGpuDriver::new(4096);
        let bogus = ObjectId::new();
        assert!(driver.vram_available(&bogus).is_err());
    }

    #[test]
    fn with_devices_creates_the_requested_count() {
        let driver = SoftwareGpuDriver::with_devices(3, 1024);
        assert_eq!(driver.device_ids().len(), 3);
        assert_eq!(driver.probe().unwrap().len(), 3);
    }

    #[test]
    fn vram_is_isolated_per_device() {
        let mut driver = SoftwareGpuDriver::with_devices(2, 1024);
        let ids = driver.device_ids();
        let (a, b) = (ids[0], ids[1]);

        driver
            .allocate_vram(&a, 1024, MemoryClass::DeviceLocal)
            .unwrap();
        assert_eq!(driver.vram_available(&a).unwrap(), 0);
        assert_eq!(driver.vram_available(&b).unwrap(), 1024);

        // Allocating the full pool on device A must not affect device B.
        assert!(driver
            .allocate_vram(&b, 1024, MemoryClass::DeviceLocal)
            .is_ok());
    }

    #[test]
    fn fault_is_isolated_per_device() {
        let mut driver = SoftwareGpuDriver::with_devices(2, 1024);
        let ids = driver.device_ids();
        let (a, b) = (ids[0], ids[1]);

        driver.inject_fault(&a, 0xdead, "device A hang");
        assert!(driver.submit(&a, WorkloadClass::Graphics, &[]).is_err());
        assert!(driver.submit(&b, WorkloadClass::Graphics, &[]).is_ok());
        assert!(driver.fault_status(&b).unwrap().is_none());
    }

    #[test]
    fn freeing_allocation_credits_the_correct_device() {
        let mut driver = SoftwareGpuDriver::with_devices(2, 1024);
        let ids = driver.device_ids();
        let (a, b) = (ids[0], ids[1]);

        let alloc = driver
            .allocate_vram(&a, 512, MemoryClass::HostVisible)
            .unwrap();
        driver.free_vram(&alloc.id).unwrap();

        assert_eq!(driver.vram_available(&a).unwrap(), 1024);
        assert_eq!(driver.vram_available(&b).unwrap(), 1024);
    }
}

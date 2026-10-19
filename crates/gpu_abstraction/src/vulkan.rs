//! Feature-gated Vulkan-backed `GpuDriver`.
//!
//! `vulkan_backend` speaks Vulkan directly and is deliberately synchronous
//! per call, but a real Vulkan device is an asynchronous, device-lost-capable
//! object. Rather than making `GraphicsRuntime` async, this adapter owns a
//! dedicated render thread with its own Tokio runtime. Public `GpuDriver`
//! calls stay synchronous: they enqueue a command on an unbounded channel and
//! block on a reply channel. All Vulkan work happens on the render thread.
//!
//! Device loss (`VK_ERROR_DEVICE_LOST`) is translated into a normal
//! `Error::Device`, recorded in the same `GpuFault` channel as a software
//! driver fault, and recovered through the existing `reset_device` path. A
//! reset recreates the Vulkan context on the render thread before clearing
//! the fault. `mark_faulted` (used by `graphics_runtime`'s panic containment)
//! is also supported so a caught panic and a real device loss are
//! indistinguishable to callers.
//!
//! Vulkan availability is explicit: [`VulkanDriver::new`] returns a clear
//! `Error::Device` instead of silently pretending to render, and
//! [`FallbackDriver::open`] makes the software fallback visible via
//! [`FallbackDriver::is_vulkan`]/[`FallbackDriver::fallback_reason`]. The
//! feature is non-default; enabling it does not make the rest of the runtime
//! async or require a loader at build time (`vulkan_backend` dlopen()s it at
//! runtime).

use crate::{DmaMapping, DriverOp, GpuAllocation, GpuDriver, GpuFault, SoftwareGpuDriver};
use graphics_api::{MemoryClass, WorkloadClass};
use hal::{DeviceInfo, DeviceType, HardwareDriver};
use sher_common::{Error, ObjectId, Result};
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Adapter data reported by the backend. Kept independent of `ash` so the
/// worker/queue logic is testable with a mock backend that never touches
/// Vulkan.
#[derive(Debug, Clone)]
pub struct BackendAdapterInfo {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub api_version: (u32, u32, u32),
}

/// Errors a backend can report to the adapter. `DeviceLost` is the one the
/// synchronous `GpuDriver` shape has to treat specially: it becomes a
/// recoverable `GpuFault` rather than a fatal process error.
#[derive(Debug, Clone)]
pub enum BackendError {
    /// No loader/ICD or no suitable physical device. Expected on machines
    /// without Vulkan; callers should fall back explicitly.
    Unavailable(String),
    /// `VK_ERROR_DEVICE_LOST` (or the backend's simulation of it).
    DeviceLost(String),
    /// The backend is alive but cannot execute this operation yet.
    Unsupported(String),
    /// Any other Vulkan execution failure.
    Execution(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::Unavailable(message) => write!(f, "backend unavailable: {message}"),
            BackendError::DeviceLost(message) => write!(f, "device lost: {message}"),
            BackendError::Unsupported(message) => write!(f, "unsupported: {message}"),
            BackendError::Execution(message) => write!(f, "execution failed: {message}"),
        }
    }
}

impl std::error::Error for BackendError {}

type BackendResult<T> = std::result::Result<T, BackendError>;

/// Minimal render-thread-facing backend interface. The real implementation
/// wraps `vulkan_backend`; tests supply a mock that can simulate device loss
/// without hardware.
pub trait VulkanBackend: Send + 'static {
    fn enumerate_adapters(&mut self) -> BackendResult<Vec<BackendAdapterInfo>>;
    fn execute(&mut self, ops: &[DriverOp]) -> BackendResult<()>;
    /// Recreate the underlying device/context after device loss.
    fn reset(&mut self) -> BackendResult<()>;
}

/// Real Vulkan backend: a thin wrapper over `vulkan_backend::VulkanContext`.
///
/// `execute` currently uses the backend's verified offscreen clear-color
/// render as a real, fence-synchronized Vulkan submission. It proves the
/// wiring reaches a real ICD; translating individual `DriverOp`s into real
/// draws/dispatches is still follow-up work (see README).
pub struct RealVulkanBackend {
    context: vulkan_backend::VulkanContext,
}

impl RealVulkanBackend {
    pub fn open() -> BackendResult<Self> {
        let context = vulkan_backend::VulkanContext::probe().map_err(map_vulkan_error)?;
        Ok(Self { context })
    }
}

impl VulkanBackend for RealVulkanBackend {
    fn enumerate_adapters(&mut self) -> BackendResult<Vec<BackendAdapterInfo>> {
        self.context
            .enumerate_adapters()
            .map(|adapters| {
                adapters
                    .into_iter()
                    .map(|adapter| BackendAdapterInfo {
                        name: adapter.name,
                        vendor_id: adapter.vendor_id,
                        device_id: adapter.device_id,
                        api_version: adapter.api_version,
                    })
                    .collect()
            })
            .map_err(map_vulkan_error)
    }

    fn execute(&mut self, _ops: &[DriverOp]) -> BackendResult<()> {
        self.context
            .render_clear_to_offscreen(1, 1, [0.0, 0.0, 0.0, 1.0])
            .map(|_| ())
            .map_err(map_vulkan_error)
    }

    fn reset(&mut self) -> BackendResult<()> {
        self.context = vulkan_backend::VulkanContext::probe().map_err(map_vulkan_error)?;
        Ok(())
    }
}

fn map_vulkan_error(error: vulkan_backend::VulkanError) -> BackendError {
    use vulkan_backend::VulkanError;
    match error {
        VulkanError::LoaderUnavailable(message) => BackendError::Unavailable(message),
        VulkanError::DeviceLost(message) => BackendError::DeviceLost(message),
        VulkanError::Vulkan(result) => BackendError::Execution(result.to_string()),
        VulkanError::NoSuitableDevice(message) => BackendError::Unavailable(message),
        VulkanError::InvalidDimensions { width, height } => {
            BackendError::Execution(format!("invalid image dimensions {width}x{height}"))
        }
    }
}

fn backend_error_to_sher(error: BackendError) -> Error {
    match error {
        BackendError::Unavailable(message) => {
            Error::Device(format!("Vulkan unavailable: {message}"))
        }
        BackendError::DeviceLost(message) => {
            Error::Device(format!("Vulkan device lost: {message}"))
        }
        BackendError::Unsupported(message) => Error::Device(format!(
            "Vulkan backend does not support this operation: {message}"
        )),
        BackendError::Execution(message) => {
            Error::Device(format!("Vulkan execution failed: {message}"))
        }
    }
}

enum Command {
    Submit {
        ops: Vec<DriverOp>,
        reply: Sender<BackendResult<()>>,
    },
    Reset {
        reply: Sender<BackendResult<()>>,
    },
    Shutdown,
}

/// Synchronous `GpuDriver` adapter over a dedicated Vulkan render thread.
pub struct VulkanDriver {
    tx: UnboundedSender<Command>,
    worker: Option<JoinHandle<()>>,
    devices: Vec<DeviceInfo>,
    allocations: HashMap<ObjectId, GpuAllocation>,
    vram_available: HashMap<ObjectId, usize>,
    next_address: HashMap<ObjectId, u64>,
    faults: HashMap<ObjectId, GpuFault>,
    registers: HashMap<usize, u32>,
}

impl VulkanDriver {
    /// Opens the real Vulkan backend and starts its render thread. Returns a
    /// clear `Error::Device` if no loader/ICD/physical device is available —
    /// use [`FallbackDriver::open`] for an explicit software fallback.
    pub fn new(vram_bytes: usize) -> Result<Self> {
        let backend = RealVulkanBackend::open().map_err(backend_error_to_sher)?;
        Self::with_backend(backend, vram_bytes)
    }

    /// Constructs an adapter over any [`VulkanBackend`]. Public so callers
    /// and tests can inject a mock; `new` is the real-hardware path.
    pub fn with_backend<B: VulkanBackend>(mut backend: B, vram_bytes: usize) -> Result<Self> {
        let adapters = backend
            .enumerate_adapters()
            .map_err(backend_error_to_sher)?;
        if adapters.is_empty() {
            return Err(Error::Device(
                "Vulkan backend reported no physical devices".to_string(),
            ));
        }

        let devices: Vec<DeviceInfo> = adapters
            .into_iter()
            .map(|adapter| DeviceInfo {
                id: ObjectId::new(),
                device_type: DeviceType::Gpu,
                name: adapter.name,
                vendor_id: adapter.vendor_id,
                device_id: adapter.device_id,
                capabilities: vec![
                    "graphics".to_string(),
                    "compute".to_string(),
                    "transfer".to_string(),
                    "vulkan".to_string(),
                ],
                mmio_base: None,
                mmio_size: None,
                irq: None,
            })
            .collect();

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let worker = thread::Builder::new()
            .name("sher-vulkan-render".to_string())
            .spawn(move || run_worker(rx, backend))
            .map_err(|error| {
                Error::Device(format!("failed to spawn Vulkan render thread: {error}"))
            })?;

        let vram_available = devices
            .iter()
            .map(|device| (device.id, vram_bytes))
            .collect();
        let next_address = devices.iter().map(|device| (device.id, 0)).collect();

        Ok(Self {
            tx,
            worker: Some(worker),
            devices,
            allocations: HashMap::new(),
            vram_available,
            next_address,
            faults: HashMap::new(),
            registers: HashMap::new(),
        })
    }

    /// First probed device id. Convenience for the single-GPU case; use
    /// `device_ids()` for multi-GPU.
    pub fn device_id(&self) -> ObjectId {
        self.devices
            .first()
            .expect("VulkanDriver always has at least one device")
            .id
    }

    pub fn device_ids(&self) -> Vec<ObjectId> {
        self.devices.iter().map(|device| device.id).collect()
    }

    fn known_device(&self, device: &ObjectId) -> Result<&DeviceInfo> {
        self.devices
            .iter()
            .find(|info| &info.id == device)
            .ok_or_else(|| Error::Device("unknown GPU device".to_string()))
    }

    fn record_fault(&mut self, device: &ObjectId, description: String) {
        if self.known_device(device).is_ok() {
            self.faults.insert(
                *device,
                GpuFault {
                    device: *device,
                    address: 0,
                    description,
                },
            );
        }
    }

    fn request(&self, command: Command) -> Result<()> {
        self.tx
            .send(command)
            .map_err(|_| Error::Device("Vulkan render thread is not running".to_string()))
    }
}

fn run_worker<B: VulkanBackend>(mut rx: UnboundedReceiver<Command>, mut backend: B) {
    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
        Ok(runtime) => runtime,
        Err(_) => return,
    };
    runtime.block_on(async move {
        while let Some(command) = rx.recv().await {
            match command {
                Command::Submit { ops, reply } => {
                    let _ = reply.send(backend.execute(&ops));
                }
                Command::Reset { reply } => {
                    let _ = reply.send(backend.reset());
                }
                Command::Shutdown => break,
            }
        }
    });
}

impl HardwareDriver for VulkanDriver {
    fn probe(&self) -> Result<Vec<DeviceInfo>> {
        Ok(self.devices.clone())
    }

    fn initialize(&mut self, device: &DeviceInfo) -> Result<()> {
        self.known_device(&device.id)?;
        Ok(())
    }

    fn shutdown(&mut self, device_id: &ObjectId) -> Result<()> {
        self.known_device(device_id)?;
        Ok(())
    }

    fn get_capabilities(&self, device_id: &ObjectId) -> Result<Vec<String>> {
        Ok(self.known_device(device_id)?.capabilities.clone())
    }

    fn read_register(&self, device_id: &ObjectId, offset: usize) -> Result<u32> {
        self.known_device(device_id)?;
        Ok(*self.registers.get(&offset).unwrap_or(&0))
    }

    fn write_register(&self, device_id: &ObjectId, _offset: usize, _value: u32) -> Result<()> {
        self.known_device(device_id)?;
        Ok(())
    }
}

impl GpuDriver for VulkanDriver {
    fn allocate_vram(
        &mut self,
        device: &ObjectId,
        size: usize,
        class: MemoryClass,
    ) -> Result<GpuAllocation> {
        self.known_device(device)?;
        let available = self
            .vram_available
            .get_mut(device)
            .ok_or_else(|| Error::Device("unknown GPU device".to_string()))?;
        if size > *available {
            return Err(Error::Memory("insufficient VRAM".to_string()));
        }
        let next = self.next_address.entry(*device).or_insert(0);
        let allocation = GpuAllocation {
            id: ObjectId::new(),
            device: *device,
            class,
            size,
            device_address: *next,
        };
        *next += size as u64;
        *available -= size;
        self.allocations.insert(allocation.id, allocation.clone());
        Ok(allocation)
    }

    fn free_vram(&mut self, allocation: &ObjectId) -> Result<()> {
        let allocation = self
            .allocations
            .remove(allocation)
            .ok_or_else(|| Error::Memory("unknown allocation".to_string()))?;
        let available = self
            .vram_available
            .get_mut(&allocation.device)
            .ok_or_else(|| Error::Device("unknown GPU device".to_string()))?;
        *available += allocation.size;
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
        self.known_device(device)?;
        if let Some(fault) = self.faults.get(device) {
            return Err(Error::Device(format!(
                "device faulted at {:#x}: {}",
                fault.address, fault.description
            )));
        }

        let (reply_tx, reply_rx) = mpsc::channel();
        self.request(Command::Submit {
            ops: ops.to_vec(),
            reply: reply_tx,
        })?;

        match reply_rx.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(BackendError::DeviceLost(message))) => {
                let description = format!("device lost: {message}");
                self.record_fault(device, description.clone());
                Err(Error::Device(format!("Vulkan device lost: {message}")))
            }
            Ok(Err(error)) => Err(backend_error_to_sher(error)),
            Err(_) => {
                let description = "Vulkan render thread stopped before replying".to_string();
                self.record_fault(device, description.clone());
                Err(Error::Device(description))
            }
        }
    }

    fn fault_status(&self, device: &ObjectId) -> Result<Option<GpuFault>> {
        self.known_device(device)?;
        Ok(self.faults.get(device).cloned())
    }

    fn vram_available(&self, device: &ObjectId) -> Result<usize> {
        self.vram_available
            .get(device)
            .copied()
            .ok_or_else(|| Error::Device("unknown GPU device".to_string()))
    }

    fn reset_device(&mut self, device: &ObjectId) -> Result<()> {
        self.known_device(device)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.request(Command::Reset { reply: reply_tx })?;

        match reply_rx.recv() {
            Ok(Ok(())) => {
                self.faults.remove(device);
                Ok(())
            }
            Ok(Err(error)) => Err(backend_error_to_sher(error)),
            Err(_) => Err(Error::Device(
                "Vulkan render thread stopped before replying to reset".to_string(),
            )),
        }
    }

    fn mark_faulted(&mut self, device: &ObjectId, description: String) {
        self.record_fault(device, description);
    }
}

impl Drop for VulkanDriver {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Explicit Vulkan-or-software selection. `open` never hides a Vulkan
/// failure: [`is_vulkan`](Self::is_vulkan) reports which driver won and
/// [`fallback_reason`](Self::fallback_reason) carries the original error.
pub enum FallbackDriver {
    Vulkan(VulkanDriver),
    Software {
        driver: SoftwareGpuDriver,
        reason: String,
    },
}

impl FallbackDriver {
    /// Tries Vulkan, then falls back to `SoftwareGpuDriver` with the reason
    /// recorded. This is the intended entry point for applications that
    /// cannot assume a Vulkan loader/ICD is installed.
    pub fn open(vram_bytes: usize) -> Self {
        Self::from_vulkan_result(VulkanDriver::new(vram_bytes), vram_bytes)
    }

    /// Same as [`open`](Self::open) but over an already-attempted result.
    /// Lets tests exercise the fallback without controlling the host's
    /// Vulkan installation.
    pub fn from_vulkan_result(result: Result<VulkanDriver>, vram_bytes: usize) -> Self {
        match result {
            Ok(driver) => FallbackDriver::Vulkan(driver),
            Err(error) => FallbackDriver::Software {
                driver: SoftwareGpuDriver::new(vram_bytes),
                reason: error.to_string(),
            },
        }
    }

    pub fn is_vulkan(&self) -> bool {
        matches!(self, FallbackDriver::Vulkan(_))
    }

    pub fn fallback_reason(&self) -> Option<&str> {
        match self {
            FallbackDriver::Vulkan(_) => None,
            FallbackDriver::Software { reason, .. } => Some(reason),
        }
    }
}

macro_rules! delegate {
    ($self:expr, $driver:ident => $call:expr) => {
        match $self {
            FallbackDriver::Vulkan(driver) => {
                let $driver = driver;
                $call
            }
            FallbackDriver::Software { driver, .. } => {
                let $driver = driver;
                $call
            }
        }
    };
}

impl HardwareDriver for FallbackDriver {
    fn probe(&self) -> Result<Vec<DeviceInfo>> {
        delegate!(self, driver => driver.probe())
    }

    fn initialize(&mut self, device: &DeviceInfo) -> Result<()> {
        delegate!(self, driver => driver.initialize(device))
    }

    fn shutdown(&mut self, device_id: &ObjectId) -> Result<()> {
        delegate!(self, driver => driver.shutdown(device_id))
    }

    fn get_capabilities(&self, device_id: &ObjectId) -> Result<Vec<String>> {
        delegate!(self, driver => driver.get_capabilities(device_id))
    }

    fn read_register(&self, device_id: &ObjectId, offset: usize) -> Result<u32> {
        delegate!(self, driver => driver.read_register(device_id, offset))
    }

    fn write_register(&self, device_id: &ObjectId, offset: usize, value: u32) -> Result<()> {
        delegate!(self, driver => driver.write_register(device_id, offset, value))
    }
}

impl GpuDriver for FallbackDriver {
    fn allocate_vram(
        &mut self,
        device: &ObjectId,
        size: usize,
        class: MemoryClass,
    ) -> Result<GpuAllocation> {
        delegate!(self, driver => driver.allocate_vram(device, size, class))
    }

    fn free_vram(&mut self, allocation: &ObjectId) -> Result<()> {
        delegate!(self, driver => driver.free_vram(allocation))
    }

    fn map_dma(&mut self, allocation: &ObjectId) -> Result<DmaMapping> {
        delegate!(self, driver => driver.map_dma(allocation))
    }

    fn submit(&mut self, device: &ObjectId, class: WorkloadClass, ops: &[DriverOp]) -> Result<()> {
        delegate!(self, driver => driver.submit(device, class, ops))
    }

    fn fault_status(&self, device: &ObjectId) -> Result<Option<GpuFault>> {
        delegate!(self, driver => driver.fault_status(device))
    }

    fn vram_available(&self, device: &ObjectId) -> Result<usize> {
        delegate!(self, driver => driver.vram_available(device))
    }

    fn reset_device(&mut self, device: &ObjectId) -> Result<()> {
        delegate!(self, driver => driver.reset_device(device))
    }

    fn mark_faulted(&mut self, device: &ObjectId, description: String) {
        delegate!(self, driver => driver.mark_faulted(device, description))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct MockBackend {
        adapters: Vec<BackendAdapterInfo>,
        executed: Arc<Mutex<Vec<DriverOp>>>,
        executions: Arc<AtomicUsize>,
        resets: Arc<AtomicUsize>,
        lost: Arc<AtomicBool>,
        unavailable: bool,
    }

    impl MockBackend {
        fn new() -> Self {
            Self {
                adapters: vec![BackendAdapterInfo {
                    name: "Mock Vulkan Device".to_string(),
                    vendor_id: 0x1234,
                    device_id: 0x5678,
                    api_version: (1, 3, 0),
                }],
                executed: Arc::new(Mutex::new(Vec::new())),
                executions: Arc::new(AtomicUsize::new(0)),
                resets: Arc::new(AtomicUsize::new(0)),
                lost: Arc::new(AtomicBool::new(false)),
                unavailable: false,
            }
        }

        fn unavailable() -> Self {
            Self {
                unavailable: true,
                ..Self::new()
            }
        }

        fn lose_device(&self) {
            self.lost.store(true, Ordering::SeqCst);
        }

        fn executed(&self) -> Vec<DriverOp> {
            self.executed.lock().unwrap().clone()
        }

        fn executions(&self) -> usize {
            self.executions.load(Ordering::SeqCst)
        }

        fn resets(&self) -> usize {
            self.resets.load(Ordering::SeqCst)
        }
    }

    impl VulkanBackend for MockBackend {
        fn enumerate_adapters(&mut self) -> BackendResult<Vec<BackendAdapterInfo>> {
            if self.unavailable {
                return Err(BackendError::Unavailable("no mock loader".to_string()));
            }
            Ok(self.adapters.clone())
        }

        fn execute(&mut self, ops: &[DriverOp]) -> BackendResult<()> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            if self.lost.load(Ordering::SeqCst) {
                return Err(BackendError::DeviceLost(
                    "VK_ERROR_DEVICE_LOST (mock)".to_string(),
                ));
            }
            self.executed.lock().unwrap().extend_from_slice(ops);
            Ok(())
        }

        fn reset(&mut self) -> BackendResult<()> {
            self.resets.fetch_add(1, Ordering::SeqCst);
            self.lost.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    fn driver_with_mock(mock: &MockBackend) -> VulkanDriver {
        VulkanDriver::with_backend(mock.clone(), 4096).unwrap()
    }

    #[test]
    fn probe_exposes_backend_adapters_as_gpu_devices() {
        let driver = driver_with_mock(&MockBackend::new());
        let devices = driver.probe().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Mock Vulkan Device");
        assert_eq!(devices[0].device_type, DeviceType::Gpu);
        assert!(devices[0].capabilities.contains(&"vulkan".to_string()));
        assert_eq!(driver.device_ids(), vec![devices[0].id]);
    }

    #[test]
    fn submit_commands_travel_through_the_render_thread_in_order() {
        let mock = MockBackend::new();
        let mut driver = driver_with_mock(&mock);
        let device = driver.device_id();

        let first = vec![
            DriverOp::Barrier,
            DriverOp::Draw {
                vertex_count: 3,
                instance_count: 1,
            },
        ];
        let second = vec![DriverOp::Dispatch { x: 8, y: 4, z: 1 }];

        driver
            .submit(&device, WorkloadClass::Graphics, &first)
            .unwrap();
        driver
            .submit(&device, WorkloadClass::Compute, &second)
            .unwrap();

        assert_eq!(mock.executions(), 2);
        assert_eq!(mock.executed().len(), 3);
        assert!(matches!(mock.executed()[0], DriverOp::Barrier));
        assert!(matches!(mock.executed()[1], DriverOp::Draw { .. }));
        assert!(matches!(
            mock.executed()[2],
            DriverOp::Dispatch { x: 8, y: 4, z: 1 }
        ));
    }

    #[test]
    fn device_lost_maps_to_a_recoverable_fault_and_reset_recovers() {
        let mock = MockBackend::new();
        let mut driver = driver_with_mock(&mock);
        let device = driver.device_id();

        mock.lose_device();
        let error = driver
            .submit(&device, WorkloadClass::Graphics, &[DriverOp::Barrier])
            .unwrap_err();
        assert!(error.to_string().contains("device lost"), "{error}");

        let fault = driver.fault_status(&device).unwrap().unwrap();
        assert!(fault.description.contains("device lost"));

        // A faulted device rejects further work without reaching the backend.
        let error = driver
            .submit(&device, WorkloadClass::Graphics, &[DriverOp::Barrier])
            .unwrap_err();
        assert!(error.to_string().contains("faulted"), "{error}");
        assert_eq!(mock.executions(), 1);

        // Existing runtime recovery path: reset clears the fault and makes the
        // device submittable again.
        driver.reset_device(&device).unwrap();
        assert!(driver.fault_status(&device).unwrap().is_none());
        assert_eq!(mock.resets(), 1);
        driver
            .submit(&device, WorkloadClass::Graphics, &[DriverOp::Barrier])
            .unwrap();
        assert_eq!(mock.executions(), 2);
    }

    #[test]
    fn mark_faulted_uses_the_same_fault_channel() {
        let mock = MockBackend::new();
        let mut driver = driver_with_mock(&mock);
        let device = driver.device_id();

        driver.mark_faulted(&device, "caught driver panic".to_string());
        let fault = driver.fault_status(&device).unwrap().unwrap();
        assert_eq!(fault.description, "caught driver panic");
        assert!(driver
            .submit(&device, WorkloadClass::Graphics, &[])
            .is_err());
        assert_eq!(mock.executions(), 0);
    }

    #[test]
    fn unavailable_backend_falls_back_explicitly_to_software() {
        let result = VulkanDriver::with_backend(MockBackend::unavailable(), 4096);
        assert!(result.is_err());

        let fallback = FallbackDriver::from_vulkan_result(result, 4096);
        assert!(!fallback.is_vulkan());
        let reason = fallback.fallback_reason().unwrap();
        assert!(reason.contains("Vulkan unavailable"), "{reason}");
        assert_eq!(fallback.probe().unwrap().len(), 1);
    }

    #[test]
    fn real_backend_construction_is_explicit_when_vulkan_is_absent() {
        match VulkanDriver::new(4096) {
            Ok(driver) => assert!(!driver.probe().unwrap().is_empty()),
            Err(error) => {
                let message = error.to_string();
                assert!(
                    message.contains("Vulkan unavailable"),
                    "unexpected Vulkan init error: {message}"
                );
            }
        }
    }

    #[test]
    fn vram_accounting_is_per_device() {
        let mut driver = driver_with_mock(&MockBackend::new());
        let device = driver.device_id();

        let allocation = driver
            .allocate_vram(&device, 1024, MemoryClass::DeviceLocal)
            .unwrap();
        assert_eq!(driver.vram_available(&device).unwrap(), 4096 - 1024);
        assert_eq!(driver.map_dma(&allocation.id).unwrap().size, 1024);

        driver.free_vram(&allocation.id).unwrap();
        assert_eq!(driver.vram_available(&device).unwrap(), 4096);
    }

    #[test]
    fn unknown_devices_are_rejected() {
        let mut driver = driver_with_mock(&MockBackend::new());
        let unknown = ObjectId::new();
        assert!(driver
            .submit(&unknown, WorkloadClass::Graphics, &[])
            .is_err());
        assert!(driver.vram_available(&unknown).is_err());
        assert!(driver.reset_device(&unknown).is_err());
    }
}

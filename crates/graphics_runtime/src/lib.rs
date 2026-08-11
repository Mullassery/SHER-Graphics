//! SHER Graphics Runtime
//!
//! Implements `graphics_api::GraphicsApi` against a concrete `GpuDriver`
//! (`gpu_abstraction`). Owns resource tracking, memory accounting, command
//! translation, timeline synchronization, and the presentation bridge to
//! `gpu_driver`'s connector/framebuffer primitives.
//!
//! This is the layer ARCHITECTURE.md section 4 is explicit should *not* be
//! portable: it assumes SHER's object model and capability system, not a
//! platform-neutral abstraction. Portability lives at the API surface
//! (`graphics_api`) and the compatibility boundary (Mesa / `graphics_compat`),
//! not here.

use gpu_abstraction::{DriverOp, GpuDriver};
use gpu_driver::{Connector, GPUDriver};
use graphics_api::{
    CommandStream, GraphicsApi, GraphicsDevice, GraphicsOp, Pipeline, PixelFormat, QueueHandle,
    Resource, ResourceKind, ShaderModule, ShaderStage, Timeline, WorkloadClass,
};
pub use graphics_api::MemoryClass;
use sher_common::{Capability, Error, ObjectId, Result};
use sher_objectmodel::CapabilitySet;
use std::collections::HashMap;

fn bytes_per_pixel(format: PixelFormat) -> usize {
    match format {
        PixelFormat::Rgba8Unorm | PixelFormat::Bgra8Unorm => 4,
        PixelFormat::Rgba16Float => 8,
        PixelFormat::Depth32Float => 4,
    }
}

fn resource_size(kind: &ResourceKind) -> usize {
    match kind {
        ResourceKind::Buffer { size, .. } => *size,
        ResourceKind::Image { width, height, depth, format, .. } => {
            (*width as usize) * (*height as usize) * (*depth as usize) * bytes_per_pixel(*format)
        }
    }
}

/// Narrows the app-facing `GraphicsOp` vocabulary down to what a driver
/// backend actually executes. `BindPipeline`/`BindResource` are runtime-side
/// state changes, not driver-level commands, so they don't cross this
/// boundary — see `gpu_abstraction::DriverOp` module docs.
fn translate_ops(ops: &[GraphicsOp]) -> Vec<DriverOp> {
    ops.iter()
        .filter_map(|op| match op {
            GraphicsOp::Draw { vertex_count, instance_count } => Some(DriverOp::Draw {
                vertex_count: *vertex_count,
                instance_count: *instance_count,
            }),
            GraphicsOp::Dispatch { x, y, z } => Some(DriverOp::Dispatch { x: *x, y: *y, z: *z }),
            GraphicsOp::Copy { src, dst } => Some(DriverOp::Copy { src: *src, dst: *dst }),
            GraphicsOp::Barrier => Some(DriverOp::Barrier),
            GraphicsOp::BindPipeline(_) | GraphicsOp::BindResource { .. } => None,
        })
        .collect()
}

/// Bridges `graphics_runtime`'s presentation intent to `gpu_driver`'s
/// existing connector/framebuffer/page-flip primitives (Phase 11's
/// DRM/KMS-shaped skeleton). See ARCHITECTURE.md section 14.
///
/// Deliberately minimal: it does not yet convert an arbitrary
/// `graphics_api::Resource` into a `gpu_driver::Framebuffer` — that
/// conversion is tracked as future work. For now it demonstrates the seam
/// end to end with its own framebuffer allocation.
pub struct PresentationBridge {
    display: GPUDriver,
}

impl PresentationBridge {
    pub fn new(display_vram_bytes: usize) -> Self {
        Self { display: GPUDriver::new(display_vram_bytes) }
    }

    pub fn register_connector(&mut self, connector: Connector) -> Result<()> {
        self.display.register_connector(connector)
    }

    pub fn present(&mut self, connector_id: &ObjectId, width: u32, height: u32) -> Result<ObjectId> {
        let framebuffer = self.display.allocate_framebuffer(width, height)?;
        self.display.page_flip(connector_id, &framebuffer.id)?;
        Ok(framebuffer.id)
    }
}

pub struct GraphicsRuntime<D: GpuDriver> {
    driver: D,
    devices: HashMap<ObjectId, GraphicsDevice>,
    resources: HashMap<ObjectId, Resource>,
    shaders: HashMap<ObjectId, ShaderModule>,
    pipelines: HashMap<ObjectId, Pipeline>,
    timelines: HashMap<ObjectId, u64>,
    presentation: PresentationBridge,
}

impl<D: GpuDriver> GraphicsRuntime<D> {
    pub fn new(driver: D, presentation_vram_bytes: usize) -> Self {
        Self {
            driver,
            devices: HashMap::new(),
            resources: HashMap::new(),
            shaders: HashMap::new(),
            pipelines: HashMap::new(),
            timelines: HashMap::new(),
            presentation: PresentationBridge::new(presentation_vram_bytes),
        }
    }

    fn device(&self, id: &ObjectId) -> Result<&GraphicsDevice> {
        self.devices
            .get(id)
            .ok_or_else(|| Error::Device("unknown graphics device".to_string()))
    }

    pub fn free_resource(&mut self, resource: &ObjectId) -> Result<()> {
        self.resources
            .remove(resource)
            .ok_or_else(|| Error::Memory("unknown resource".to_string()))?;
        self.driver.free_vram(resource)
    }

    pub fn register_connector(&mut self, connector: Connector) -> Result<()> {
        self.presentation.register_connector(connector)
    }

    pub fn present_frame(&mut self, connector: &ObjectId, width: u32, height: u32) -> Result<ObjectId> {
        self.presentation.present(connector, width, height)
    }
}

impl<D: GpuDriver> GraphicsApi for GraphicsRuntime<D> {
    fn create_device(&mut self, gpu: ObjectId, caps: &CapabilitySet) -> Result<GraphicsDevice> {
        graphics_api::require_capability(caps, Capability::GpuMemoryAlloc)?;

        let known = self.driver.probe()?;
        let device_info = known
            .into_iter()
            .find(|d| d.id == gpu)
            .ok_or_else(|| Error::Device("GPU not found via driver probe".to_string()))?;
        self.driver.initialize(&device_info)?;

        let device = GraphicsDevice {
            id: ObjectId::new(),
            gpu,
            queues: vec![
                QueueHandle { id: ObjectId::new(), class: WorkloadClass::Graphics },
                QueueHandle { id: ObjectId::new(), class: WorkloadClass::Compute },
                QueueHandle { id: ObjectId::new(), class: WorkloadClass::Transfer },
            ],
        };
        self.devices.insert(device.id, device.clone());
        Ok(device)
    }

    fn create_timeline(&mut self, device: &ObjectId) -> Result<Timeline> {
        self.device(device)?;
        let timeline = Timeline { id: ObjectId::new(), device: *device, current_value: 0 };
        self.timelines.insert(timeline.id, 0);
        Ok(timeline)
    }

    fn create_resource(
        &mut self,
        device: &ObjectId,
        kind: ResourceKind,
        memory_class: MemoryClass,
    ) -> Result<Resource> {
        let gpu = self.device(device)?.gpu;
        let size = resource_size(&kind);
        let allocation = self.driver.allocate_vram(&gpu, size, memory_class)?;
        let resource = Resource { id: allocation.id, device: *device, kind, memory_class };
        self.resources.insert(resource.id, resource.clone());
        Ok(resource)
    }

    fn create_command_stream(
        &mut self,
        device: &ObjectId,
        class: WorkloadClass,
    ) -> Result<CommandStream> {
        self.device(device)?;
        Ok(CommandStream { id: ObjectId::new(), device: *device, target_class: class, ops: Vec::new() })
    }

    fn create_shader_module(
        &mut self,
        device: &ObjectId,
        stage: ShaderStage,
        ir: Vec<u8>,
    ) -> Result<ShaderModule> {
        self.device(device)?;
        let module = ShaderModule { id: ObjectId::new(), device: *device, stage, ir };
        self.shaders.insert(module.id, module.clone());
        Ok(module)
    }

    fn create_pipeline(
        &mut self,
        device: &ObjectId,
        shader: &ObjectId,
        class: WorkloadClass,
    ) -> Result<Pipeline> {
        self.device(device)?;
        if !self.shaders.contains_key(shader) {
            return Err(Error::Device("unknown shader module".to_string()));
        }
        let pipeline = Pipeline { id: ObjectId::new(), device: *device, class, shader: *shader };
        self.pipelines.insert(pipeline.id, pipeline.clone());
        Ok(pipeline)
    }

    fn submit(
        &mut self,
        queue: &QueueHandle,
        stream: CommandStream,
        timeline: &ObjectId,
        signal_value: u64,
    ) -> Result<()> {
        if stream.target_class != queue.class {
            return Err(Error::Device(
                "command stream workload class does not match queue".to_string(),
            ));
        }
        let gpu = self.device(&stream.device)?.gpu;
        let current = *self
            .timelines
            .get(timeline)
            .ok_or_else(|| Error::Device("unknown timeline".to_string()))?;
        if signal_value <= current {
            return Err(Error::Device(
                "timeline signal value must be strictly increasing".to_string(),
            ));
        }

        let driver_ops = translate_ops(&stream.ops);
        self.driver.submit(&gpu, queue.class, &driver_ops)?;

        self.timelines.insert(*timeline, signal_value);
        Ok(())
    }

    fn timeline_value(&self, timeline: &ObjectId) -> Result<u64> {
        self.timelines
            .get(timeline)
            .copied()
            .ok_or_else(|| Error::Device("unknown timeline".to_string()))
    }

    fn wait(&self, timeline: &ObjectId, value: u64) -> Result<()> {
        // Reference implementation: `submit` above executes synchronously,
        // so waiting is a value check rather than a real block. A hardware
        // backend instead parks on the kernel wake-on-value primitive
        // described in ARCHITECTURE.md section 13.
        let current = self.timeline_value(timeline)?;
        if current >= value {
            Ok(())
        } else {
            Err(Error::Device(format!(
                "timeline not yet signaled: at {current}, waiting for {value}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpu_abstraction::SoftwareGpuDriver;
    use gpu_driver::{ConnectorStatus, ConnectorType};
    use graphics_api::BufferUsage;
    use sher_common::PermissionTier;

    fn caps_with(cap: Capability) -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.grant(cap, PermissionTier::High);
        caps
    }

    fn runtime_with_device() -> (GraphicsRuntime<SoftwareGpuDriver>, GraphicsDevice) {
        let driver = SoftwareGpuDriver::new(64 * 1024 * 1024);
        let gpu = driver.device_id();
        let mut runtime = GraphicsRuntime::new(driver, 16 * 1024 * 1024);
        let caps = caps_with(Capability::GpuMemoryAlloc);
        let device = runtime.create_device(gpu, &caps).unwrap();
        (runtime, device)
    }

    #[test]
    fn create_device_requires_capability() {
        let driver = SoftwareGpuDriver::new(1024);
        let gpu = driver.device_id();
        let mut runtime = GraphicsRuntime::new(driver, 1024);
        let result = runtime.create_device(gpu, &CapabilitySet::default());
        assert!(result.is_err());
    }

    #[test]
    fn create_device_rejects_unknown_gpu() {
        let driver = SoftwareGpuDriver::new(1024);
        let mut runtime = GraphicsRuntime::new(driver, 1024);
        let caps = caps_with(Capability::GpuMemoryAlloc);
        let result = runtime.create_device(ObjectId::new(), &caps);
        assert!(result.is_err());
    }

    #[test]
    fn create_device_populates_three_workload_queues() {
        let (_, device) = runtime_with_device();
        assert!(device.queue(WorkloadClass::Graphics).is_some());
        assert!(device.queue(WorkloadClass::Compute).is_some());
        assert!(device.queue(WorkloadClass::Transfer).is_some());
    }

    #[test]
    fn create_resource_allocates_from_driver() {
        let (mut runtime, device) = runtime_with_device();
        let resource = runtime
            .create_resource(
                &device.id,
                ResourceKind::Buffer { size: 4096, usage: BufferUsage::default() },
                MemoryClass::HostVisible,
            )
            .unwrap();
        match resource.kind {
            ResourceKind::Buffer { size, .. } => assert_eq!(size, 4096),
            _ => panic!("expected buffer"),
        }
    }

    #[test]
    fn create_resource_fails_when_vram_exhausted() {
        let driver = SoftwareGpuDriver::new(1024);
        let gpu = driver.device_id();
        let mut runtime = GraphicsRuntime::new(driver, 1024);
        let caps = caps_with(Capability::GpuMemoryAlloc);
        let device = runtime.create_device(gpu, &caps).unwrap();

        let result = runtime.create_resource(
            &device.id,
            ResourceKind::Buffer { size: 4096, usage: BufferUsage::default() },
            MemoryClass::DeviceLocal,
        );
        assert!(result.is_err());
    }

    #[test]
    fn submit_advances_timeline() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let stream = runtime.create_command_stream(&device.id, WorkloadClass::Graphics).unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        runtime.submit(&queue, stream, &timeline.id, 1).unwrap();
        assert_eq!(runtime.timeline_value(&timeline.id).unwrap(), 1);
        assert!(runtime.wait(&timeline.id, 1).is_ok());
        assert!(runtime.wait(&timeline.id, 2).is_err());
    }

    #[test]
    fn submit_rejects_mismatched_workload_class() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let stream = runtime.create_command_stream(&device.id, WorkloadClass::Compute).unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        let result = runtime.submit(&queue, stream, &timeline.id, 1);
        assert!(result.is_err());
    }

    #[test]
    fn submit_requires_monotonic_timeline_value() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let queue = device.queue(WorkloadClass::Transfer).unwrap();

        let first = runtime.create_command_stream(&device.id, WorkloadClass::Transfer).unwrap();
        runtime.submit(&queue, first, &timeline.id, 5).unwrap();

        let second = runtime.create_command_stream(&device.id, WorkloadClass::Transfer).unwrap();
        let result = runtime.submit(&queue, second, &timeline.id, 5);
        assert!(result.is_err());
    }

    #[test]
    fn pipeline_creation_requires_known_shader() {
        let (mut runtime, device) = runtime_with_device();
        let bogus_shader = ObjectId::new();
        let result = runtime.create_pipeline(&device.id, &bogus_shader, WorkloadClass::Graphics);
        assert!(result.is_err());
    }

    #[test]
    fn pipeline_creation_succeeds_for_registered_shader() {
        let (mut runtime, device) = runtime_with_device();
        let shader = runtime
            .create_shader_module(&device.id, ShaderStage::Vertex, vec![0u8; 8])
            .unwrap();
        let pipeline = runtime
            .create_pipeline(&device.id, &shader.id, WorkloadClass::Graphics)
            .unwrap();
        assert_eq!(pipeline.shader, shader.id);
    }

    #[test]
    fn presentation_bridge_round_trip() {
        let (mut runtime, _device) = runtime_with_device();
        let connector = Connector {
            id: ObjectId::new(),
            connector_type: ConnectorType::HDMI,
            status: ConnectorStatus::Connected,
            supported_modes: vec![],
            current_mode: None,
        };
        let connector_id = connector.id;
        runtime.register_connector(connector).unwrap();

        let framebuffer = runtime.present_frame(&connector_id, 1920, 1080).unwrap();
        assert_ne!(framebuffer, ObjectId::nil());
    }

    #[test]
    fn free_resource_returns_vram() {
        let (mut runtime, device) = runtime_with_device();
        let resource = runtime
            .create_resource(
                &device.id,
                ResourceKind::Buffer { size: 4096, usage: BufferUsage::default() },
                MemoryClass::HostVisible,
            )
            .unwrap();
        assert!(runtime.free_resource(&resource.id).is_ok());
        assert!(runtime.free_resource(&resource.id).is_err());
    }
}

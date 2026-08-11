//! Native SHER Graphics API
//!
//! Application-facing object model for SHER Graphics. This crate defines the
//! vocabulary (devices, resources, command streams, timelines, pipelines) and
//! the `GraphicsApi` trait that `graphics_runtime` implements.
//!
//! Deliberately excluded, and why (see ARCHITECTURE.md section 2-3 for the
//! full rationale):
//! - No instance/physical-device/device three-tier bootstrap. `hal` already
//!   enumerates GPUs; a device is addressed by the `ObjectId` `hal` assigned it.
//! - No descriptor sets as a distinct object family. Resource binding is
//!   expressed directly as `GraphicsOp::BindResource` on the command stream;
//!   the runtime decides the on-hardware binding-table representation.
//! - No separate "allocate memory" + "bind memory to resource" step by
//!   default. `create_resource` returns something already backed by memory
//!   from the requested `MemoryClass`.
//! - One synchronization primitive (`Timeline`), not a binary/timeline split.
//!   SHER has no ABI history to preserve, so it doesn't inherit Vulkan's.

use sher_common::{Capability, Error, ObjectId, PermissionTier, Result};
use sher_objectmodel::CapabilitySet;

/// Hardware workload classes. Replaces Vulkan's opaque, driver-enumerated
/// queue-family bitmasks — the runtime already knows what hardware it's
/// running on via `hal`, so applications ask for a class of work directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkloadClass {
    Graphics,
    Compute,
    Transfer,
}

/// GPU memory heap classes. Physical reality (device-local vs. host-visible
/// vs. cached readback) rather than API taste, so this is kept close to what
/// hardware actually exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryClass {
    /// VRAM, not CPU-visible without a copy.
    DeviceLocal,
    /// CPU-writable, GPU-readable. Staging/uniform uploads.
    HostVisible,
    /// CPU-readable back. Readback/download buffers.
    HostVisibleCached,
    /// Resizable BAR / Smart Access Memory — CPU-visible VRAM window, only
    /// available when the underlying `GpuDriver` reports support.
    DeviceLocalHostVisible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    Rgba8Unorm,
    Bgra8Unorm,
    Rgba16Float,
    Depth32Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BufferUsage {
    pub vertex: bool,
    pub index: bool,
    pub uniform: bool,
    pub storage: bool,
    pub transfer_src: bool,
    pub transfer_dst: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageUsage {
    pub sampled: bool,
    pub storage: bool,
    pub color_attachment: bool,
    pub depth_attachment: bool,
    pub transfer_src: bool,
    pub transfer_dst: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResourceKind {
    Buffer {
        size: usize,
        usage: BufferUsage,
    },
    Image {
        width: u32,
        height: u32,
        depth: u32,
        format: PixelFormat,
        usage: ImageUsage,
    },
}

#[derive(Debug, Clone)]
pub struct Resource {
    pub id: ObjectId,
    pub device: ObjectId,
    pub kind: ResourceKind,
    pub memory_class: MemoryClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    Vertex,
    Fragment,
    Compute,
}

/// Intermediate representation blob. In the compatibility paths this holds
/// SPIR-V/NIR produced by Mesa's own compiler; in the native path it holds
/// output from a Rust-native shader compiler. `graphics_runtime` never
/// interprets the bytes itself — only the driver backend does.
#[derive(Debug, Clone)]
pub struct ShaderModule {
    pub id: ObjectId,
    pub device: ObjectId,
    pub stage: ShaderStage,
    pub ir: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub id: ObjectId,
    pub device: ObjectId,
    pub class: WorkloadClass,
    pub shader: ObjectId,
}

/// A single, portable, driver-independent command. Translation into
/// hardware-specific ring-buffer commands happens once, inside the driver
/// backend — never duplicated per application.
#[derive(Debug, Clone)]
pub enum GraphicsOp {
    BindPipeline(ObjectId),
    BindResource { binding: u32, resource: ObjectId },
    Draw { vertex_count: u32, instance_count: u32 },
    Dispatch { x: u32, y: u32, z: u32 },
    Copy { src: ObjectId, dst: ObjectId },
    Barrier,
}

#[derive(Debug, Clone)]
pub struct CommandStream {
    pub id: ObjectId,
    pub device: ObjectId,
    pub target_class: WorkloadClass,
    pub ops: Vec<GraphicsOp>,
}

impl CommandStream {
    pub fn push(&mut self, op: GraphicsOp) {
        self.ops.push(op);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QueueHandle {
    pub id: ObjectId,
    pub class: WorkloadClass,
}

#[derive(Debug, Clone)]
pub struct GraphicsDevice {
    pub id: ObjectId,
    pub gpu: ObjectId,
    pub queues: Vec<QueueHandle>,
}

impl GraphicsDevice {
    pub fn queue(&self, class: WorkloadClass) -> Option<QueueHandle> {
        self.queues.iter().copied().find(|q| q.class == class)
    }
}

/// A monotonically increasing per-context counter. The one synchronization
/// primitive in SHER Graphics — see module docs and ARCHITECTURE.md section
/// 13 for why this replaces Vulkan's binary+timeline semaphore split.
#[derive(Debug, Clone)]
pub struct Timeline {
    pub id: ObjectId,
    pub device: ObjectId,
    pub current_value: u64,
}

/// Checks that a capability set authorizes a GPU-facing operation. Reuses
/// SHER Kernel's existing capability/tier model (`sher_common::Capability`,
/// `PermissionTier`) rather than inventing a graphics-specific permission
/// system — see ARCHITECTURE.md section 16.
pub fn require_capability(caps: &CapabilitySet, capability: Capability) -> Result<()> {
    if caps.has_capability(capability) {
        Ok(())
    } else {
        Err(Error::Security(format!(
            "missing required capability: {capability:?}"
        )))
    }
}

/// Default tier recommended for a given GPU capability. Callers decide
/// whether to follow this; it exists so every crate that grants GPU
/// capabilities uses the same defaults instead of picking tiers ad hoc.
pub fn default_tier(capability: Capability) -> PermissionTier {
    match capability {
        Capability::GpuAdmin => PermissionTier::Critical,
        Capability::GpuCommandSubmit => PermissionTier::High,
        Capability::GpuMemoryAlloc => PermissionTier::Medium,
        _ => PermissionTier::Low,
    }
}

/// Implemented by `graphics_runtime`. Applications and Mesa-facing
/// compatibility glue (`graphics_compat`) code against this trait only —
/// never against `gpu_abstraction` or a specific driver directly.
pub trait GraphicsApi {
    fn create_device(&mut self, gpu: ObjectId, caps: &CapabilitySet) -> Result<GraphicsDevice>;
    fn create_timeline(&mut self, device: &ObjectId) -> Result<Timeline>;
    fn create_resource(
        &mut self,
        device: &ObjectId,
        kind: ResourceKind,
        memory_class: MemoryClass,
    ) -> Result<Resource>;
    fn create_command_stream(
        &mut self,
        device: &ObjectId,
        class: WorkloadClass,
    ) -> Result<CommandStream>;
    fn create_shader_module(
        &mut self,
        device: &ObjectId,
        stage: ShaderStage,
        ir: Vec<u8>,
    ) -> Result<ShaderModule>;
    fn create_pipeline(
        &mut self,
        device: &ObjectId,
        shader: &ObjectId,
        class: WorkloadClass,
    ) -> Result<Pipeline>;
    fn submit(
        &mut self,
        queue: &QueueHandle,
        stream: CommandStream,
        timeline: &ObjectId,
        signal_value: u64,
    ) -> Result<()>;
    fn timeline_value(&self, timeline: &ObjectId) -> Result<u64>;
    fn wait(&self, timeline: &ObjectId, value: u64) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_stream_records_ops() {
        let mut stream = CommandStream {
            id: ObjectId::new(),
            device: ObjectId::new(),
            target_class: WorkloadClass::Graphics,
            ops: Vec::new(),
        };
        let pipeline = ObjectId::new();
        stream.push(GraphicsOp::BindPipeline(pipeline));
        stream.push(GraphicsOp::Draw { vertex_count: 3, instance_count: 1 });
        assert_eq!(stream.ops.len(), 2);
    }

    #[test]
    fn device_finds_queue_by_class() {
        let device = GraphicsDevice {
            id: ObjectId::new(),
            gpu: ObjectId::new(),
            queues: vec![
                QueueHandle { id: ObjectId::new(), class: WorkloadClass::Graphics },
                QueueHandle { id: ObjectId::new(), class: WorkloadClass::Compute },
            ],
        };
        assert!(device.queue(WorkloadClass::Graphics).is_some());
        assert!(device.queue(WorkloadClass::Transfer).is_none());
    }

    #[test]
    fn capability_check_rejects_missing_grant() {
        let caps = CapabilitySet::default();
        let result = require_capability(&caps, Capability::GpuCommandSubmit);
        assert!(result.is_err());
    }

    #[test]
    fn capability_check_accepts_valid_grant() {
        let mut caps = CapabilitySet::default();
        caps.grant(Capability::GpuCommandSubmit, PermissionTier::High);
        let result = require_capability(&caps, Capability::GpuCommandSubmit);
        assert!(result.is_ok());
    }

    #[test]
    fn default_tiers_match_capability_sensitivity() {
        assert_eq!(default_tier(Capability::GpuAdmin), PermissionTier::Critical);
        assert_eq!(default_tier(Capability::GpuCommandSubmit), PermissionTier::High);
        assert_eq!(default_tier(Capability::GpuMemoryAlloc), PermissionTier::Medium);
    }

    #[test]
    fn resource_kind_buffer_construction() {
        let resource = Resource {
            id: ObjectId::new(),
            device: ObjectId::new(),
            kind: ResourceKind::Buffer {
                size: 4096,
                usage: BufferUsage { uniform: true, ..Default::default() },
            },
            memory_class: MemoryClass::HostVisible,
        };
        match resource.kind {
            ResourceKind::Buffer { size, usage } => {
                assert_eq!(size, 4096);
                assert!(usage.uniform);
                assert!(!usage.vertex);
            }
            _ => panic!("expected buffer"),
        }
    }
}

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

use gpu_abstraction::{DriverOp, GpuDriver, GpuFault};
use gpu_driver::{Connector, GPUDriver};
pub use graphics_api::MemoryClass;
use graphics_api::{
    CommandStream, GraphicsApi, GraphicsDevice, GraphicsOp, Pipeline, PixelFormat, QueueHandle,
    Resource, ResourceKind, ShaderModule, ShaderStage, Timeline, WorkloadClass,
};
use sher_common::{Capability, Error, ObjectId, Result};
use sher_objectmodel::CapabilitySet;
use sher_security::audit::AuditLog;
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
        ResourceKind::Image {
            width,
            height,
            depth,
            format,
            ..
        } => (*width as usize) * (*height as usize) * (*depth as usize) * bytes_per_pixel(*format),
    }
}

/// Validates a command stream's binding state before it reaches the driver:
/// every `BindPipeline`/`BindResource` must reference a live object on the
/// same device, and `Draw`/`Dispatch` must be preceded by a `BindPipeline`.
/// This is where "the runtime decides the on-hardware binding-table
/// representation" (ARCHITECTURE.md section 3) starts — a real backend
/// would additionally compile this binding state into hardware descriptor
/// tables here, rather than passing it through.
fn validate_stream(
    stream: &CommandStream,
    pipelines: &HashMap<ObjectId, Pipeline>,
    resources: &HashMap<ObjectId, Resource>,
) -> Result<()> {
    let mut bound_pipeline: Option<ObjectId> = None;
    for op in &stream.ops {
        match op {
            GraphicsOp::BindPipeline(pipeline_id) => {
                let pipeline = pipelines.get(pipeline_id).ok_or_else(|| {
                    Error::Device("BindPipeline references unknown pipeline".to_string())
                })?;
                if pipeline.device != stream.device {
                    return Err(Error::Device(
                        "BindPipeline references a pipeline from a different device".to_string(),
                    ));
                }
                bound_pipeline = Some(*pipeline_id);
            }
            GraphicsOp::BindResource { resource, .. } => {
                let res = resources.get(resource).ok_or_else(|| {
                    Error::Device("BindResource references unknown resource".to_string())
                })?;
                if res.device != stream.device {
                    return Err(Error::Device(
                        "BindResource references a resource from a different device".to_string(),
                    ));
                }
            }
            GraphicsOp::Draw { .. } | GraphicsOp::Dispatch { .. } => {
                if bound_pipeline.is_none() {
                    return Err(Error::Device(
                        "Draw/Dispatch issued without a bound pipeline".to_string(),
                    ));
                }
            }
            GraphicsOp::Copy { src, dst } => {
                if !resources.contains_key(src) || !resources.contains_key(dst) {
                    return Err(Error::Device(
                        "Copy references unknown resource".to_string(),
                    ));
                }
            }
            GraphicsOp::Barrier => {}
        }
    }
    Ok(())
}

/// Narrows the app-facing `GraphicsOp` vocabulary down to what a driver
/// backend actually executes. `BindPipeline`/`BindResource` are runtime-side
/// state changes, not driver-level commands, so they don't cross this
/// boundary — see `gpu_abstraction::DriverOp` module docs. By the time
/// `translate_ops` runs, `validate_stream` has already confirmed every
/// binding was valid.
fn translate_ops(ops: &[GraphicsOp]) -> Vec<DriverOp> {
    ops.iter()
        .filter_map(|op| match op {
            GraphicsOp::Draw {
                vertex_count,
                instance_count,
            } => Some(DriverOp::Draw {
                vertex_count: *vertex_count,
                instance_count: *instance_count,
            }),
            GraphicsOp::Dispatch { x, y, z } => Some(DriverOp::Dispatch {
                x: *x,
                y: *y,
                z: *z,
            }),
            GraphicsOp::Copy { src, dst } => Some(DriverOp::Copy {
                src: *src,
                dst: *dst,
            }),
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
        Self {
            display: GPUDriver::new(display_vram_bytes),
        }
    }

    pub fn register_connector(&mut self, connector: Connector) -> Result<()> {
        self.display.register_connector(connector)
    }

    pub fn has_connector(&self, connector_id: &ObjectId) -> bool {
        self.display.get_connector(connector_id).is_some()
    }

    pub fn present(
        &mut self,
        connector_id: &ObjectId,
        width: u32,
        height: u32,
    ) -> Result<ObjectId> {
        let framebuffer = self.display.allocate_framebuffer(width, height)?;
        self.display.page_flip(connector_id, &framebuffer.id)?;
        Ok(framebuffer.id)
    }
}

/// The rendering-mechanism half of the cursor, per ARCHITECTURE.md
/// section 44: SHER-GRAPHICS knows how to efficiently render the cursor,
/// nothing more. `position` is a render-time coordinate SHER-DISPLAY hands
/// down every time it changes, not something SHER-GRAPHICS tracks from raw
/// input — there is deliberately no way to *derive* a `CursorState` from a
/// pointer event here, only to set one from the outside.
#[derive(Debug, Clone, Default)]
pub struct CursorState {
    pub image: Option<ObjectId>,
    pub position: (i32, i32),
    pub visible: bool,
}

pub struct GraphicsRuntime<D: GpuDriver> {
    driver: D,
    devices: HashMap<ObjectId, GraphicsDevice>,
    /// Capabilities granted at `create_device` time, re-checked per operation
    /// (currently: `GpuCommandSubmit` on `submit`) rather than trusted once
    /// and forgotten — see ARCHITECTURE.md section 16.
    device_caps: HashMap<ObjectId, CapabilitySet>,
    resources: HashMap<ObjectId, Resource>,
    shaders: HashMap<ObjectId, ShaderModule>,
    pipelines: HashMap<ObjectId, Pipeline>,
    timelines: HashMap<ObjectId, u64>,
    presentation: PresentationBridge,
    /// Reuses `sher_security`'s `AuditLog`/`AuditEvent` pattern, per
    /// ARCHITECTURE.md section 16 ("GPU capability grants, submissions from
    /// unusual contexts, and fault events all flow into the same audit
    /// trail"). Covers `create_device` (capability grant check),
    /// `submit` (per-submission authorization), and `recover_device`
    /// (fault-recovery trigger).
    ///
    /// `AuditEvent::actor` is the device/GPU `ObjectId` being acted on, not
    /// the calling process — `GraphicsApi` doesn't yet receive a caller
    /// identity (`CapabilitySet` carries grants but no owning `ObjectId`),
    /// so there is no true actor to record. Attributing the process that
    /// presented `caps` is tracked as follow-up work alongside that
    /// upstream gap.
    audit: AuditLog,
    /// Cursor rendering state per presentation connector — see `CursorState`.
    cursors: HashMap<ObjectId, CursorState>,
}

impl<D: GpuDriver> GraphicsRuntime<D> {
    pub fn new(driver: D, presentation_vram_bytes: usize) -> Self {
        Self {
            driver,
            devices: HashMap::new(),
            device_caps: HashMap::new(),
            resources: HashMap::new(),
            shaders: HashMap::new(),
            pipelines: HashMap::new(),
            timelines: HashMap::new(),
            presentation: PresentationBridge::new(presentation_vram_bytes),
            audit: AuditLog::default(),
            cursors: HashMap::new(),
        }
    }

    /// Read access to the audit trail — the foundation `sher-graphics-monitor`
    /// (ARCHITECTURE.md section 32) would query for capability-denial and
    /// fault-recovery history.
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit
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

    pub fn present_frame(
        &mut self,
        connector: &ObjectId,
        width: u32,
        height: u32,
    ) -> Result<ObjectId> {
        self.presentation.present(connector, width, height)
    }

    /// Looks up (or lazily creates) the cursor state for `connector`,
    /// rejecting connectors the presentation bridge doesn't know about —
    /// there is no cursor without a presentation target to render it on.
    fn cursor_mut(&mut self, connector: &ObjectId) -> Result<&mut CursorState> {
        if !self.presentation.has_connector(connector) {
            return Err(Error::Device("unknown presentation connector".to_string()));
        }
        Ok(self.cursors.entry(*connector).or_default())
    }

    /// Sets the image SHER-GRAPHICS renders as the cursor on `connector`.
    /// ARCHITECTURE.md section 44: this is purely "how to render it" —
    /// `image` must already be a registered `Image` resource; SHER-GRAPHICS
    /// never decides *which* image represents "the cursor" on its own.
    pub fn set_cursor_image(&mut self, connector: &ObjectId, image: &ObjectId) -> Result<()> {
        let resource = self
            .resources
            .get(image)
            .ok_or_else(|| Error::Device("unknown cursor image resource".to_string()))?;
        if !matches!(resource.kind, ResourceKind::Image { .. }) {
            return Err(Error::Device(
                "cursor image resource must be an Image, not a Buffer".to_string(),
            ));
        }
        self.cursor_mut(connector)?.image = Some(*image);
        Ok(())
    }

    /// Moves the rendered cursor to `(x, y)`. The coordinate is handed down
    /// by SHER-DISPLAY (which owns cursor policy and target) every time it
    /// changes — SHER-GRAPHICS does not compute or track it independently.
    pub fn set_cursor_position(&mut self, connector: &ObjectId, x: i32, y: i32) -> Result<()> {
        self.cursor_mut(connector)?.position = (x, y);
        Ok(())
    }

    pub fn show_cursor(&mut self, connector: &ObjectId) -> Result<()> {
        self.cursor_mut(connector)?.visible = true;
        Ok(())
    }

    pub fn hide_cursor(&mut self, connector: &ObjectId) -> Result<()> {
        self.cursor_mut(connector)?.visible = false;
        Ok(())
    }

    /// Returns the default (no image, hidden, origin position) `CursorState`
    /// for a connector that's registered but has never had a cursor
    /// primitive called on it, rather than treating "no cursor set yet" as
    /// an error the way an unregistered connector is.
    pub fn cursor_state(&self, connector: &ObjectId) -> Result<CursorState> {
        if !self.presentation.has_connector(connector) {
            return Err(Error::Device("unknown presentation connector".to_string()));
        }
        Ok(self.cursors.get(connector).cloned().unwrap_or_default())
    }

    /// Escape hatch to the underlying driver, for test/debug hooks
    /// (`SoftwareGpuDriver::inject_fault`) and admin tooling (driver reload,
    /// power-state changes) named alongside fault recovery under
    /// `Capability::GpuAdmin` in ARCHITECTURE.md section 16. Not part of
    /// `GraphicsApi` — the portable API surface never needs driver-specific
    /// methods; this is deliberately a separate, narrower door.
    pub fn driver_mut(&mut self, caps: &CapabilitySet) -> Result<&mut D> {
        graphics_api::require_capability(caps, Capability::GpuAdmin)?;
        Ok(&mut self.driver)
    }

    /// Reports whether the GPU backing `device` has faulted. See
    /// ARCHITECTURE.md section 18: faults surface through the normal
    /// `Result`/`Error` channel on `submit`, but this lets callers poll
    /// state without attempting a submission first.
    pub fn device_fault(&self, device: &ObjectId) -> Result<Option<GpuFault>> {
        let gpu = self.device(device)?.gpu;
        self.driver.fault_status(&gpu)
    }

    /// Recovers from a GPU fault by isolating and tearing down *only* the
    /// faulting context — matching the kernel-wide "one driver's failure
    /// doesn't crash others" principle (ARCHITECTURE.md section 18). The
    /// underlying GPU is reset at the driver level, and the runtime-level
    /// `GraphicsDevice` is discarded: its id becomes invalid for further
    /// calls, so callers must `create_device` again rather than resume the
    /// faulted context as if nothing happened. Resources, shaders, and
    /// pipelines that belonged to it are dropped along with it, since
    /// their validity was tied to a context that no longer exists.
    ///
    /// Gated on `Capability::GpuAdmin` — ARCHITECTURE.md section 16 lists
    /// "fault-recovery triggers" explicitly as a `GpuAdmin` (Tier 4,
    /// Critical) operation, distinct from the `GpuMemoryAlloc`/
    /// `GpuCommandSubmit` capabilities regular device users hold. Checked
    /// against the capability set the *caller* presents now, not the set
    /// the faulted device was created with — an app's own crashed context
    /// should not be recoverable by the app itself.
    ///
    /// Timelines are not yet torn down here — the timeline table only
    /// tracks id → current value, not which device created it. A timeline
    /// belonging to a recovered device simply stops advancing (its
    /// `device` no longer resolves), rather than being explicitly freed;
    /// tracked as follow-up work alongside a real timeline registry.
    pub fn recover_device(&mut self, device: &ObjectId, caps: &CapabilitySet) -> Result<()> {
        let authorized = graphics_api::require_capability(caps, Capability::GpuAdmin);
        self.audit
            .log(*device, "recover_device", authorized.is_ok());
        authorized?;
        let gpu = self.device(device)?.gpu;
        self.driver.reset_device(&gpu)?;

        self.devices.remove(device);
        self.device_caps.remove(device);
        self.resources.retain(|_, r| &r.device != device);
        self.shaders.retain(|_, s| &s.device != device);
        self.pipelines.retain(|_, p| &p.device != device);
        Ok(())
    }

    /// Driver crash containment (external critique, verified real gap — see
    /// README.md's Known Issues): a real panic inside driver code used to
    /// unwind straight through this call and into the caller, taking down
    /// the whole process with it. `submit` is the hot, per-frame path where
    /// a malformed command stream reaching a driver backend is most likely
    /// to trigger one, so it's caught here via `catch_unwind` and converted
    /// into the same `GpuFault` channel a hardware-reported fault already
    /// uses — the device is marked faulted (via `GpuDriver::mark_faulted`)
    /// and the caller gets a normal `Err`, not a crash. Recovering from it
    /// is the existing `recover_device` path: this does not, on its own,
    /// resume the faulted context, matching how a hardware fault already
    /// behaves.
    ///
    /// `AssertUnwindSafe` is required because `&mut self.driver` isn't
    /// `UnwindSafe` by default (a panic mid-mutation could leave it in an
    /// inconsistent state) — that's precisely why the caught path doesn't
    /// try to resume normal operation on the same device afterward, only
    /// mark-and-report, leaving `reset_device`/`recover_device` (the
    /// existing, already-audited recovery path) as the one way back to a
    /// submittable device.
    ///
    /// Still not process/WASM/eBPF-level isolation — see README.md's Known
    /// Issues for what that would require and why it lives in SHER-Kernel's
    /// `driver_runtime`, not here.
    fn submit_to_driver(
        &mut self,
        gpu: &ObjectId,
        class: WorkloadClass,
        ops: &[gpu_abstraction::DriverOp],
    ) -> Result<()> {
        let driver = &mut self.driver;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            driver.submit(gpu, class, ops)
        })) {
            Ok(result) => result,
            Err(panic_payload) => {
                let description = panic_message(&*panic_payload);
                self.driver.mark_faulted(gpu, description.clone());
                Err(Error::Device(format!(
                    "driver panicked during submit: {description}"
                )))
            }
        }
    }
}

/// Best-effort extraction of a human-readable message from a caught panic
/// payload. `panic!("...")`/`.unwrap()`/`.expect("...")` all produce either
/// `&'static str` or `String` payloads in practice; anything else (a custom
/// `panic_any` with another type) falls back to a fixed message rather than
/// failing to report the fault at all.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "driver panicked with a non-string payload".to_string()
    }
}

impl<D: GpuDriver> GraphicsApi for GraphicsRuntime<D> {
    fn create_device(&mut self, gpu: ObjectId, caps: &CapabilitySet) -> Result<GraphicsDevice> {
        let authorized = graphics_api::require_capability(caps, Capability::GpuMemoryAlloc);
        self.audit.log(gpu, "create_device", authorized.is_ok());
        authorized?;

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
                QueueHandle {
                    id: ObjectId::new(),
                    class: WorkloadClass::Graphics,
                },
                QueueHandle {
                    id: ObjectId::new(),
                    class: WorkloadClass::Compute,
                },
                QueueHandle {
                    id: ObjectId::new(),
                    class: WorkloadClass::Transfer,
                },
            ],
        };
        self.devices.insert(device.id, device.clone());
        self.device_caps.insert(device.id, caps.clone());
        Ok(device)
    }

    fn create_timeline(&mut self, device: &ObjectId) -> Result<Timeline> {
        self.device(device)?;
        let timeline = Timeline {
            id: ObjectId::new(),
            device: *device,
            current_value: 0,
        };
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
        let resource = Resource {
            id: allocation.id,
            device: *device,
            kind,
            memory_class,
        };
        self.resources.insert(resource.id, resource.clone());
        Ok(resource)
    }

    fn create_command_stream(
        &mut self,
        device: &ObjectId,
        class: WorkloadClass,
    ) -> Result<CommandStream> {
        self.device(device)?;
        Ok(CommandStream {
            id: ObjectId::new(),
            device: *device,
            target_class: class,
            ops: Vec::new(),
        })
    }

    fn create_shader_module(
        &mut self,
        device: &ObjectId,
        stage: ShaderStage,
        ir: Vec<u8>,
    ) -> Result<ShaderModule> {
        self.device(device)?;
        let module = ShaderModule {
            id: ObjectId::new(),
            device: *device,
            stage,
            ir,
        };
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
        let pipeline = Pipeline {
            id: ObjectId::new(),
            device: *device,
            class,
            shader: *shader,
        };
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
        let caps = self
            .device_caps
            .get(&stream.device)
            .ok_or_else(|| Error::Device("unknown graphics device".to_string()))?;
        let authorized = graphics_api::require_capability(caps, Capability::GpuCommandSubmit);
        self.audit.log(stream.device, "submit", authorized.is_ok());
        authorized?;
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
        validate_stream(&stream, &self.pipelines, &self.resources)?;

        let driver_ops = translate_ops(&stream.ops);
        self.submit_to_driver(&gpu, queue.class, &driver_ops)?;

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
    use graphics_api::{BufferUsage, ImageUsage};
    use sher_common::PermissionTier;

    fn caps_with(cap: Capability) -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.grant(cap, PermissionTier::High);
        caps
    }

    /// `GpuMemoryAlloc` (required by `create_device`) plus `GpuCommandSubmit`
    /// (required by `submit`) — the baseline grant most tests need, since
    /// most tests exercise more than just device creation.
    fn full_caps() -> CapabilitySet {
        let mut caps = CapabilitySet::default();
        caps.grant(Capability::GpuMemoryAlloc, PermissionTier::High);
        caps.grant(Capability::GpuCommandSubmit, PermissionTier::High);
        caps
    }

    /// `GpuAdmin` only — deliberately *not* combined with `full_caps()`, so
    /// tests that exercise `recover_device` prove the admin grant is what's
    /// authorizing recovery, not incidental possession of the regular
    /// device-user capabilities.
    fn admin_caps() -> CapabilitySet {
        caps_with(Capability::GpuAdmin)
    }

    fn runtime_with_device() -> (GraphicsRuntime<SoftwareGpuDriver>, GraphicsDevice) {
        let driver = SoftwareGpuDriver::new(64 * 1024 * 1024);
        let gpu = driver.device_id();
        let mut runtime = GraphicsRuntime::new(driver, 16 * 1024 * 1024);
        let device = runtime.create_device(gpu, &full_caps()).unwrap();
        (runtime, device)
    }

    #[test]
    fn create_device_requires_capability() {
        let driver = SoftwareGpuDriver::new(1024);
        let gpu = driver.device_id();
        let mut runtime = GraphicsRuntime::new(driver, 1024);
        let result = runtime.create_device(gpu, &CapabilitySet::default());
        assert!(result.is_err());

        let entry = runtime.audit_log().events.last().unwrap();
        assert_eq!(entry.actor, gpu);
        assert_eq!(entry.action, "create_device");
        assert!(!entry.result);
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
                ResourceKind::Buffer {
                    size: 4096,
                    usage: BufferUsage::default(),
                },
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
            ResourceKind::Buffer {
                size: 4096,
                usage: BufferUsage::default(),
            },
            MemoryClass::DeviceLocal,
        );
        assert!(result.is_err());
    }

    #[test]
    fn submit_advances_timeline() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Graphics)
            .unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        runtime.submit(&queue, stream, &timeline.id, 1).unwrap();
        assert_eq!(runtime.timeline_value(&timeline.id).unwrap(), 1);
        assert!(runtime.wait(&timeline.id, 1).is_ok());
        assert!(runtime.wait(&timeline.id, 2).is_err());
    }

    #[test]
    fn submit_requires_gpu_command_submit_capability() {
        let driver = SoftwareGpuDriver::new(64 * 1024 * 1024);
        let gpu = driver.device_id();
        let mut runtime = GraphicsRuntime::new(driver, 1024);
        // Only GpuMemoryAlloc granted - enough to create the device, not
        // enough to submit work on it.
        let device = runtime
            .create_device(gpu, &caps_with(Capability::GpuMemoryAlloc))
            .unwrap();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Graphics)
            .unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        let result = runtime.submit(&queue, stream, &timeline.id, 1);
        assert!(result.is_err());

        let entry = runtime.audit_log().events.last().unwrap();
        assert_eq!(entry.actor, device.id);
        assert_eq!(entry.action, "submit");
        assert!(!entry.result);
    }

    #[test]
    fn audit_log_records_authorized_and_denied_gpu_admin_operations() {
        let (mut runtime, device) = runtime_with_device();

        // Denied: full_caps() lacks GpuAdmin.
        assert!(runtime.recover_device(&device.id, &full_caps()).is_err());
        // Authorized: admin_caps() grants it.
        runtime.recover_device(&device.id, &admin_caps()).unwrap();

        let events = &runtime.audit_log().events;
        let recover_events: Vec<_> = events
            .iter()
            .filter(|e| e.action == "recover_device")
            .collect();
        assert_eq!(recover_events.len(), 2);
        assert!(!recover_events[0].result);
        assert!(recover_events[1].result);
        assert!(recover_events.iter().all(|e| e.actor == device.id));
    }

    #[test]
    fn submit_rejects_mismatched_workload_class() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Compute)
            .unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        let result = runtime.submit(&queue, stream, &timeline.id, 1);
        assert!(result.is_err());
    }

    #[test]
    fn submit_requires_monotonic_timeline_value() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let queue = device.queue(WorkloadClass::Transfer).unwrap();

        let first = runtime
            .create_command_stream(&device.id, WorkloadClass::Transfer)
            .unwrap();
        runtime.submit(&queue, first, &timeline.id, 5).unwrap();

        let second = runtime
            .create_command_stream(&device.id, WorkloadClass::Transfer)
            .unwrap();
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

    fn registered_connector(runtime: &mut GraphicsRuntime<SoftwareGpuDriver>) -> ObjectId {
        let connector = Connector {
            id: ObjectId::new(),
            connector_type: ConnectorType::HDMI,
            status: ConnectorStatus::Connected,
            supported_modes: vec![],
            current_mode: None,
        };
        let connector_id = connector.id;
        runtime.register_connector(connector).unwrap();
        connector_id
    }

    #[test]
    fn cursor_state_defaults_hidden_with_no_image() {
        let (mut runtime, _device) = runtime_with_device();
        let connector_id = registered_connector(&mut runtime);

        let state = runtime.cursor_state(&connector_id).unwrap();
        assert!(state.image.is_none());
        assert!(!state.visible);
        assert_eq!(state.position, (0, 0));
    }

    #[test]
    fn cursor_state_requires_known_connector() {
        let (runtime, _device) = runtime_with_device();
        assert!(runtime.cursor_state(&ObjectId::new()).is_err());
    }

    #[test]
    fn cursor_primitives_update_state_without_touching_input_or_display_semantics() {
        let (mut runtime, device) = runtime_with_device();
        let connector_id = registered_connector(&mut runtime);
        let image = runtime
            .create_resource(
                &device.id,
                ResourceKind::Image {
                    width: 32,
                    height: 32,
                    depth: 1,
                    format: PixelFormat::Rgba8Unorm,
                    usage: ImageUsage {
                        sampled: true,
                        ..Default::default()
                    },
                },
                MemoryClass::DeviceLocal,
            )
            .unwrap();

        runtime.set_cursor_image(&connector_id, &image.id).unwrap();
        runtime.set_cursor_position(&connector_id, 42, 17).unwrap();
        runtime.show_cursor(&connector_id).unwrap();

        let state = runtime.cursor_state(&connector_id).unwrap();
        assert_eq!(state.image, Some(image.id));
        assert_eq!(state.position, (42, 17));
        assert!(state.visible);

        runtime.hide_cursor(&connector_id).unwrap();
        assert!(!runtime.cursor_state(&connector_id).unwrap().visible);
    }

    #[test]
    fn set_cursor_image_rejects_buffer_resource() {
        let (mut runtime, device) = runtime_with_device();
        let connector_id = registered_connector(&mut runtime);
        let buffer = runtime
            .create_resource(
                &device.id,
                ResourceKind::Buffer {
                    size: 64,
                    usage: BufferUsage::default(),
                },
                MemoryClass::HostVisible,
            )
            .unwrap();

        assert!(runtime.set_cursor_image(&connector_id, &buffer.id).is_err());
    }

    #[test]
    fn set_cursor_image_rejects_unknown_resource() {
        let (mut runtime, _device) = runtime_with_device();
        let connector_id = registered_connector(&mut runtime);
        assert!(runtime
            .set_cursor_image(&connector_id, &ObjectId::new())
            .is_err());
    }

    #[test]
    fn cursor_primitives_reject_unknown_connector() {
        let (mut runtime, _device) = runtime_with_device();
        let unknown = ObjectId::new();
        assert!(runtime.set_cursor_position(&unknown, 1, 1).is_err());
        assert!(runtime.show_cursor(&unknown).is_err());
        assert!(runtime.hide_cursor(&unknown).is_err());
    }

    #[test]
    fn submit_rejects_draw_without_bound_pipeline() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        let mut stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Graphics)
            .unwrap();
        stream.push(GraphicsOp::Draw {
            vertex_count: 3,
            instance_count: 1,
        });

        let result = runtime.submit(&queue, stream, &timeline.id, 1);
        assert!(result.is_err());
    }

    #[test]
    fn submit_accepts_draw_after_bind_pipeline() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();
        let shader = runtime
            .create_shader_module(&device.id, ShaderStage::Vertex, vec![0u8; 4])
            .unwrap();
        let pipeline = runtime
            .create_pipeline(&device.id, &shader.id, WorkloadClass::Graphics)
            .unwrap();

        let mut stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Graphics)
            .unwrap();
        stream.push(GraphicsOp::BindPipeline(pipeline.id));
        stream.push(GraphicsOp::Draw {
            vertex_count: 3,
            instance_count: 1,
        });

        assert!(runtime.submit(&queue, stream, &timeline.id, 1).is_ok());
    }

    #[test]
    fn submit_rejects_bind_pipeline_unknown() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        let mut stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Graphics)
            .unwrap();
        stream.push(GraphicsOp::BindPipeline(ObjectId::new()));

        let result = runtime.submit(&queue, stream, &timeline.id, 1);
        assert!(result.is_err());
    }

    #[test]
    fn submit_rejects_bind_resource_unknown() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        let mut stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Graphics)
            .unwrap();
        stream.push(GraphicsOp::BindResource {
            binding: 0,
            resource: ObjectId::new(),
        });

        let result = runtime.submit(&queue, stream, &timeline.id, 1);
        assert!(result.is_err());
    }

    #[test]
    fn submit_rejects_copy_with_unknown_resource() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let queue = device.queue(WorkloadClass::Transfer).unwrap();

        let mut stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Transfer)
            .unwrap();
        stream.push(GraphicsOp::Copy {
            src: ObjectId::new(),
            dst: ObjectId::new(),
        });

        let result = runtime.submit(&queue, stream, &timeline.id, 1);
        assert!(result.is_err());
    }

    #[test]
    fn device_fault_reports_none_when_healthy() {
        let (runtime, device) = runtime_with_device();
        assert!(runtime.device_fault(&device.id).unwrap().is_none());
    }

    #[test]
    fn faulted_device_rejects_submit_until_recovered() {
        let (mut runtime, device) = runtime_with_device();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let queue = device.queue(WorkloadClass::Transfer).unwrap();

        runtime.driver_mut(&admin_caps()).unwrap().inject_fault(
            &device.gpu,
            0xdead,
            "simulated hang",
        );
        assert!(runtime.device_fault(&device.id).unwrap().is_some());

        let stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Transfer)
            .unwrap();
        let result = runtime.submit(&queue, stream, &timeline.id, 1);
        assert!(result.is_err());

        runtime.recover_device(&device.id, &admin_caps()).unwrap();

        // The faulting context was torn down, not resumed: the old device
        // id is no longer valid, matching ARCHITECTURE.md section 18.
        let stream = runtime.create_command_stream(&device.id, WorkloadClass::Transfer);
        assert!(stream.is_err());
    }

    #[test]
    fn recover_device_drops_its_resources_and_pipelines() {
        let (mut runtime, device) = runtime_with_device();
        let shader = runtime
            .create_shader_module(&device.id, ShaderStage::Vertex, vec![0u8; 4])
            .unwrap();
        let pipeline = runtime
            .create_pipeline(&device.id, &shader.id, WorkloadClass::Graphics)
            .unwrap();
        let resource = runtime
            .create_resource(
                &device.id,
                ResourceKind::Buffer {
                    size: 256,
                    usage: BufferUsage::default(),
                },
                MemoryClass::HostVisible,
            )
            .unwrap();

        runtime
            .driver_mut(&admin_caps())
            .unwrap()
            .inject_fault(&device.gpu, 0x1, "fault");
        runtime.recover_device(&device.id, &admin_caps()).unwrap();

        assert!(!runtime.pipelines.contains_key(&pipeline.id));
        assert!(!runtime.shaders.contains_key(&shader.id));
        assert!(!runtime.resources.contains_key(&resource.id));
    }

    #[test]
    fn recovering_unknown_device_fails() {
        let (mut runtime, _device) = runtime_with_device();
        assert!(runtime
            .recover_device(&ObjectId::new(), &admin_caps())
            .is_err());
    }

    #[test]
    fn driver_mut_requires_gpu_admin_capability() {
        let (mut runtime, _device) = runtime_with_device();

        // `full_caps()` is what an ordinary device user holds
        // (GpuMemoryAlloc + GpuCommandSubmit) — the driver escape hatch is
        // narrower than that, matching ARCHITECTURE.md section 16's
        // GpuAdmin/Tier 4 gate on driver-level admin operations.
        assert!(runtime.driver_mut(&full_caps()).is_err());
        assert!(runtime.driver_mut(&admin_caps()).is_ok());
    }

    #[test]
    fn recover_device_requires_gpu_admin_capability() {
        let (mut runtime, device) = runtime_with_device();
        runtime
            .driver_mut(&admin_caps())
            .unwrap()
            .inject_fault(&device.gpu, 0x1, "fault");

        // `full_caps()` grants `GpuMemoryAlloc` + `GpuCommandSubmit` — the
        // capabilities an ordinary device user holds — but not `GpuAdmin`,
        // so a faulted app's own context is not self-recoverable.
        let result = runtime.recover_device(&device.id, &full_caps());
        assert!(result.is_err());

        // The device is still faulted and was not torn down by the
        // rejected call.
        assert!(runtime.device_fault(&device.id).unwrap().is_some());

        runtime.recover_device(&device.id, &admin_caps()).unwrap();
        assert!(runtime.device(&device.id).is_err());
    }

    /// Test double: identical to `SoftwareGpuDriver` for everything except
    /// `submit`, which always panics — used to prove `GraphicsRuntime::submit`
    /// catches a driver panic instead of letting it unwind through the
    /// caller (the driver-crash-containment gap this pass fixes; see
    /// README.md's Known Issues).
    struct PanickingDriver {
        inner: SoftwareGpuDriver,
    }

    impl PanickingDriver {
        fn new(vram_bytes: usize) -> Self {
            Self {
                inner: SoftwareGpuDriver::new(vram_bytes),
            }
        }

        fn device_id(&self) -> ObjectId {
            self.inner.device_id()
        }
    }

    impl hal::HardwareDriver for PanickingDriver {
        fn probe(&self) -> Result<Vec<hal::DeviceInfo>> {
            self.inner.probe()
        }
        fn initialize(&mut self, device: &hal::DeviceInfo) -> Result<()> {
            self.inner.initialize(device)
        }
        fn shutdown(&mut self, device_id: &ObjectId) -> Result<()> {
            self.inner.shutdown(device_id)
        }
        fn get_capabilities(&self, device_id: &ObjectId) -> Result<Vec<String>> {
            self.inner.get_capabilities(device_id)
        }
        fn read_register(&self, device_id: &ObjectId, offset: usize) -> Result<u32> {
            self.inner.read_register(device_id, offset)
        }
        fn write_register(&self, device_id: &ObjectId, offset: usize, value: u32) -> Result<()> {
            self.inner.write_register(device_id, offset, value)
        }
    }

    impl GpuDriver for PanickingDriver {
        fn allocate_vram(
            &mut self,
            device: &ObjectId,
            size: usize,
            class: MemoryClass,
        ) -> Result<gpu_abstraction::GpuAllocation> {
            self.inner.allocate_vram(device, size, class)
        }
        fn free_vram(&mut self, allocation: &ObjectId) -> Result<()> {
            self.inner.free_vram(allocation)
        }
        fn map_dma(&mut self, allocation: &ObjectId) -> Result<gpu_abstraction::DmaMapping> {
            self.inner.map_dma(allocation)
        }
        fn submit(
            &mut self,
            _device: &ObjectId,
            _class: WorkloadClass,
            _ops: &[DriverOp],
        ) -> Result<()> {
            panic!("simulated driver panic during submit");
        }
        fn fault_status(&self, device: &ObjectId) -> Result<Option<GpuFault>> {
            self.inner.fault_status(device)
        }
        fn vram_available(&self, device: &ObjectId) -> Result<usize> {
            self.inner.vram_available(device)
        }
        fn reset_device(&mut self, device: &ObjectId) -> Result<()> {
            self.inner.reset_device(device)
        }
        fn mark_faulted(&mut self, device: &ObjectId, description: String) {
            self.inner.mark_faulted(device, description)
        }
    }

    #[test]
    fn submit_catches_a_driver_panic_instead_of_crashing() {
        let driver = PanickingDriver::new(64 * 1024 * 1024);
        let gpu = driver.device_id();
        let mut runtime = GraphicsRuntime::new(driver, 16 * 1024 * 1024);
        let device = runtime.create_device(gpu, &full_caps()).unwrap();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Graphics)
            .unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        // If the panic escaped `submit`, this call itself would unwind the
        // test thread instead of returning -- reaching the assertions below
        // at all is part of what this test proves.
        let result = runtime.submit(&queue, stream, &timeline.id, 1);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("driver panicked"));
    }

    #[test]
    fn a_caught_driver_panic_marks_the_device_faulted_and_is_hot_restartable() {
        let driver = PanickingDriver::new(64 * 1024 * 1024);
        let gpu = driver.device_id();
        let mut runtime = GraphicsRuntime::new(driver, 16 * 1024 * 1024);
        let device = runtime.create_device(gpu, &full_caps()).unwrap();
        let timeline = runtime.create_timeline(&device.id).unwrap();
        let stream = runtime
            .create_command_stream(&device.id, WorkloadClass::Graphics)
            .unwrap();
        let queue = device.queue(WorkloadClass::Graphics).unwrap();

        assert!(runtime.submit(&queue, stream, &timeline.id, 1).is_err());

        // The caught panic surfaces through the exact same fault channel a
        // hardware-reported fault would.
        assert!(runtime.device_fault(&device.id).unwrap().is_some());

        // Hot-restart: the existing GpuAdmin-gated recovery path tears down
        // the faulted context without taking down the runtime (or the
        // process) -- a fresh device can be created and used immediately
        // after, on the same underlying GPU.
        runtime.recover_device(&device.id, &admin_caps()).unwrap();
        assert!(runtime.device(&device.id).is_err());

        let new_device = runtime.create_device(gpu, &full_caps()).unwrap();
        assert_ne!(new_device.id, device.id);
    }

    #[test]
    fn runtime_supports_two_independent_devices() {
        let driver = SoftwareGpuDriver::with_devices(2, 4096);
        let gpu_ids = driver.device_ids();
        let mut runtime = GraphicsRuntime::new(driver, 1024);

        let device_a = runtime.create_device(gpu_ids[0], &full_caps()).unwrap();
        let device_b = runtime.create_device(gpu_ids[1], &full_caps()).unwrap();
        assert_ne!(device_a.id, device_b.id);

        // Exhaust device A's VRAM entirely - device B must be unaffected.
        runtime
            .create_resource(
                &device_a.id,
                ResourceKind::Buffer {
                    size: 4096,
                    usage: BufferUsage::default(),
                },
                MemoryClass::DeviceLocal,
            )
            .unwrap();
        assert!(runtime
            .create_resource(
                &device_a.id,
                ResourceKind::Buffer {
                    size: 1,
                    usage: BufferUsage::default()
                },
                MemoryClass::DeviceLocal,
            )
            .is_err());
        assert!(runtime
            .create_resource(
                &device_b.id,
                ResourceKind::Buffer {
                    size: 4096,
                    usage: BufferUsage::default()
                },
                MemoryClass::DeviceLocal,
            )
            .is_ok());

        // A fault on device A must not block submission on device B.
        runtime.driver_mut(&admin_caps()).unwrap().inject_fault(
            &device_a.gpu,
            0xbad,
            "device A hang",
        );
        assert!(runtime.device_fault(&device_a.id).unwrap().is_some());
        assert!(runtime.device_fault(&device_b.id).unwrap().is_none());

        let timeline_b = runtime.create_timeline(&device_b.id).unwrap();
        let stream_b = runtime
            .create_command_stream(&device_b.id, WorkloadClass::Transfer)
            .unwrap();
        let queue_b = device_b.queue(WorkloadClass::Transfer).unwrap();
        assert!(runtime
            .submit(&queue_b, stream_b, &timeline_b.id, 1)
            .is_ok());
    }

    #[test]
    fn free_resource_returns_vram() {
        let (mut runtime, device) = runtime_with_device();
        let resource = runtime
            .create_resource(
                &device.id,
                ResourceKind::Buffer {
                    size: 4096,
                    usage: BufferUsage::default(),
                },
                MemoryClass::HostVisible,
            )
            .unwrap();
        assert!(runtime.free_resource(&resource.id).is_ok());
        assert!(runtime.free_resource(&resource.id).is_err());
    }
}

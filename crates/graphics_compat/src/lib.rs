//! Mesa compatibility seam
//!
//! This is deliberately the thinnest crate in the workspace — see
//! ARCHITECTURE.md section 5: almost everything Mesa needs (NIR, the GLSL
//! and SPIR-V front ends, shader compiler backends, Gallium pipe drivers,
//! Vulkan ICDs) is reused completely unmodified. The only SHER-specific
//! code is the seam Mesa's winsys/WSI code talks to.
//!
//! This crate models that seam server-side, for **Phase A** of the
//! migration strategy (ARCHITECTURE.md section 20): a DRM-ioctl-compatible
//! surface, answered by `graphics_runtime` instead of a real Linux DRM
//! driver, so an unmodified Mesa winsys can talk to it exactly as it would
//! talk to `/dev/dri/cardN`.
//!
//! The Vulkan-side equivalent (a small WSI platform backend living in
//! Mesa's own C tree, analogous to its existing X11/Wayland/Android
//! backends) is out of scope for this crate — it would call into the same
//! `graphics_runtime::GraphicsRuntime` this shim wraps, once written.
//!
//! Real ioctl numbers/struct layouts are intentionally not modeled here;
//! `DrmRequest`/`DrmResponse` name the operations Mesa's winsys actually
//! needs (GEM allocation lifecycle, PRIME/DMA-buf export, mode resource
//! enumeration) without committing to a wire format yet.

use gpu_abstraction::GpuDriver;
use graphics_api::{BufferUsage, GraphicsApi, MemoryClass, ResourceKind};
use graphics_runtime::GraphicsRuntime;
use sher_common::{ObjectId, Result};

#[derive(Debug, Clone)]
pub enum DrmRequest {
    /// Analogous to `DRM_IOCTL_GEM_CREATE` — allocate a GPU buffer object.
    GemCreate { device: ObjectId, size: usize },
    /// Analogous to `DRM_IOCTL_GEM_CLOSE`.
    GemClose { handle: ObjectId },
    /// Analogous to `DRM_IOCTL_PRIME_HANDLE_TO_FD` — export for zero-copy
    /// sharing with the compositor. A real shim returns an actual dma-buf
    /// file descriptor; this reference shim returns the resource id it
    /// stands in for.
    PrimeHandleToFd { handle: ObjectId },
    /// Analogous to `DRM_IOCTL_MODE_GETRESOURCES`.
    ///
    /// **Unimplemented stub, by design, not by oversight.** The handler for
    /// this request always returns an empty resource list
    /// (`DrmResponse::Resources(Vec::new())`) regardless of what
    /// devices/connectors actually exist in the wrapped `GraphicsRuntime` —
    /// it does not query real connector/CRTC/encoder state at all. This is
    /// intentional for a reference/compat shim with no real DRM-ioctl wire
    /// format behind it yet (see the module docs), but it means: if
    /// anything ever expects this to enumerate real mode resources (e.g. a
    /// real Mesa winsys pointed at this shim), it will silently see zero
    /// resources rather than an error. As of this writing nothing in
    /// SHER-Graphics, SHER-Display, or SHER-Kernel calls this — see
    /// `ROADMAP_HONEST.md` for the verified-empty-callers note. Implementing
    /// real enumeration is real feature work (querying actual connector
    /// state), not a quick fix, and is intentionally out of scope here.
    ModeGetResources,
}

#[derive(Debug, Clone)]
pub enum DrmResponse {
    GemHandle(ObjectId),
    Closed,
    PrimeFd(ObjectId),
    Resources(Vec<ObjectId>),
}

pub struct DrmCompatShim<'a, D: GpuDriver> {
    runtime: &'a mut GraphicsRuntime<D>,
}

impl<'a, D: GpuDriver> DrmCompatShim<'a, D> {
    pub fn new(runtime: &'a mut GraphicsRuntime<D>) -> Self {
        Self { runtime }
    }

    pub fn handle(&mut self, request: DrmRequest) -> Result<DrmResponse> {
        match request {
            DrmRequest::GemCreate { device, size } => {
                let resource = self.runtime.create_resource(
                    &device,
                    ResourceKind::Buffer {
                        size,
                        usage: BufferUsage::default(),
                    },
                    MemoryClass::DeviceLocal,
                )?;
                Ok(DrmResponse::GemHandle(resource.id))
            }
            DrmRequest::GemClose { handle } => {
                self.runtime.free_resource(&handle)?;
                Ok(DrmResponse::Closed)
            }
            DrmRequest::PrimeHandleToFd { handle } => Ok(DrmResponse::PrimeFd(handle)),
            // Unimplemented stub by design: see the doc comment on
            // `DrmRequest::ModeGetResources`. Always empty, never queries
            // real connector/CRTC/encoder state.
            DrmRequest::ModeGetResources => Ok(DrmResponse::Resources(Vec::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpu_abstraction::SoftwareGpuDriver;
    use sher_common::{Capability, PermissionTier};
    use sher_objectmodel::CapabilitySet;

    fn shim_ready() -> (GraphicsRuntime<SoftwareGpuDriver>, ObjectId) {
        let driver = SoftwareGpuDriver::new(16 * 1024 * 1024);
        let gpu = driver.device_id();
        let mut runtime = GraphicsRuntime::new(driver, 1024);
        let mut caps = CapabilitySet::default();
        caps.grant(Capability::GpuMemoryAlloc, PermissionTier::High);
        let device = runtime.create_device(gpu, &caps).unwrap();
        (runtime, device.id)
    }

    #[test]
    fn gem_create_and_close_round_trip() {
        let (mut runtime, device) = shim_ready();
        let mut shim = DrmCompatShim::new(&mut runtime);

        let handle = match shim
            .handle(DrmRequest::GemCreate { device, size: 4096 })
            .unwrap()
        {
            DrmResponse::GemHandle(id) => id,
            other => panic!("unexpected response: {other:?}"),
        };

        let response = shim.handle(DrmRequest::GemClose { handle }).unwrap();
        assert!(matches!(response, DrmResponse::Closed));
    }

    #[test]
    fn prime_export_returns_handle() {
        let (mut runtime, device) = shim_ready();
        let mut shim = DrmCompatShim::new(&mut runtime);

        let handle = match shim
            .handle(DrmRequest::GemCreate { device, size: 1024 })
            .unwrap()
        {
            DrmResponse::GemHandle(id) => id,
            other => panic!("unexpected response: {other:?}"),
        };

        let response = shim.handle(DrmRequest::PrimeHandleToFd { handle }).unwrap();
        assert!(matches!(response, DrmResponse::PrimeFd(id) if id == handle));
    }

    #[test]
    fn mode_get_resources_returns_empty_by_default() {
        let (mut runtime, _device) = shim_ready();
        let mut shim = DrmCompatShim::new(&mut runtime);

        let response = shim.handle(DrmRequest::ModeGetResources).unwrap();
        assert!(matches!(response, DrmResponse::Resources(ref v) if v.is_empty()));
    }

    #[test]
    fn gem_create_fails_for_unknown_device() {
        let (mut runtime, _device) = shim_ready();
        let mut shim = DrmCompatShim::new(&mut runtime);

        let result = shim.handle(DrmRequest::GemCreate {
            device: ObjectId::new(),
            size: 4096,
        });
        assert!(result.is_err());
    }
}

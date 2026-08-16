//! Real Vulkan backend for SHER Graphics.
//!
//! ## What this crate is, honestly
//!
//! Every other crate in this workspace (`graphics_api`, `gpu_abstraction`,
//! `graphics_runtime`, `graphics_compat`) is a pure-Rust, zero-`unsafe`,
//! zero-FFI software simulation of a GPU stack: `SoftwareGpuDriver` accepts
//! and validates work but there is no GPU, real or virtual, executing any
//! of it. That's a legitimate architecture-and-correctness-testing tool
//! (the same role `llvmpipe`/`lavapipe` play for Mesa), but it is not a
//! Vulkan implementation, and this workspace previously described itself
//! as "Vulkan/OpenGL via Mesa" despite having no Vulkan/OpenGL/Mesa
//! dependency anywhere.
//!
//! This crate is the other half of an honest answer: a real binding to a
//! real Vulkan loader/ICD, using [`ash`](https://docs.rs/ash), the
//! standard idiomatic Rust Vulkan binding. It is intentionally **thin** —
//! it does not attempt to implement `gpu_abstraction::GpuDriver` or wire
//! into `graphics_runtime::GraphicsRuntime`. Bridging a real, asynchronous,
//! device-lost-capable Vulkan device into a trait shaped around the
//! software driver's synchronous, always-succeeds-unless-told-otherwise
//! semantics is real design work (device-lost recovery, real fence-based
//! timelines, real memory-type-aware allocation instead of a bump
//! allocator) that belongs to a follow-up pass, not this one. What's here
//! is real and independently verifiable:
//!
//! 1. [`VulkanContext::probe`] — creates a real `VkInstance`, enumerates
//!    real `VkPhysicalDevice`s via `vkEnumeratePhysicalDevices`, and
//!    returns their real name/vendor/device-type/driver info as reported
//!    by the actual ICD (e.g. MoltenVK translating to Metal on macOS, or a
//!    native Vulkan driver on Linux/Windows). No synthetic data.
//! 2. [`VulkanContext::render_clear_to_offscreen`] — creates a real
//!    `VkDevice` and queue, allocates a real device-local `VkImage`,
//!    records and submits a real command buffer that clears it to a given
//!    RGBA color via `vkCmdClearColorImage`, copies it to a host-visible
//!    readback buffer via `vkCmdCopyImageToBuffer`, waits on a real fence,
//!    and returns the raw pixel bytes read back from actual GPU/ICD
//!    memory. The integration test in this crate asserts those bytes
//!    actually match the requested color — i.e. this proves real work
//!    executed on a real driver, not just that the calls didn't error.
//!
//! ## Why `ash` with the `loaded` feature
//!
//! `ash`'s default `loaded` feature `dlopen()`s `libvulkan`/`vulkan-1`/
//! MoltenVK's loader **at runtime** via `libloading`, rather than linking
//! against it at build time. That means `cargo build`/`cargo check
//! --workspace` succeed even in CI environments with no Vulkan loader
//! installed at all (this workspace's GitHub Actions runner is
//! `ubuntu-latest` with no Vulkan packages) — only code paths that
//! actually call [`VulkanContext::probe`] touch the loader, and every
//! entry point here returns a `Result`/reports unavailability instead of
//! panicking when no loader/ICD is present.
//!
//! ## What was actually verified on this machine
//!
//! macOS, no Vulkan/MoltenVK preinstalled. `brew install molten-vk
//! vulkan-loader vulkan-tools vulkan-headers` (real Vulkan-to-Metal
//! translation layer + loader + `vulkaninfo`), then this crate's tests
//! were run against it. `vulkaninfo --summary` and this crate's own probe
//! both enumerated a real device: `Apple M5` (`DRIVER_ID_MOLTENVK`,
//! `deviceType = INTEGRATED_GPU`). See `README.md` for the exact
//! reproduction steps and `ARCHITECTURE.md`'s Vulkan-backend addendum for
//! the architectural decision this represents.

use ash::vk;
use std::fmt;

/// Everything that can go wrong talking to a real Vulkan implementation.
/// Deliberately distinct from `sher_common::Error`: this crate has no
/// dependency on the rest of the SHER stack, by design (see module docs —
/// it's a standalone real-Vulkan proof, not wired into the runtime yet).
#[derive(Debug)]
pub enum VulkanError {
    /// No Vulkan loader/ICD found on this system (e.g. `libvulkan`/
    /// `vulkan-1`/MoltenVK not installed, or `VK_ICD_FILENAMES`/
    /// `DYLD_LIBRARY_PATH` not pointing at one). Not a bug — this is the
    /// expected, handled outcome on a machine with no Vulkan installed.
    LoaderUnavailable(String),
    /// A `vkCreateInstance`/`vkEnumeratePhysicalDevices`/etc. call itself
    /// returned a Vulkan error code.
    Vulkan(vk::Result),
    /// No physical device satisfied a requirement (e.g. no queue family
    /// supporting graphics+transfer).
    NoSuitableDevice(String),
}

impl fmt::Display for VulkanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VulkanError::LoaderUnavailable(msg) => {
                write!(f, "Vulkan loader/ICD unavailable: {msg}")
            }
            VulkanError::Vulkan(result) => write!(f, "Vulkan call failed: {result}"),
            VulkanError::NoSuitableDevice(msg) => write!(f, "no suitable Vulkan device: {msg}"),
        }
    }
}

impl std::error::Error for VulkanError {}

impl From<vk::Result> for VulkanError {
    fn from(result: vk::Result) -> Self {
        VulkanError::Vulkan(result)
    }
}

/// Real data read back from a real `VkPhysicalDeviceProperties` — every
/// field here came from the ICD's `vkGetPhysicalDeviceProperties`, not a
/// synthetic default.
#[derive(Debug, Clone)]
pub struct AdapterInfo {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: vk::PhysicalDeviceType,
    pub api_version: (u32, u32, u32),
    pub driver_version: u32,
}

/// Owns a real `VkInstance`. `render_clear_to_offscreen` creates and fully
/// tears down its own `VkDevice` and every resource on it per call, so
/// this struct itself never holds device-level state across calls.
pub struct VulkanContext {
    /// Never read directly, but must outlive `instance`: the loader
    /// `Arc<Library>` it holds keeps the `dlopen`'d Vulkan library mapped
    /// for as long as `instance`'s resolved function pointers might be
    /// called. Dropping it early would be a real unsoundness bug, not a
    /// dead-code warning — so it stays a field, not a local dropped at the
    /// end of `probe`.
    #[allow(dead_code)]
    entry: ash::Entry,
    instance: ash::Instance,
}

impl VulkanContext {
    /// Loads the Vulkan loader and creates a minimal `VkInstance`. Returns
    /// `Err(VulkanError::LoaderUnavailable)` — not a panic — if no loader
    /// is present, so callers (including this crate's own tests) can treat
    /// "no Vulkan on this machine" as a normal, handled outcome.
    ///
    /// Requests `VK_KHR_portability_enumeration` and sets
    /// `ENUMERATE_PORTABILITY_KHR` unconditionally: harmless on a
    /// conformant Linux/Windows Vulkan loader (the extension simply won't
    /// be found, and we only enable it after checking
    /// `enumerate_instance_extension_properties`), required on macOS/
    /// MoltenVK, which is a portability ICD and refuses to enumerate
    /// otherwise.
    pub fn probe() -> Result<Self, VulkanError> {
        // SAFETY: `Entry::load` dlopen()s the platform Vulkan loader and
        // resolves its global function pointers. It performs no GPU state
        // changes; the only way this is unsound is if a different, ABI-
        // incompatible library were loaded under the same name, which is a
        // system configuration concern `ash` itself documents, not
        // something this call site can guard against further.
        let entry = unsafe { ash::Entry::load() }
            .map_err(|e| VulkanError::LoaderUnavailable(e.to_string()))?;

        let app_name = c"SHER Graphics vulkan_backend probe";
        let engine_name = c"SHER Graphics";
        let app_info = vk::ApplicationInfo::default()
            .application_name(app_name)
            .application_version(vk::make_api_version(0, 0, 1, 0))
            .engine_name(engine_name)
            .engine_version(vk::make_api_version(0, 0, 1, 0))
            .api_version(vk::API_VERSION_1_1);

        // SAFETY: `entry` was just loaded successfully above; the
        // `p_next` chain is empty and every pointer in `create_info` is
        // kept alive for the duration of this call by the local bindings
        // (`app_info`, `enabled_extensions`).
        let available_extensions = unsafe { entry.enumerate_instance_extension_properties(None) }?;
        let portability_supported = available_extensions
            .iter()
            .any(|ext| ext.extension_name_as_c_str() == Ok(vk::KHR_PORTABILITY_ENUMERATION_NAME));

        let mut enabled_extensions: Vec<*const std::os::raw::c_char> = Vec::new();
        if portability_supported {
            enabled_extensions.push(vk::KHR_PORTABILITY_ENUMERATION_NAME.as_ptr());
        }

        let mut create_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&enabled_extensions);
        if portability_supported {
            create_info = create_info.flags(vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR);
        }

        // SAFETY: `entry` is valid and loaded; `create_info` and every
        // pointer it references (`app_info`, `enabled_extensions`) outlive
        // this call.
        let instance = unsafe { entry.create_instance(&create_info, None) }?;

        Ok(Self { entry, instance })
    }

    /// Whether a Vulkan loader/ICD is available on this machine at all,
    /// without keeping any instance alive afterward. Used by this crate's
    /// tests (and usable by callers) to skip real-GPU-dependent checks in
    /// environments — like this workspace's CI — that have no Vulkan
    /// installed, without treating that as a failure.
    pub fn available() -> bool {
        Self::probe().is_ok()
    }

    /// Real physical-device enumeration: `vkEnumeratePhysicalDevices` +
    /// `vkGetPhysicalDeviceProperties` per device, with every field of
    /// [`AdapterInfo`] copied straight out of the driver's response.
    pub fn enumerate_adapters(&self) -> Result<Vec<AdapterInfo>, VulkanError> {
        // SAFETY: `self.instance` is a valid, live instance for the
        // lifetime of `self`.
        let physical_devices = unsafe { self.instance.enumerate_physical_devices() }?;

        let mut adapters = Vec::with_capacity(physical_devices.len());
        for pdevice in physical_devices {
            // SAFETY: `pdevice` came from `enumerate_physical_devices` on
            // this same live instance, immediately above.
            let props = unsafe { self.instance.get_physical_device_properties(pdevice) };
            let name = props
                .device_name_as_c_str()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "<unreadable device name>".to_string());
            adapters.push(AdapterInfo {
                name,
                vendor_id: props.vendor_id,
                device_id: props.device_id,
                device_type: props.device_type,
                api_version: (
                    vk::api_version_major(props.api_version),
                    vk::api_version_minor(props.api_version),
                    vk::api_version_patch(props.api_version),
                ),
                driver_version: props.driver_version,
            });
        }
        Ok(adapters)
    }

    /// Real offscreen render: allocates a device-local `width`x`height`
    /// RGBA8 image on the first suitable physical device, clears it to
    /// `color` (linear RGBA, `[0.0, 1.0]` per channel) via
    /// `vkCmdClearColorImage`, copies it to a host-visible readback
    /// buffer, submits the command buffer, waits on a real fence, and
    /// returns the raw bytes read back from that buffer — i.e. actual
    /// pixel data produced by the ICD, not a value this function
    /// constructs itself.
    ///
    /// Every Vulkan object this function creates (`VkDevice`, `VkImage`,
    /// `VkBuffer`, two `VkDeviceMemory` allocations, command
    /// pool/buffer, fence) is destroyed before returning, success or
    /// error — nothing outlives this call.
    pub fn render_clear_to_offscreen(
        &self,
        width: u32,
        height: u32,
        color: [f32; 4],
    ) -> Result<Vec<u8>, VulkanError> {
        const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
        const BYTES_PER_PIXEL: u64 = 4;

        // SAFETY: `self.instance` is valid for `self`'s lifetime.
        let physical_devices = unsafe { self.instance.enumerate_physical_devices() }?;
        let physical_device = physical_devices.first().copied().ok_or_else(|| {
            VulkanError::NoSuitableDevice("vkEnumeratePhysicalDevices returned zero devices".into())
        })?;

        // SAFETY: `physical_device` came from this instance immediately above.
        let queue_families = unsafe {
            self.instance
                .get_physical_device_queue_family_properties(physical_device)
        };
        let queue_family_index = queue_families
            .iter()
            .position(|qf| {
                qf.queue_flags
                    .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::TRANSFER)
            })
            .ok_or_else(|| {
                VulkanError::NoSuitableDevice("no queue family supports GRAPHICS | TRANSFER".into())
            })? as u32;

        // SAFETY: `physical_device` is valid; `vk::PhysicalDevice::null()`
        // is used as a stand-in for "get subset support without an actual
        // call" — here we instead directly query extension support below.
        let device_extensions = unsafe {
            self.instance
                .enumerate_device_extension_properties(physical_device)
        }?;
        let mut enabled_device_extensions: Vec<*const std::os::raw::c_char> = Vec::new();
        if device_extensions
            .iter()
            .any(|ext| ext.extension_name_as_c_str() == Ok(vk::KHR_PORTABILITY_SUBSET_NAME))
        {
            enabled_device_extensions.push(vk::KHR_PORTABILITY_SUBSET_NAME.as_ptr());
        }

        let queue_priorities = [1.0_f32];
        let queue_create_info = vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(&queue_priorities);
        let queue_create_infos = [queue_create_info];
        let device_create_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&enabled_device_extensions);

        // SAFETY: `physical_device` is valid; `device_create_info` and
        // every slice/pointer it references outlive this call.
        let device = unsafe {
            self.instance
                .create_device(physical_device, &device_create_info, None)
        }?;

        // Every fallible step from here on must go through `cleanup` on
        // the way out so partially-created resources never leak, even on
        // an error path. `run` holds the actual logic; `render_clear_to_offscreen`
        // just guarantees `destroy_device` always runs.
        let result = Self::render_clear_inner(
            &self.instance,
            &device,
            physical_device,
            queue_family_index,
            width,
            height,
            color,
            FORMAT,
            BYTES_PER_PIXEL,
        );

        // SAFETY: `device` was successfully created above and every
        // resource created from it inside `render_clear_inner` was already
        // destroyed there (including on its own error paths) before
        // returning.
        unsafe { device.destroy_device(None) };

        result
    }

    #[allow(clippy::too_many_arguments)]
    fn render_clear_inner(
        instance: &ash::Instance,
        device: &ash::Device,
        physical_device: vk::PhysicalDevice,
        queue_family_index: u32,
        width: u32,
        height: u32,
        color: [f32; 4],
        format: vk::Format,
        bytes_per_pixel: u64,
    ) -> Result<Vec<u8>, VulkanError> {
        // SAFETY: `device` is a live logical device created from
        // `queue_family_index`, which was used to create exactly one
        // queue at index 0.
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: `device` is live; `image_info` is a self-contained value type.
        let image = unsafe { device.create_image(&image_info, None) }?;

        // SAFETY: `image` was just created on this same `device`.
        let image_mem_req = unsafe { device.get_image_memory_requirements(image) };
        // SAFETY: `physical_device` is valid.
        let mem_props = unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let image_mem_type = find_memory_type(
            &mem_props,
            image_mem_req.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .ok_or_else(|| {
            VulkanError::NoSuitableDevice("no DEVICE_LOCAL memory type for image".into())
        });
        let image_mem_type = match image_mem_type {
            Ok(t) => t,
            Err(e) => {
                unsafe { device.destroy_image(image, None) };
                return Err(e);
            }
        };
        let image_alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(image_mem_req.size)
            .memory_type_index(image_mem_type);
        // SAFETY: `device` is live; `image_alloc_info` is self-contained.
        let image_memory = match unsafe { device.allocate_memory(&image_alloc_info, None) } {
            Ok(m) => m,
            Err(e) => {
                unsafe { device.destroy_image(image, None) };
                return Err(e.into());
            }
        };
        // SAFETY: `image` and `image_memory` both belong to `device`;
        // neither has been bound/freed yet.
        if let Err(e) = unsafe { device.bind_image_memory(image, image_memory, 0) } {
            unsafe {
                device.free_memory(image_memory, None);
                device.destroy_image(image, None);
            }
            return Err(e.into());
        }

        let buffer_size = width as u64 * height as u64 * bytes_per_pixel;
        let buffer_info = vk::BufferCreateInfo::default()
            .size(buffer_size)
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: `device` is live; `buffer_info` is self-contained.
        let readback_buffer = match unsafe { device.create_buffer(&buffer_info, None) } {
            Ok(b) => b,
            Err(e) => {
                cleanup_image(device, image, image_memory);
                return Err(e.into());
            }
        };
        // SAFETY: `readback_buffer` was just created on this device.
        let buffer_mem_req = unsafe { device.get_buffer_memory_requirements(readback_buffer) };
        let buffer_mem_type = find_memory_type(
            &mem_props,
            buffer_mem_req.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        );
        let buffer_mem_type = match buffer_mem_type {
            Some(t) => t,
            None => {
                unsafe { device.destroy_buffer(readback_buffer, None) };
                cleanup_image(device, image, image_memory);
                return Err(VulkanError::NoSuitableDevice(
                    "no HOST_VISIBLE|HOST_COHERENT memory type for readback buffer".into(),
                ));
            }
        };
        let buffer_alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(buffer_mem_req.size)
            .memory_type_index(buffer_mem_type);
        // SAFETY: `device` is live.
        let buffer_memory = match unsafe { device.allocate_memory(&buffer_alloc_info, None) } {
            Ok(m) => m,
            Err(e) => {
                unsafe { device.destroy_buffer(readback_buffer, None) };
                cleanup_image(device, image, image_memory);
                return Err(e.into());
            }
        };
        // SAFETY: both belong to `device` and neither is bound yet.
        if let Err(e) = unsafe { device.bind_buffer_memory(readback_buffer, buffer_memory, 0) } {
            unsafe {
                device.free_memory(buffer_memory, None);
                device.destroy_buffer(readback_buffer, None);
            }
            cleanup_image(device, image, image_memory);
            return Err(e.into());
        }

        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(queue_family_index)
            .flags(vk::CommandPoolCreateFlags::TRANSIENT);
        // SAFETY: `device` is live.
        let command_pool = match unsafe { device.create_command_pool(&pool_info, None) } {
            Ok(p) => p,
            Err(e) => {
                cleanup_buffer(device, readback_buffer, buffer_memory);
                cleanup_image(device, image, image_memory);
                return Err(e.into());
            }
        };
        let cmd_alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: `command_pool` was just created on `device`.
        let command_buffers = match unsafe { device.allocate_command_buffers(&cmd_alloc_info) } {
            Ok(b) => b,
            Err(e) => {
                unsafe { device.destroy_command_pool(command_pool, None) };
                cleanup_buffer(device, readback_buffer, buffer_memory);
                cleanup_image(device, image, image_memory);
                return Err(e.into());
            }
        };
        let cmd = command_buffers[0];

        let record_result = (|| -> Result<(), VulkanError> {
            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            // SAFETY: `cmd` was just allocated and is not in the recording
            // state yet.
            unsafe { device.begin_command_buffer(cmd, &begin_info) }?;

            let subresource_range = vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1);

            let to_transfer_dst = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(subresource_range)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
            // SAFETY: `cmd` is in the recording state (begin_command_buffer
            // above succeeded); `image` is valid and owned by `device`.
            unsafe {
                device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_transfer_dst],
                );
            }

            let clear_color = vk::ClearColorValue { float32: color };
            // SAFETY: `cmd` is recording; `image` is in
            // TRANSFER_DST_OPTIMAL as of the barrier just recorded above.
            unsafe {
                device.cmd_clear_color_image(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &clear_color,
                    &[subresource_range],
                );
            }

            let to_transfer_src = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(subresource_range)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ);
            // SAFETY: same as the previous barrier — `cmd` recording, `image` valid.
            unsafe {
                device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_transfer_src],
                );
            }

            let copy_region = vk::BufferImageCopy::default()
                .buffer_offset(0)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_offset(vk::Offset3D::default())
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                });
            // SAFETY: `cmd` recording; `image` is TRANSFER_SRC_OPTIMAL as
            // of the barrier above; `readback_buffer` is large enough
            // (`buffer_size` == width*height*bytes_per_pixel).
            unsafe {
                device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    readback_buffer,
                    &[copy_region],
                );
            }

            // SAFETY: `cmd` is in the recording state.
            unsafe { device.end_command_buffer(cmd) }?;
            Ok(())
        })();

        if let Err(e) = record_result {
            unsafe { device.destroy_command_pool(command_pool, None) };
            cleanup_buffer(device, readback_buffer, buffer_memory);
            cleanup_image(device, image, image_memory);
            return Err(e);
        }

        let fence_info = vk::FenceCreateInfo::default();
        // SAFETY: `device` is live.
        let fence = match unsafe { device.create_fence(&fence_info, None) } {
            Ok(f) => f,
            Err(e) => {
                unsafe { device.destroy_command_pool(command_pool, None) };
                cleanup_buffer(device, readback_buffer, buffer_memory);
                cleanup_image(device, image, image_memory);
                return Err(e.into());
            }
        };

        let command_buffers_to_submit = [cmd];
        let submit_info = vk::SubmitInfo::default().command_buffers(&command_buffers_to_submit);
        let submit_result = (|| -> Result<(), VulkanError> {
            // SAFETY: `queue` belongs to `device`; `cmd` finished recording
            // successfully above; `fence` was just created, unsignaled.
            unsafe { device.queue_submit(queue, &[submit_info], fence) }?;
            // SAFETY: `fence` was just submitted with the queue above.
            unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }?;
            Ok(())
        })();

        let pixels = submit_result.and_then(|()| {
            // SAFETY: `buffer_memory` is HOST_VISIBLE|HOST_COHERENT (that
            // was a requirement of `find_memory_type` above), currently
            // unmapped, and the fence wait above guarantees the GPU copy
            // into it has completed before we read it.
            let mapped_ptr = unsafe {
                device.map_memory(buffer_memory, 0, buffer_size, vk::MemoryMapFlags::empty())
            }?;
            // SAFETY: `mapped_ptr` is valid for `buffer_size` bytes per
            // the successful `map_memory` call immediately above, and
            // HOST_COHERENT means no explicit invalidate is required
            // before reading GPU-written data.
            let bytes = unsafe {
                std::slice::from_raw_parts(mapped_ptr as *const u8, buffer_size as usize)
            }
            .to_vec();
            // SAFETY: `buffer_memory` is currently mapped (mapped above on
            // this same call path).
            unsafe { device.unmap_memory(buffer_memory) };
            Ok(bytes)
        });

        // SAFETY: `fence`/`command_pool` (which owns `cmd`) were both
        // created on `device` above and are no longer in use — either the
        // wait completed or we're on an error path where the fence was
        // never waited on but is still safe to destroy per the Vulkan
        // spec (destroying a fence never in the "in use" state pending a
        // queue that will signal it is undefined only if a submission
        // referencing it is still pending; `queue_submit` either failed,
        // meaning it was never queued, or the wait above already
        // completed it).
        unsafe {
            device.destroy_fence(fence, None);
            device.destroy_command_pool(command_pool, None);
        }
        cleanup_buffer(device, readback_buffer, buffer_memory);
        cleanup_image(device, image, image_memory);

        pixels
    }
}

fn cleanup_image(device: &ash::Device, image: vk::Image, memory: vk::DeviceMemory) {
    // SAFETY: caller guarantees `image`/`memory` belong to `device` and
    // are no longer referenced by any pending GPU work at the point this
    // is called (every call site is after a fence wait or on a path where
    // the image was never submitted to a queue).
    unsafe {
        device.destroy_image(image, None);
        device.free_memory(memory, None);
    }
}

fn cleanup_buffer(device: &ash::Device, buffer: vk::Buffer, memory: vk::DeviceMemory) {
    // SAFETY: same reasoning as `cleanup_image`.
    unsafe {
        device.destroy_buffer(buffer, None);
        device.free_memory(memory, None);
    }
}

fn find_memory_type(
    props: &vk::PhysicalDeviceMemoryProperties,
    type_bits: u32,
    required: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..props.memory_type_count).find(|&i| {
        let type_supported = type_bits & (1 << i) != 0;
        let props_supported = props.memory_types[i as usize]
            .property_flags
            .contains(required);
        type_supported && props_supported
    })
}

impl Drop for VulkanContext {
    fn drop(&mut self) {
        // SAFETY: `self.instance` was created successfully in `probe` and
        // nothing else holds a reference to it or calls into it once
        // `self` starts dropping. A custom `Drop::drop` body runs before
        // any of `self`'s fields are themselves dropped, so `self.entry`
        // (whose `Arc<Library>` keeps the Vulkan loader `dlopen`'d) is
        // still fully valid for the duration of this `vkDestroyInstance`
        // call — it's only decremented/potentially unloaded afterward,
        // when Rust auto-drops `self.entry` as part of tearing down `self`.
        unsafe { self.instance.destroy_instance(None) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Skips (rather than fails) when no Vulkan loader/ICD is present —
    /// this workspace's CI runs on `ubuntu-latest` with no Vulkan
    /// packages installed, and that must remain a green build, not a
    /// failure. On a machine with a real Vulkan installation (this crate
    /// was developed and verified against `brew install molten-vk
    /// vulkan-loader vulkan-tools` on macOS), these tests exercise real
    /// GPU/ICD calls end to end.
    macro_rules! require_vulkan {
        () => {
            match VulkanContext::probe() {
                Ok(ctx) => ctx,
                Err(VulkanError::LoaderUnavailable(msg)) => {
                    eprintln!("skipping: no Vulkan loader/ICD available ({msg})");
                    return;
                }
                Err(e) => panic!("unexpected error probing for Vulkan: {e}"),
            }
        };
    }

    #[test]
    fn probe_reports_availability_without_panicking() {
        // This must never panic, on any machine, Vulkan present or not.
        let _ = VulkanContext::available();
    }

    #[test]
    fn enumerate_adapters_returns_real_device_data() {
        let ctx = require_vulkan!();
        let adapters = ctx
            .enumerate_adapters()
            .expect("enumeration should succeed once an instance exists");
        assert!(
            !adapters.is_empty(),
            "a Vulkan loader with a working ICD must report at least one physical device"
        );
        for adapter in &adapters {
            assert!(
                !adapter.name.is_empty(),
                "physical device name must be non-empty (came from the ICD, not synthesized)"
            );
        }
    }

    #[test]
    fn clear_color_render_round_trips_through_real_gpu_memory() {
        let ctx = require_vulkan!();
        // Distinctive, non-trivial color: red=200, green=100, blue=50, alpha=255.
        let color = [200.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0, 1.0];
        let pixels = ctx
            .render_clear_to_offscreen(4, 4, color)
            .expect("clear-color render should succeed against a real ICD");

        assert_eq!(
            pixels.len(),
            4 * 4 * 4,
            "expected RGBA8 bytes for a 4x4 image"
        );

        for (i, pixel) in pixels.chunks_exact(4).enumerate() {
            // Allow +/-2 for UNORM rounding, which is real ICD behavior,
            // not a shortcut in this test.
            assert!(
                (pixel[0] as i32 - 200).abs() <= 2,
                "pixel {i} red channel = {}, expected ~200",
                pixel[0]
            );
            assert!(
                (pixel[1] as i32 - 100).abs() <= 2,
                "pixel {i} green channel = {}, expected ~100",
                pixel[1]
            );
            assert!(
                (pixel[2] as i32 - 50).abs() <= 2,
                "pixel {i} blue channel = {}, expected ~50",
                pixel[2]
            );
            assert_eq!(
                pixel[3], 255,
                "pixel {i} alpha channel should be fully opaque"
            );
        }
    }

    #[test]
    fn offscreen_render_supports_multiple_sequential_calls() {
        let ctx = require_vulkan!();
        // Proves each call tears down its own device/image/buffer fully —
        // if resources leaked, a second real allocation on most ICDs
        // would still succeed at this tiny size, so this is a smoke test
        // for "the API doesn't require re-probing," not for leak
        // detection specifically.
        let first = ctx
            .render_clear_to_offscreen(2, 2, [1.0, 0.0, 0.0, 1.0])
            .unwrap();
        let second = ctx
            .render_clear_to_offscreen(2, 2, [0.0, 1.0, 0.0, 1.0])
            .unwrap();
        assert_eq!(first.len(), second.len());
        assert_ne!(
            first, second,
            "different clear colors must produce different pixel bytes"
        );
    }
}

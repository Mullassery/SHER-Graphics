//! End-to-end validation of the SHER Graphics stack, with no real GPU
//! involved — the same role `llvmpipe`/`lavapipe` play for Mesa
//! (ARCHITECTURE.md section 21: "what's realistically implementable
//! first"). Exercises every layer:
//!
//!   graphics_api (trait + vocabulary)
//!       -> graphics_runtime (GraphicsRuntime, implements GraphicsApi)
//!       -> gpu_abstraction (SoftwareGpuDriver, implements GpuDriver)
//!       -> gpu_driver (PresentationBridge -> Connector/Framebuffer)
//!
//! Run with: cargo run -p graphics_runtime --example triangle

use gpu_abstraction::SoftwareGpuDriver;
use gpu_driver::{Connector, ConnectorStatus, ConnectorType};
use graphics_api::{
    BufferUsage, GraphicsApi, GraphicsOp, MemoryClass, ResourceKind, ShaderStage, WorkloadClass,
};
use graphics_runtime::GraphicsRuntime;
use sher_common::{Capability, ObjectId, PermissionTier};
use sher_objectmodel::CapabilitySet;

fn main() -> sher_common::Result<()> {
    println!("== SHER Graphics: end-to-end triangle (SoftwareGpuDriver) ==\n");

    // 1. Driver + runtime. No hardware, no root, no display server needed.
    let driver = SoftwareGpuDriver::new(64 * 1024 * 1024);
    let gpu = driver.device_id();
    let mut runtime = GraphicsRuntime::new(driver, 16 * 1024 * 1024);
    println!("1. SoftwareGpuDriver ready, gpu = {gpu}");

    // 2. A graphics context requires the same capability model the rest of
    //    SHER Kernel uses - no graphics-specific permission system.
    let mut caps = CapabilitySet::default();
    caps.grant(Capability::GpuMemoryAlloc, PermissionTier::High);
    caps.grant(Capability::GpuCommandSubmit, PermissionTier::High);
    let device = runtime.create_device(gpu, &caps)?;
    println!(
        "2. GraphicsDevice created, id = {}, {} workload queues",
        device.id,
        device.queues.len()
    );

    // 3. One synchronization primitive: a Timeline. No binary/timeline
    //    semaphore split to reason about.
    let timeline = runtime.create_timeline(&device.id)?;
    println!("3. Timeline created, id = {}", timeline.id);

    // 4. Shader modules. IR bytes are a stand-in for SPIR-V/NIR - the
    //    runtime never interprets them, only the driver backend does.
    let vertex_shader =
        runtime.create_shader_module(&device.id, ShaderStage::Vertex, vec![0u8; 16])?;
    let fragment_shader =
        runtime.create_shader_module(&device.id, ShaderStage::Fragment, vec![0u8; 16])?;
    println!(
        "4. Shader modules created: vertex = {}, fragment = {}",
        vertex_shader.id, fragment_shader.id
    );

    // 5. Pipeline. No monolithic pipeline-state-object struct to hash -
    //    see ARCHITECTURE.md section 2 for why.
    let pipeline =
        runtime.create_pipeline(&device.id, &vertex_shader.id, WorkloadClass::Graphics)?;
    println!("5. Pipeline created, id = {}", pipeline.id);

    // 6. A vertex buffer, backed by host-visible memory so this example
    //    doesn't need a real upload path.
    let vertex_buffer = runtime.create_resource(
        &device.id,
        ResourceKind::Buffer {
            size: 3 * 3 * 4,
            usage: BufferUsage {
                vertex: true,
                ..Default::default()
            },
        },
        MemoryClass::HostVisible,
    )?;
    println!(
        "6. Vertex buffer resource created, id = {}",
        vertex_buffer.id
    );

    // 7. Record a command stream: bind pipeline, bind the vertex buffer,
    //    draw. graphics_runtime validates this binding sequence before it
    //    ever reaches the driver (a Draw without a bound pipeline would be
    //    rejected here).
    let mut stream = runtime.create_command_stream(&device.id, WorkloadClass::Graphics)?;
    stream.push(GraphicsOp::BindPipeline(pipeline.id));
    stream.push(GraphicsOp::BindResource {
        binding: 0,
        resource: vertex_buffer.id,
    });
    stream.push(GraphicsOp::Draw {
        vertex_count: 3,
        instance_count: 1,
    });
    println!("7. Command stream recorded: bind pipeline, bind buffer, draw 3 vertices");

    // 8. Submit on the Graphics queue, signaling the timeline to 1.
    let queue = device
        .queue(WorkloadClass::Graphics)
        .expect("create_device always populates a Graphics queue");
    runtime.submit(&queue, stream, &timeline.id, 1)?;
    println!("8. Submitted to Graphics queue, timeline signaled to 1");

    // 9. Wait for completion. This reference runtime executes synchronously,
    //    so the wait resolves immediately - a hardware backend would park
    //    on the kernel wake-on-value primitive instead (ARCHITECTURE.md
    //    section 13).
    runtime.wait(&timeline.id, 1)?;
    println!(
        "9. Wait satisfied, timeline_value = {}",
        runtime.timeline_value(&timeline.id)?
    );

    // 10. Present. Bridges to gpu_driver's connector/framebuffer/page-flip
    //     primitives (Phase 11's skeleton) - the same seam Mesa's WSI
    //     backend would eventually target.
    let connector = Connector {
        id: ObjectId::new(),
        connector_type: ConnectorType::HDMI,
        status: ConnectorStatus::Connected,
        supported_modes: vec![],
        current_mode: None,
    };
    let connector_id = connector.id;
    runtime.register_connector(connector)?;
    let framebuffer = runtime.present_frame(&connector_id, 1920, 1080)?;
    println!("10. Presented frame, framebuffer id = {framebuffer}");

    println!("\nEnd-to-end path validated: graphics_api -> graphics_runtime -> gpu_abstraction -> gpu_driver.");
    Ok(())
}

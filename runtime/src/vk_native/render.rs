//! Real GPU rendering + presentation for the native backend.
//!
//! This is the "draw something and show it" layer, independent of how the
//! surface is presented:
//!
//! * [`OffscreenTarget`] owns a host-visible `B8G8R8A8_UNORM` image plus a
//!   render pass and framebuffer. The GPU draws into it with a real graphics
//!   pipeline (SPIR-V shaders), then the bytes are mapped on the CPU.
//! * That buffer is handed to [`crate::vk_native::x11::X11Window::present`],
//!   which `XPutImage`s it to a window — the only way to *see* the output under
//!   Termux-X11/Xvfb, where Turnip cannot present a swapchain (no DRI3).
//!
//! The triangle is the built-in smoke test; the same target will receive the
//! D3D11 pipeline's output once shader translation (DXBC -> SPIR-V) lands.

use ash::vk;

use super::NativeDevice;

/// Offscreen colour target the GPU renders into, with CPU readback.
pub struct OffscreenTarget {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub render_pass: vk::RenderPass,
    pub framebuffer: vk::Framebuffer,
    pub pipeline: vk::Pipeline,
    pub pipeline_layout: vk::PipelineLayout,
    pub command_pool: vk::CommandPool,
    pub command_buffer: vk::CommandBuffer,
    pub fence: vk::Fence,
    pub vertex_buffer: vk::Buffer,
    pub vertex_memory: vk::DeviceMemory,
    /// Capacity of the vertex buffer, in vertices.
    vertex_capacity: u32,
    /// How many vertices the next draw uses.
    vertex_count: u32,
    pub format: vk::Format,
    pub width: u32,
    pub height: u32,
    /// `row_pitch` reported by the driver for the linear image (copy rows by it).
    row_pitch: u64,
    mapped_size: u64,
    mapped: *mut u8,
}

/// BGRA (matches `B8G8R8A8_UNORM`, what `XPutImage` expects on a 24/32-bit
/// TrueColor visual).
pub type Color = [f32; 4];

impl OffscreenTarget {
    /// Build an offscreen target of `width`x`height` with a graphics pipeline
    /// that draws a coloured triangle. All resources are created up front and
    /// kept until [`destroy`](Self::destroy).
    pub fn new(dev: &NativeDevice, width: u32, height: u32) -> Option<Self> {
        let d = &dev.device;
        let format = vk::Format::B8G8R8A8_UNORM;

        // Host-visible linear image (so it can be mapped straight to X).
        let img_ci = vk::ImageCreateInfo::default()
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
            .tiling(vk::ImageTiling::LINEAR)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { d.create_image(&img_ci, None) }.ok()?;
        let req = unsafe { d.get_image_memory_requirements(image) };
        let mprops = unsafe {
            dev.instance
                .get_physical_device_memory_properties(dev.physical_device)
        };
        let mem_type = host_visible_type(&mprops, req.memory_type_bits)?;
        let memory = unsafe {
            d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(mem_type),
                None,
            )
        }
        .ok()?;
        unsafe { d.bind_image_memory(image, memory, 0) }.ok()?;
        let view = unsafe {
            d.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    }),
                None,
            )
        }
        .ok()?;
        let sub = unsafe {
            d.get_image_subresource_layout(
                image,
                vk::ImageSubresource {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    array_layer: 0,
                },
            )
        };

        // Render pass (single colour attachment, cleared, kept in GENERAL).
        let att = vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::GENERAL);
        let cref = [vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&cref)];
        let render_pass = unsafe {
            d.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(std::slice::from_ref(&att))
                    .subpasses(&subpass),
                None,
            )
        }
        .ok()?;

        let framebuffer = unsafe {
            d.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(render_pass)
                    .attachments(std::slice::from_ref(&view))
                    .width(width)
                    .height(height)
                    .layers(1),
                None,
            )
        }
        .ok()?;

        // Shaders (built-in SPIR-V) + graphics pipeline.
        let vs = read_spv(include_bytes!("shaders/tri.vert.spv"));
        let fs = read_spv(include_bytes!("shaders/tri.frag.spv"));
        let vsm = unsafe {
            d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&vs), None)
        }
        .ok()?;
        let fsm = unsafe {
            d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&fs), None)
        }
        .ok()?;
        let main = std::ffi::CStr::from_bytes_with_nul(b"main\0").unwrap();
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vsm)
                .name(main),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fsm)
                .name(main),
        ];
        let bindings = [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(20)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attrs = [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .binding(0)
                .format(vk::Format::R32G32_SFLOAT)
                .offset(0),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(8),
        ];
        let vinput = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attrs);
        let ia = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = [vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        }];
        let scissor = [vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D { width, height },
        }];
        let vp = vk::PipelineViewportStateCreateInfo::default()
            .viewports(&viewport)
            .scissors(&scissor);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let cblend = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .blend_enable(false)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cblend);
        let pipeline_layout = unsafe {
            d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default(), None)
        }
        .ok()?;
        let pci = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vinput)
            .input_assembly_state(&ia)
            .viewport_state(&vp)
            .rasterization_state(&raster)
            .multisample_state(&ms)
            .color_blend_state(&blend)
            .layout(pipeline_layout)
            .render_pass(render_pass)
            .subpass(0);
        let pipeline = unsafe {
            d.create_graphics_pipelines(vk::PipelineCache::null(), &[pci], None)
        }
        .ok()?[0];
        unsafe {
            d.destroy_shader_module(vsm, None);
            d.destroy_shader_module(fsm, None);
        }

        // Vertex buffer (host-visible): one coloured triangle, with room to
        // accept a streamed mesh later (D3D11 IA vertex buffers).
        let verts: [[f32; 5]; 3] = [
            [0.0, -0.7, 1.0, 0.0, 0.0],
            [0.7, 0.7, 0.0, 1.0, 0.0],
            [-0.7, 0.7, 0.0, 0.0, 1.0],
        ];
        let capacity = 256u32;
        let vbytes =
            unsafe { std::slice::from_raw_parts(verts.as_ptr() as *const u8, std::mem::size_of_val(&verts)) };
        let (vertex_buffer, vertex_memory) = create_host_buffer_sized(
            d,
            &mprops,
            vk::BufferUsageFlags::VERTEX_BUFFER,
            vbytes,
            (capacity as usize * 20) as u64,
        )?;

        // Command pool + buffer, one pre-recorded frame.
        let command_pool = unsafe {
            d.create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(dev.families.graphics),
                None,
            )
        }
        .ok()?;
        let command_buffer = unsafe {
            d.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .ok()?[0];
        let fence = unsafe {
            d.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )
        }
        .ok()?;

        let mut t = OffscreenTarget {
            image,
            memory,
            view,
            render_pass,
            framebuffer,
            pipeline,
            pipeline_layout,
            command_pool,
            command_buffer,
            fence,
            vertex_buffer,
            vertex_memory,
            vertex_capacity: capacity,
            vertex_count: 3,
            format,
            width,
            height,
            row_pitch: sub.row_pitch,
            mapped_size: sub.size,
            mapped: std::ptr::null_mut(),
        };
        t.record(dev, [0.05, 0.05, 0.1, 1.0]);
        crate::glog!(
            "[gow.vk.native.render] offscreen target {width}x{height} fmt={} rowPitch={}",
            format.as_raw(),
            sub.row_pitch
        );
        Some(t)
    }

    /// Overwrite the vertex stream (position+colour per vertex, 5 floats). Lets
    /// the D3D11 layer feed real geometry into the built-in scene pipeline.
    pub fn set_vertices(&mut self, dev: &NativeDevice, verts: &[[f32; 5]]) {
        let bytes = unsafe {
            std::slice::from_raw_parts(verts.as_ptr() as *const u8, std::mem::size_of_val(verts))
        };
        unsafe {
            let p = dev
                .device
                .map_memory(self.vertex_memory, 0, bytes.len() as u64, vk::MemoryMapFlags::empty());
            if let Ok(p) = p {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
                dev.device.unmap_memory(self.vertex_memory);
                self.vertex_count = verts.len() as u32;
            }
        }
    }

    /// Record the draw (clear + triangle) into the command buffer.
    pub fn record(&self, dev: &NativeDevice, clear: Color) {
        let d = &dev.device;
        unsafe {
            d.reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .ok();
            d.begin_command_buffer(self.command_buffer, &vk::CommandBufferBeginInfo::default())
                .ok();
            let cv = vk::ClearValue {
                color: vk::ClearColorValue { float32: clear },
            };
            let rpb = vk::RenderPassBeginInfo::default()
                .render_pass(self.render_pass)
                .framebuffer(self.framebuffer)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D {
                        width: self.width,
                        height: self.height,
                    },
                })
                .clear_values(std::slice::from_ref(&cv));
            d.cmd_begin_render_pass(self.command_buffer, &rpb, vk::SubpassContents::INLINE);
            d.cmd_bind_pipeline(
                self.command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline,
            );
            d.cmd_bind_vertex_buffers(self.command_buffer, 0, &[self.vertex_buffer], &[0]);
            d.cmd_draw(self.command_buffer, self.vertex_count.min(self.vertex_capacity), 1, 0, 0);
            d.cmd_end_render_pass(self.command_buffer);
            let _ = d.end_command_buffer(self.command_buffer);
        }
    }

    /// Submit the recorded frame and wait for it to finish.
    pub fn render_and_wait(&self, dev: &NativeDevice) -> bool {
        let d = &dev.device;
        unsafe {
            if d.reset_fences(&[self.fence]).is_err() {
                return false;
            }
            let submit = vk::SubmitInfo::default()
                .command_buffers(std::slice::from_ref(&self.command_buffer));
            if d.queue_submit(dev.queue, &[submit], self.fence).is_err() {
                return false;
            }
            d.wait_for_fences(&[self.fence], true, u64::MAX).is_ok()
        }
    }

    /// Map the image and copy it into `out` as tightly packed BGRA
    /// (`width*height*4` bytes, row stride `width*4`).
    pub fn read_into(&mut self, dev: &NativeDevice, out: &mut [u8]) -> bool {
        let d = &dev.device;
        let need = (self.width * self.height * 4) as usize;
        if out.len() != need {
            return false;
        }
        if self.mapped.is_null() {
            match unsafe {
                d.map_memory(self.memory, 0, self.mapped_size, vk::MemoryMapFlags::empty())
            } {
                Ok(p) => self.mapped = p as *mut u8,
                Err(e) => {
                    crate::glog!("[gow.vk.native.render] map_memory failed: {e:?}");
                    return false;
                }
            }
        }
        unsafe {
            for y in 0..self.height as usize {
                let src = self.mapped.add(y * self.row_pitch as usize);
                std::ptr::copy_nonoverlapping(
                    src,
                    out.as_mut_ptr().add(y * self.width as usize * 4),
                    (self.width * 4) as usize,
                );
            }
        }
        true
    }

    pub fn destroy(&mut self, dev: &NativeDevice) {
        let d = &dev.device;
        unsafe {
            let _ = d.device_wait_idle();
            if !self.mapped.is_null() {
                d.unmap_memory(self.memory);
                self.mapped = std::ptr::null_mut();
            }
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.command_pool, None);
            d.destroy_buffer(self.vertex_buffer, None);
            d.free_memory(self.vertex_memory, None);
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.pipeline_layout, None);
            d.destroy_framebuffer(self.framebuffer, None);
            d.destroy_render_pass(self.render_pass, None);
            d.destroy_image_view(self.view, None);
            d.destroy_image(self.image, None);
            d.free_memory(self.memory, None);
        }
    }
}

/// Public wrapper for the scene module.
pub fn host_visible_type_pub(
    mprops: &vk::PhysicalDeviceMemoryProperties,
    bits: u32,
) -> Option<u32> {
    host_visible_type(mprops, bits)
}

/// Public wrapper: create a mapped host-visible buffer initialised with `data`.
pub fn create_host_buffer_pub(
    d: &ash::Device,
    mprops: &vk::PhysicalDeviceMemoryProperties,
    usage: vk::BufferUsageFlags,
    data: &[u8],
) -> Option<(vk::Buffer, vk::DeviceMemory)> {
    create_host_buffer(d, mprops, usage, data)
}

fn host_visible_type(mprops: &vk::PhysicalDeviceMemoryProperties, bits: u32) -> Option<u32> {
    (0..mprops.memory_type_count).find(|&i| {
        let f = mprops.memory_types[i as usize].property_flags;
        (bits & (1 << i)) != 0
            && f.contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
            && f.contains(vk::MemoryPropertyFlags::HOST_COHERENT)
    })
}

fn create_host_buffer(
    d: &ash::Device,
    mprops: &vk::PhysicalDeviceMemoryProperties,
    usage: vk::BufferUsageFlags,
    data: &[u8],
) -> Option<(vk::Buffer, vk::DeviceMemory)> {
    create_host_buffer_sized(d, mprops, usage, data, data.len() as u64)
}

/// Like [`create_host_buffer`] but allocates `size` bytes (>= `data.len()`), so
/// the buffer can later receive a streamed mesh.
fn create_host_buffer_sized(
    d: &ash::Device,
    mprops: &vk::PhysicalDeviceMemoryProperties,
    usage: vk::BufferUsageFlags,
    data: &[u8],
    size: u64,
) -> Option<(vk::Buffer, vk::DeviceMemory)> {
    let size = size.max(data.len() as u64);
    unsafe {
        let buf = d
            .create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(usage)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
            .ok()?;
        let req = d.get_buffer_memory_requirements(buf);
        let ty = host_visible_type(mprops, req.memory_type_bits)?;
        let mem = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(ty),
                None,
            )
            .ok()?;
        d.bind_buffer_memory(buf, mem, 0).ok()?;
        let p = d
            .map_memory(mem, 0, req.size, vk::MemoryMapFlags::empty())
            .ok()?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), p as *mut u8, data.len());
        d.unmap_memory(mem);
        Some((buf, mem))
    }
}

fn read_spv(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

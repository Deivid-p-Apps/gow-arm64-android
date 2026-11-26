//! A real 3D scene for the native backend: a spinning coloured cube rendered on
//! the GPU with a **depth buffer**, a **push-constant MVP matrix** and an
//! animated per-frame matrix. This is what proves the renderer is a genuine 3D
//! pipeline (vertex transform, depth test, uniforms), not just a clear.
//!
//! Output goes through [`super::render::OffscreenTarget`] -> `XPutImage`
//! (`super::x11`), so it is visible under Xvfb / Termux:X11.

use ash::vk;

use super::NativeDevice;

/// Simple column-major 4x4 matrix (matches GLSL `mat4` memory order).
#[derive(Clone, Copy)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    pub fn identity() -> Self {
        let mut m = [0.0f32; 16];
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        Mat4(m)
    }

    pub fn mul(&self, o: &Mat4) -> Mat4 {
        let a = &self.0;
        let b = &o.0;
        let mut r = [0.0f32; 16];
        for c in 0..4 {
            for row in 0..4 {
                let mut s = 0.0;
                for k in 0..4 {
                    s += a[k * 4 + row] * b[c * 4 + k];
                }
                r[c * 4 + row] = s;
            }
        }
        Mat4(r)
    }

    /// Right-handed perspective with Vulkan's depth range [0,1] (see
    /// `GLM_FORCE_DEPTH_ZERO_TO_ONE`).
    pub fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> Self {
        let f = 1.0 / (fovy / 2.0).tan();
        let mut m = [0.0f32; 16];
        m[0] = f / aspect;
        m[5] = -f; // Vulkan Y-flip (framebuffer origin is top-left)
        m[10] = far / (near - far);
        m[11] = -1.0;
        m[14] = (near * far) / (near - far);
        Mat4(m)
    }

    pub fn translation(x: f32, y: f32, z: f32) -> Self {
        let mut m = Mat4::identity();
        m.0[12] = x;
        m.0[13] = y;
        m.0[14] = z;
        Mat4(m.0)
    }

    pub fn rotation_y(a: f32) -> Self {
        let mut m = Mat4::identity();
        let (s, c) = (a.sin(), a.cos());
        m.0[0] = c;
        m.0[2] = -s;
        m.0[8] = s;
        m.0[10] = c;
        Mat4(m.0)
    }

    pub fn rotation_x(a: f32) -> Self {
        let mut m = Mat4::identity();
        let (s, c) = (a.sin(), a.cos());
        m.0[5] = c;
        m.0[6] = s;
        m.0[9] = -s;
        m.0[10] = c;
        Mat4(m.0)
    }
}

const VERTEX_STRIDE: u32 = 24; // vec3 pos + vec3 col

fn cube_vertices() -> Vec<[f32; 6]> {
    // 6 faces * 2 triangles * 3 verts, per-face colour, positions in [-1,1].
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        // normal      corners
        ([0.0, 0.0, 1.0], [[-1.0, -1.0, 1.0], [1.0, -1.0, 1.0], [1.0, 1.0, 1.0], [-1.0, 1.0, 1.0]]),
        ([0.0, 0.0, -1.0], [[1.0, -1.0, -1.0], [-1.0, -1.0, -1.0], [-1.0, 1.0, -1.0], [1.0, 1.0, -1.0]]),
        ([1.0, 0.0, 0.0], [[1.0, -1.0, 1.0], [1.0, -1.0, -1.0], [1.0, 1.0, -1.0], [1.0, 1.0, 1.0]]),
        ([-1.0, 0.0, 0.0], [[-1.0, -1.0, -1.0], [-1.0, -1.0, 1.0], [-1.0, 1.0, 1.0], [-1.0, 1.0, -1.0]]),
        ([0.0, 1.0, 0.0], [[-1.0, 1.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, -1.0], [-1.0, 1.0, -1.0]]),
        ([0.0, -1.0, 0.0], [[-1.0, -1.0, -1.0], [1.0, -1.0, -1.0], [1.0, -1.0, 1.0], [-1.0, -1.0, 1.0]]),
    ];
    let color_of = |n: [f32; 3]| -> [f32; 3] {
        // Give each face a distinct colour from its normal.
        [
            (n[0] * 0.5 + 0.5).max(0.2),
            (n[1] * 0.5 + 0.5).max(0.2),
            (n[2] * 0.5 + 0.5).max(0.2),
        ]
    };
    let mut v = Vec::with_capacity(36);
    for (n, q) in faces {
        let col = color_of(n);
        let idx = [0usize, 1, 2, 0, 2, 3];
        for &i in &idx {
            let p = q[i];
            v.push([p[0], p[1], p[2], col[0], col[1], col[2]]);
        }
    }
    v
}

/// A depth-tested 3D target: colour + depth images, push-constant pipeline.
pub struct Scene3D {
    pub width: u32,
    pub height: u32,
    color: vk::Image,
    color_mem: vk::DeviceMemory,
    color_view: vk::ImageView,
    depth: vk::Image,
    depth_mem: vk::DeviceMemory,
    depth_view: vk::ImageView,
    render_pass: vk::RenderPass,
    framebuffer: vk::Framebuffer,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    vertex_buffer: vk::Buffer,
    vertex_memory: vk::DeviceMemory,
    row_pitch: u64,
    mapped_size: u64,
    mapped: *mut u8,
    start: std::time::Instant,
}

impl Scene3D {
    pub fn new(dev: &NativeDevice, width: u32, height: u32) -> Option<Self> {
        let d = &dev.device;
        let format = vk::Format::B8G8R8A8_UNORM;
        let mprops = unsafe {
            dev.instance
                .get_physical_device_memory_properties(dev.physical_device)
        };

        let (color, color_mem, color_view, row_pitch, mapped_size) =
            make_color_target(d, &mprops, width, height, format)?;
        let (depth, depth_mem, depth_view) = make_depth_target(d, &mprops, width, height)?;

        let atts = [
            vk::AttachmentDescription::default()
                .format(format)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .final_layout(vk::ImageLayout::GENERAL),
            vk::AttachmentDescription::default()
                .format(vk::Format::D32_SFLOAT)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::DONT_CARE)
                .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .final_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL),
        ];
        let cref = [vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let dref = vk::AttachmentReference::default()
            .attachment(1)
            .layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL);
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&cref)
            .depth_stencil_attachment(&dref)];
        let render_pass = unsafe {
            d.create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(&atts)
                    .subpasses(&subpass),
                None,
            )
        }
        .ok()?;
        let framebuffer = unsafe {
            d.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(render_pass)
                    .attachments(&[color_view, depth_view])
                    .width(width)
                    .height(height)
                    .layers(1),
                None,
            )
        }
        .ok()?;

        // Pipeline with push-constant MVP.
        let vs = read_spv(include_bytes!("shaders/cube.vert.spv"));
        let fs = read_spv(include_bytes!("shaders/cube.frag.spv"));
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
            .stride(VERTEX_STRIDE)
            .input_rate(vk::VertexInputRate::VERTEX)];
        let attrs = [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(0),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .binding(0)
                .format(vk::Format::R32G32B32_SFLOAT)
                .offset(12),
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
            .cull_mode(vk::CullModeFlags::BACK)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let ms = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let ds = vk::PipelineDepthStencilStateCreateInfo::default()
            .depth_test_enable(true)
            .depth_write_enable(true)
            .depth_compare_op(vk::CompareOp::LESS_OR_EQUAL);
        let cblend = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .blend_enable(false)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cblend);
        let range = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(64)];
        let layout = unsafe {
            d.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&range),
                None,
            )
        }
        .ok()?;
        let pci = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vinput)
            .input_assembly_state(&ia)
            .viewport_state(&vp)
            .rasterization_state(&raster)
            .multisample_state(&ms)
            .depth_stencil_state(&ds)
            .color_blend_state(&blend)
            .layout(layout)
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

        let verts = cube_vertices();
        let vbytes = unsafe {
            std::slice::from_raw_parts(verts.as_ptr() as *const u8, std::mem::size_of_val(verts.as_slice()))
        };
        let (vertex_buffer, vertex_memory) =
            super::render::create_host_buffer_pub(d, &mprops, vk::BufferUsageFlags::VERTEX_BUFFER, vbytes)?;

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

        crate::glog!(
            "[gow.vk.native.scene] 3D scene {width}x{height} depth=D32S8?=D32 stage=pushMVP"
        );
        Some(Scene3D {
            width,
            height,
            color,
            color_mem,
            color_view,
            depth,
            depth_mem,
            depth_view,
            render_pass,
            framebuffer,
            pipeline,
            layout,
            command_pool,
            command_buffer,
            fence,
            vertex_buffer,
            vertex_memory,
            row_pitch,
            mapped_size,
            mapped: std::ptr::null_mut(),
            start: std::time::Instant::now(),
        })
    }

    /// Draw one animated frame (rotating cube) and wait for completion.
    pub fn render_frame(&self, dev: &NativeDevice) -> bool {
        let d = &dev.device;
        let t = self.start.elapsed().as_secs_f32();
        let aspect = self.width as f32 / self.height as f32;
        let proj = Mat4::perspective(45f32.to_radians(), aspect, 0.1, 100.0);
        let view = Mat4::translation(0.0, 0.0, -4.0);
        let model = Mat4::rotation_y(t).mul(&Mat4::rotation_x(t * 0.6));
        let mvp = proj.mul(&view).mul(&model);
        let mvp_bytes = unsafe {
            std::slice::from_raw_parts(mvp.0.as_ptr() as *const u8, std::mem::size_of_val(&mvp.0))
        };

        unsafe {
            if d.reset_fences(&[self.fence]).is_err() {
                return false;
            }
            d.reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .ok();
            d.begin_command_buffer(self.command_buffer, &vk::CommandBufferBeginInfo::default())
                .ok();
            let clears = [
                vk::ClearValue {
                    color: vk::ClearColorValue { float32: [0.02, 0.03, 0.06, 1.0] },
                },
                vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 },
                },
            ];
            let rpb = vk::RenderPassBeginInfo::default()
                .render_pass(self.render_pass)
                .framebuffer(self.framebuffer)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: vk::Extent2D { width: self.width, height: self.height },
                })
                .clear_values(&clears);
            d.cmd_begin_render_pass(self.command_buffer, &rpb, vk::SubpassContents::INLINE);
            d.cmd_bind_pipeline(self.command_buffer, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
            d.cmd_push_constants(
                self.command_buffer,
                self.layout,
                vk::ShaderStageFlags::VERTEX,
                0,
                mvp_bytes,
            );
            d.cmd_bind_vertex_buffers(self.command_buffer, 0, &[self.vertex_buffer], &[0]);
            d.cmd_draw(self.command_buffer, 36, 1, 0, 0);
            d.cmd_end_render_pass(self.command_buffer);
            let _ = d.end_command_buffer(self.command_buffer);

            let submit = vk::SubmitInfo::default()
                .command_buffers(std::slice::from_ref(&self.command_buffer));
            if d.queue_submit(dev.queue, &[submit], self.fence).is_err() {
                return false;
            }
            d.wait_for_fences(&[self.fence], true, u64::MAX).is_ok()
        }
    }

    pub fn read_into(&mut self, dev: &NativeDevice, out: &mut [u8]) -> bool {
        let d = &dev.device;
        let need = (self.width * self.height * 4) as usize;
        if out.len() != need {
            return false;
        }
        if self.mapped.is_null() {
            match unsafe {
                d.map_memory(self.color_mem, 0, self.mapped_size, vk::MemoryMapFlags::empty())
            } {
                Ok(p) => self.mapped = p as *mut u8,
                Err(e) => {
                    crate::glog!("[gow.vk.native.scene] map failed: {e:?}");
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
                d.unmap_memory(self.color_mem);
                self.mapped = std::ptr::null_mut();
            }
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.command_pool, None);
            d.destroy_buffer(self.vertex_buffer, None);
            d.free_memory(self.vertex_memory, None);
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.layout, None);
            d.destroy_framebuffer(self.framebuffer, None);
            d.destroy_render_pass(self.render_pass, None);
            d.destroy_image_view(self.color_view, None);
            d.destroy_image(self.color, None);
            d.free_memory(self.color_mem, None);
            d.destroy_image_view(self.depth_view, None);
            d.destroy_image(self.depth, None);
            d.free_memory(self.depth_mem, None);
        }
    }
}

type ColorTarget = (vk::Image, vk::DeviceMemory, vk::ImageView, u64, u64);

fn make_color_target(
    d: &ash::Device,
    mprops: &vk::PhysicalDeviceMemoryProperties,
    width: u32,
    height: u32,
    format: vk::Format,
) -> Option<ColorTarget> {
    unsafe {
        let image = d
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D { width, height, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::LINEAR)
                    .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )
            .ok()?;
        let req = d.get_image_memory_requirements(image);
        let ty = super::render::host_visible_type_pub(mprops, req.memory_type_bits)?;
        let mem = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(ty),
                None,
            )
            .ok()?;
        d.bind_image_memory(image, mem, 0).ok()?;
        let view = d
            .create_image_view(
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
            .ok()?;
        let sub = d.get_image_subresource_layout(
            image,
            vk::ImageSubresource {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                array_layer: 0,
            },
        );
        Some((image, mem, view, sub.row_pitch, sub.size))
    }
}

fn make_depth_target(
    d: &ash::Device,
    mprops: &vk::PhysicalDeviceMemoryProperties,
    width: u32,
    height: u32,
) -> Option<(vk::Image, vk::DeviceMemory, vk::ImageView)> {
    unsafe {
        let image = d
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(vk::Format::D32_SFLOAT)
                    .extent(vk::Extent3D { width, height, depth: 1 })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED),
                None,
            )
            .ok()?;
        let req = d.get_image_memory_requirements(image);
        // Device-local is fine for depth (never mapped).
        let ty = (0..mprops.memory_type_count).find(|&i| (req.memory_type_bits & (1 << i)) != 0)?;
        let mem = d
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(req.size)
                    .memory_type_index(ty),
                None,
            )
            .ok()?;
        d.bind_image_memory(image, mem, 0).ok()?;
        let view = d
            .create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(vk::Format::D32_SFLOAT)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::DEPTH,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    }),
                None,
            )
            .ok()?;
        Some((image, mem, view))
    }
}

fn read_spv(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

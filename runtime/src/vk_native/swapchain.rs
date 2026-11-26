//! `VkSwapchainKHR` creation + a minimal clear/present loop.
//!
//! This is the piece that proves the native backend can actually put pixels on
//! the screen on Turnip: acquire an image, clear it red through a render pass
//! (`VK_ATTACHMENT_LOAD_OP_CLEAR`), submit and present. The D3D11 context
//! stubs in [`crate::vk_native::d3d11`] will later reuse the same swapchain.

use ash::vk;
use std::ffi::c_void;

use super::NativeDevice;

/// A swapchain with per-image image views, framebuffers and command buffers, so
/// a clear+present frame is a single call ([`Swapchain::present_clear`]).
pub struct Swapchain {
    loader: ash::khr::swapchain::Device,
    pub swapchain: vk::SwapchainKHR,
    pub format: vk::Format,
    pub color_space: vk::ColorSpaceKHR,
    pub extent: vk::Extent2D,
    pub images: Vec<vk::Image>,
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    render_pass: vk::RenderPass,
    command_pool: vk::CommandPool,
    command_buffers: Vec<vk::CommandBuffer>,
    // Per-frame sync; one image is in flight at a time here (simple + correct
    // for a proof; a real renderer rotates these).
    image_available: vk::Semaphore,
    render_finished: vk::Semaphore,
    in_flight: vk::Fence,
}

impl Swapchain {
    /// Build a swapchain on `dev`'s surface.
    ///
    /// 2-3 images are requested (per `min_image_count`), a B8G8R8A8/R8G8B8A8
    /// sRGB format is preferred with FIFO present (vsync). For a headless
    /// surface, `currentExtent` is `(0xFFFFFFFF, 0xFFFFFFFF)`, so
    /// `preferred_extent` is used instead.
    pub fn new(dev: &NativeDevice, preferred_extent: vk::Extent2D, vsync: bool) -> Option<Self> {
        let surface = dev.surface;
        let pd = dev.physical_device;
        let inst = &dev.instance;
        let surface_loader = dev.surface_loader();

        let formats = unsafe {
            surface_loader
                .get_physical_device_surface_formats(pd, surface)
                .ok()?
        };
        if formats.is_empty() {
            crate::glog!("[gow.vk.native] no surface formats");
            return None;
        }
        // Prefer an sRGB 8-bit format; D3D11 back buffers are sRGB.
        let fmt = formats
            .iter()
            .find(|f| {
                (f.format == vk::Format::B8G8R8A8_SRGB || f.format == vk::Format::R8G8B8A8_SRGB)
                    && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
            })
            .or_else(|| formats.iter().find(|f| f.format == vk::Format::B8G8R8A8_UNORM))
            .copied()
            .unwrap_or(formats[0]);

        let modes = unsafe {
            surface_loader
                .get_physical_device_surface_present_modes(pd, surface)
                .unwrap_or_default()
        };
        let present_mode = if vsync || modes.contains(&vk::PresentModeKHR::FIFO) {
            vk::PresentModeKHR::FIFO
        } else {
            modes.first().copied().unwrap_or(vk::PresentModeKHR::FIFO)
        };

        let caps = unsafe {
            surface_loader
                .get_physical_device_surface_capabilities(pd, surface)
                .ok()?
        };
        let extent = if caps.current_extent.width == u32::MAX
            || caps.current_extent.height == u32::MAX
        {
            // Headless: caller-provided size, clamped to the reported range.
            let mut e = preferred_extent;
            e.width = e.width.clamp(caps.min_image_extent.width, caps.max_image_extent.width);
            e.height = e
                .height
                .clamp(caps.min_image_extent.height, caps.max_image_extent.height);
            e
        } else {
            caps.current_extent
        };

        // 2-3 images, honouring the driver's min/max.
        let mut images = caps.min_image_count.max(2);
        if caps.max_image_count != 0 {
            images = images.min(caps.max_image_count);
        }
        images = images.min(3);

        let mut queue_family_indices = [dev.families.graphics, dev.families.present];
        let (sharing_mode, queue_ptr, queue_count) = if dev.families.graphics
            == dev.families.present
        {
            (vk::SharingMode::EXCLUSIVE, std::ptr::null(), 0u32)
        } else {
            (
                vk::SharingMode::CONCURRENT,
                queue_family_indices.as_mut_ptr(),
                queue_family_indices.len() as u32,
            )
        };

        let ci = vk::SwapchainCreateInfoKHR::default()
            .surface(surface)
            .min_image_count(images)
            .image_format(fmt.format)
            .image_color_space(fmt.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(sharing_mode)
            .queue_family_indices(&queue_family_indices[..queue_count as usize])
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(vk::SwapchainKHR::null());
        let _ = queue_ptr;

        let loader = ash::khr::swapchain::Device::new(inst, &dev.device);
        let swapchain = unsafe {
            match loader.create_swapchain(&ci, None) {
                Ok(s) => s,
                Err(e) => {
                    crate::glog!("[gow.vk.native] vkCreateSwapchainKHR failed: {e:?}");
                    return None;
                }
            }
        };
        let sc_images = unsafe { loader.get_swapchain_images(swapchain).ok()? };
        crate::glog!(
            "[gow.vk.native] swapchain {}x{} fmt={} {} image(s) present={:?}",
            extent.width,
            extent.height,
            fmt.format.as_raw(),
            sc_images.len(),
            present_mode.as_raw()
        );

        // Render pass: single colour attachment, cleared then presented.
        let color_att = vk::AttachmentDescription::default()
            .format(fmt.format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR);
        let color_ref = [vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
        let subpass = [vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&color_ref)];
        let rp_ci = vk::RenderPassCreateInfo::default()
            .attachments(std::slice::from_ref(&color_att))
            .subpasses(&subpass);
        let render_pass = unsafe { dev.device.create_render_pass(&rp_ci, None).ok()? };

        // Views + framebuffers per image.
        let mut views = Vec::with_capacity(sc_images.len());
        let mut framebuffers = Vec::with_capacity(sc_images.len());
        for img in &sc_images {
            let iv = vk::ImageViewCreateInfo::default()
                .image(*img)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(fmt.format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let view = match unsafe { dev.device.create_image_view(&iv, None) } {
                Ok(v) => v,
                Err(e) => {
                    crate::glog!("[gow.vk.native] create_image_view failed: {e:?}");
                    return None;
                }
            };
            views.push(view);
            let fb_ci = vk::FramebufferCreateInfo::default()
                .render_pass(render_pass)
                .attachments(std::slice::from_ref(&view))
                .width(extent.width)
                .height(extent.height)
                .layers(1);
            framebuffers.push(unsafe { dev.device.create_framebuffer(&fb_ci, None).ok()? });
        }

        // Command pool + one buffer per image, each recording a clear.
        let pool_ci = vk::CommandPoolCreateInfo::default()
            .queue_family_index(dev.families.graphics)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        let command_pool = unsafe { dev.device.create_command_pool(&pool_ci, None).ok()? };
        let cb_ai = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(sc_images.len() as u32);
        let command_buffers = unsafe { dev.device.allocate_command_buffers(&cb_ai).ok()? };

        // Sync objects.
        let sem_ci = vk::SemaphoreCreateInfo::default();
        let fence_ci = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        let image_available = unsafe { dev.device.create_semaphore(&sem_ci, None).ok()? };
        let render_finished = unsafe { dev.device.create_semaphore(&sem_ci, None).ok()? };
        let in_flight = unsafe { dev.device.create_fence(&fence_ci, None).ok()? };

        let mut sc = Swapchain {
            loader,
            swapchain,
            format: fmt.format,
            color_space: fmt.color_space,
            extent,
            images: sc_images,
            views,
            framebuffers,
            render_pass,
            command_pool,
            command_buffers,
            image_available,
            render_finished,
            in_flight,
        };
        sc.record_clear_all(dev, [1.0, 0.0, 0.0, 1.0]);
        Some(sc)
    }

    /// Record a clear-to-`color` render pass into every image's command buffer.
    fn record_clear_all(&mut self, dev: &NativeDevice, color: [f32; 4]) {
        let clear = vk::ClearValue {
            color: vk::ClearColorValue { float32: color },
        };
        for (i, cb) in self.command_buffers.iter().enumerate() {
            let begin = vk::CommandBufferBeginInfo::default();
            unsafe {
                if dev.device.begin_command_buffer(*cb, &begin).is_err() {
                    continue;
                }
                let rp_begin = vk::RenderPassBeginInfo::default()
                    .render_pass(self.render_pass)
                    .framebuffer(self.framebuffers[i])
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: self.extent,
                    })
                    .clear_values(std::slice::from_ref(&clear));
                dev.device
                    .cmd_begin_render_pass(*cb, &rp_begin, vk::SubpassContents::INLINE);
                dev.device.cmd_end_render_pass(*cb);
                let _ = dev.device.end_command_buffer(*cb);
            }
        }
    }

    /// One complete frame: acquire -> submit the pre-recorded clear -> present.
    ///
    /// Returns the acquired image index. This is the "screen goes red" proof.
    pub fn present_clear(&self, dev: &NativeDevice, color: [f32; 4]) -> Option<u32> {
        let _ = color; // buffers pre-recorded at creation; a real ctx re-records
        let device = &dev.device;
        unsafe {
            device
                .wait_for_fences(&[self.in_flight], true, u64::MAX)
                .ok()?;
            let (index, _suboptimal) = match self.loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.image_available,
                vk::Fence::null(),
            ) {
                Ok(v) => v,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return None,
                Err(e) => {
                    crate::glog!("[gow.vk.native] acquire_next_image failed: {e:?}");
                    return None;
                }
            };
            let cb = self.command_buffers[index as usize % self.command_buffers.len()];
            device.reset_fences(&[self.in_flight]).ok()?;

            let wait_stage = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let wait_sems = [self.image_available];
            let signal_sems = [self.render_finished];
            let cbs = [cb];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&wait_sems)
                .wait_dst_stage_mask(&wait_stage)
                .command_buffers(&cbs)
                .signal_semaphores(&signal_sems);
            if let Err(e) = device.queue_submit(dev.queue, &[submit], self.in_flight) {
                crate::glog!("[gow.vk.native] queue_submit failed: {e:?}");
                return None;
            }

            let swapchains = [self.swapchain];
            let indices = [index];
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&signal_sems)
                .swapchains(&swapchains)
                .image_indices(&indices);
            match self.loader.queue_present(dev.queue, &present) {
                Ok(_) => Some(index),
                Err(e) => {
                    crate::glog!("[gow.vk.native] queue_present failed: {e:?}");
                    None
                }
            }
        }
    }

    /// Wait for the device to be idle and destroy everything this swapchain owns
    /// (it does not touch the device/instance, which outlive it).
    pub fn destroy(&mut self, dev: &NativeDevice) {
        let d = &dev.device;
        unsafe {
            let _ = d.device_wait_idle();
            d.destroy_fence(self.in_flight, None);
            d.destroy_semaphore(self.render_finished, None);
            d.destroy_semaphore(self.image_available, None);
            d.destroy_command_pool(self.command_pool, None);
            for fb in self.framebuffers.drain(..) {
                d.destroy_framebuffer(fb, None);
            }
            for v in self.views.drain(..) {
                d.destroy_image_view(v, None);
            }
            d.destroy_render_pass(self.render_pass, None);
            self.loader.destroy_swapchain(self.swapchain, None);
        }
    }

    /// A raw window handle for the eventual D3D11 context (unused right now).
    pub fn _window_hint(&self) -> *mut c_void {
        std::ptr::null_mut()
    }
}

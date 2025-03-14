// Copyright (c) 2021 Okko Hakola
// Licensed under the Apache License, Version 2.0
// <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT
// license <LICENSE-MIT or https://opensource.org/licenses/MIT>,
// at your option. All files in the project carrying such
// notice may not be copied, modified, or distributed except
// according to those terms.
use std::sync::Arc;

use egui::{ClippedPrimitive, TexturesDelta};
use vulkano::{
    command_buffer::SecondaryAutoCommandBuffer, device::Queue, image::ImageViewAbstract,
    render_pass::Subpass, swapchain::Surface, sync::GpuFuture,
};
use winit::window::Window;

use crate::{
    renderer::Renderer,
    utils::{immutable_texture_from_bytes, immutable_texture_from_file},
};

pub struct Gui {
    pub egui_ctx: egui::Context,
    pub egui_winit: egui_winit::State,
    renderer: Renderer,
    surface: Arc<Surface<Window>>,

    shapes: Vec<egui::epaint::ClippedShape>,
    textures_delta: egui::TexturesDelta,
}

impl Gui {
    /// Creates new Egui to Vulkano integration by setting the necessary parameters
    /// This is to be called once we have access to vulkano_win's winit window surface
    /// and gfx queue. Created with this, the renderer will own a render pass which is useful to e.g. place your render pass' images
    /// onto egui windows
    /// - `surface`: Vulkano's Winit Surface [`Arc<Surface<Window>>`]
    /// - `gfx_queue`: Vulkano's [`Queue`]
    /// - `is_overlay`: If true, you should be responsible for clearing the image before `draw_on_image`, else it gets cleared
    ///
    /// Note that your swapchain images should be created with `vulkano::format::Format::B8G8R8A8_SRGB`
    pub fn new(surface: Arc<Surface<Window>>, gfx_queue: Arc<Queue>, is_overlay: bool) -> Gui {
        let format = vulkano::format::Format::B8G8R8A8_SRGB;
        let formats = gfx_queue
            .device()
            .physical_device()
            .surface_formats(&surface, Default::default())
            .unwrap();
        assert!(
            formats.iter().find(|f| f.0 == format).is_some(),
            "Swapchain format does not support {:?}",
            format
        );
        let max_texture_side =
            gfx_queue.device().physical_device().properties().max_image_array_layers as usize;
        let renderer = Renderer::new_with_render_pass(gfx_queue, format, is_overlay);
        Gui {
            egui_ctx: Default::default(),
            egui_winit: egui_winit::State::new(max_texture_side, surface.window()),
            renderer,
            surface,
            shapes: vec![],
            textures_delta: Default::default(),
        }
    }

    /// Same as `new` but instead of integration owning a render pass, egui renders on your subpass
    ///
    /// Note that your swapchain images should be created with `vulkano::format::Format::B8G8R8A8_SRGB`
    pub fn new_with_subpass(
        surface: Arc<Surface<Window>>,
        gfx_queue: Arc<Queue>,
        subpass: Subpass,
    ) -> Gui {
        let format = vulkano::format::Format::B8G8R8A8_SRGB;
        let formats = gfx_queue
            .device()
            .physical_device()
            .surface_formats(&surface, Default::default())
            .unwrap();
        assert!(
            formats.iter().find(|f| f.0 == format).is_some(),
            "Swapchain format does not support {:?}",
            format
        );
        let max_texture_side =
            gfx_queue.device().physical_device().properties().max_image_array_layers as usize;
        let renderer = Renderer::new_with_subpass(gfx_queue, format, subpass);
        Gui {
            egui_ctx: Default::default(),
            egui_winit: egui_winit::State::new(max_texture_side, surface.window()),
            renderer,
            surface,
            shapes: vec![],
            textures_delta: Default::default(),
        }
    }

    /// Updates context state by winit window event.
    /// Returns `true` if egui wants exclusive use of this event
    /// (e.g. a mouse click on an egui window, or entering text into a text field).
    /// For instance, if you use egui for a game, you want to first call this
    /// and only when this returns `false` pass on the events to your game.
    ///
    /// Note that egui uses `tab` to move focus between elements, so this will always return `true` for tabs.
    pub fn update(&mut self, winit_event: &winit::event::WindowEvent<'_>) -> bool {
        self.egui_winit.on_event(&self.egui_ctx, winit_event)
    }

    /// Begins Egui frame & determines what will be drawn later. This must be called before draw, and after `update` (winit event).
    pub fn immediate_ui(&mut self, layout_function: impl FnOnce(&mut Self)) {
        let raw_input = self.egui_winit.take_egui_input(self.surface.window());
        self.egui_ctx.begin_frame(raw_input);
        // Render Egui
        layout_function(self);
    }

    /// If you wish to better control when to begin frame, do so by calling this function
    /// (Finish by drawing)
    pub fn begin_frame(&mut self) {
        let raw_input = self.egui_winit.take_egui_input(self.surface.window());
        self.egui_ctx.begin_frame(raw_input);
    }

    /// Renders ui on `final_image` & Updates cursor icon
    /// Finishes Egui frame
    /// - `before_future` = Vulkano's GpuFuture
    /// - `final_image` = Vulkano's image (render target)
    pub fn draw_on_image<F>(
        &mut self,
        before_future: F,
        final_image: Arc<dyn ImageViewAbstract + 'static>,
    ) -> Box<dyn GpuFuture>
    where
        F: GpuFuture + 'static,
    {
        if !self.renderer.has_renderpass() {
            panic!(
                "Gui integration has been created with subpass, use `draw_on_subpass_image` \
                 instead"
            )
        }

        let format = final_image.format();
        if format != Some(vulkano::format::Format::B8G8R8A8_SRGB) {
            panic!(
                "Render target image color format is wrong {:?}, should be {:?}",
                format,
                Some(vulkano::format::Format::B8G8R8A8_SRGB)
            );
        }

        let (clipped_meshes, textures_delta) = self.extract_draw_data_at_frame_end();

        self.renderer.draw_on_image(
            &clipped_meshes,
            &textures_delta,
            self.egui_winit.pixels_per_point(),
            before_future,
            final_image,
        )
    }

    /// Creates commands for rendering ui on subpass' image and returns the command buffer for execution on your side
    /// - Finishes Egui frame
    /// - You must execute the secondary command buffer yourself
    pub fn draw_on_subpass_image(
        &mut self,
        image_dimensions: [u32; 2],
    ) -> SecondaryAutoCommandBuffer {
        if self.renderer.has_renderpass() {
            panic!(
                "Gui integration has been created with its own render pass, use `draw_on_image` \
                 instead"
            )
        }

        let (clipped_meshes, textures_delta) = self.extract_draw_data_at_frame_end();

        self.renderer.draw_on_subpass_image(
            &clipped_meshes,
            &textures_delta,
            self.egui_winit.pixels_per_point(),
            image_dimensions,
        )
    }

    fn extract_draw_data_at_frame_end(&mut self) -> (Vec<ClippedPrimitive>, TexturesDelta) {
        self.end_frame();
        let shapes = std::mem::take(&mut self.shapes);
        let textures_delta = std::mem::take(&mut self.textures_delta);
        let clipped_meshes = self.egui_ctx.tessellate(shapes);
        (clipped_meshes, textures_delta)
    }

    fn end_frame(&mut self) {
        let egui::FullOutput { platform_output, needs_repaint: _n, textures_delta, shapes } =
            self.egui_ctx.end_frame();

        self.egui_winit.handle_platform_output(
            self.surface.window(),
            &self.egui_ctx,
            platform_output,
        );
        self.shapes = shapes;
        self.textures_delta = textures_delta;
    }

    /// Registers a user image from Vulkano image view to be used by egui
    pub fn register_user_image_view(
        &mut self,
        image: Arc<dyn ImageViewAbstract + Send + Sync>,
    ) -> egui::TextureId {
        self.renderer.register_image(image)
    }

    /// Registers a user image to be used by egui
    /// - `image_file_bytes`: e.g. include_bytes!("./assets/tree.png")
    /// - `format`: e.g. vulkano::format::Format::R8G8B8A8Unorm
    pub fn register_user_image(
        &mut self,
        image_file_bytes: &[u8],
        format: vulkano::format::Format,
    ) -> egui::TextureId {
        let image = immutable_texture_from_file(self.renderer.queue(), image_file_bytes, format)
            .expect("Failed to create image");
        self.renderer.register_image(image)
    }

    pub fn register_user_image_from_bytes(
        &mut self,
        image_byte_data: &[u8],
        dimensions: [u32; 2],
        format: vulkano::format::Format,
    ) -> egui::TextureId {
        let image = immutable_texture_from_bytes(
            self.renderer.queue(),
            image_byte_data,
            dimensions,
            format,
        )
        .expect("Failed to create image");
        self.renderer.register_image(image)
    }

    /// Unregisters a user image
    pub fn unregister_user_image(&mut self, texture_id: egui::TextureId) {
        self.renderer.unregister_image(texture_id);
    }

    /// Access egui's context (which can be used to e.g. set fonts, visuals etc)
    pub fn context(&self) -> egui::Context {
        self.egui_ctx.clone()
    }
}

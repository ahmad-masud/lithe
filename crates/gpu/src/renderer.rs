//! wgpu device and the single draw pass.
//!
//! The whole UI is one instanced draw against one coverage atlas. A frame is
//! therefore: fill a reused instance vector, upload it once, submit. There is
//! no retained scene, no widget tree and no per-frame allocation.
//!
//! The renderer can target either a window surface or an offscreen texture.
//! The offscreen path exists so the real drawing code can be rendered and
//! inspected pixel-for-pixel without a display, which is the only honest way
//! to regression-test a hand-rolled text renderer.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use winit::window::Window;

use crate::atlas::{Atlas, CellMetrics, Face, Family, Size};
use crate::Rgba;

/// One instance. 48 bytes, so a dense 4K screenful of cells is well under a
/// megabyte of upload.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Quad {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 4],
    color: [f32; 4],
    /// 0 for the glyph atlas, 1 for an image texture, 2 for a rounded
    /// rectangle drawn by the shader rather than sampled.
    mode: f32,
    /// Corner radius in physical px, for mode 2.
    radius: f32,
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    screen: [f32; 2],
    _pad: [f32; 2],
}

const INITIAL_QUAD_CAPACITY: usize = 8192;

enum Target {
    Window {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    Offscreen {
        texture: wgpu::Texture,
    },
}

/// A decoded image living on the GPU.
///
/// Images do not go in the glyph atlas: that is a single-channel coverage
/// texture sized for glyphs, and a screenshot is neither. Each image gets
/// its own texture and its own bind group, and drawing one splits the
/// frame into batches — see [`Frame::present`].
pub struct Image {
    bind_group: wgpu::BindGroup,
    pub width: u32,
    pub height: u32,
}

/// A run of quads sharing one texture.
struct Batch {
    end: u32,
    /// `None` means the glyph atlas.
    image: Option<usize>,
}

pub struct Renderer {
    target: Target,
    // Every wgpu handle is an Arc underneath, so these clone freely and a
    // second window shares one device rather than opening its own.
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,

    width: u32,
    height: u32,

    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    globals: wgpu::Buffer,
    instances: wgpu::Buffer,
    instance_capacity: usize,

    /// Shared between windows: one rasterised glyph set, not one per window.
    /// The font system behind it is ~8 MB, so this is most of what a second
    /// window would otherwise cost.
    atlas: Rc<RefCell<Atlas>>,
    /// Reused across frames; cleared, never reallocated in steady state.
    quads: Vec<Quad>,
    /// Where the texture changes within `quads`.
    batches: Vec<Batch>,
    /// Images drawn this frame, borrowed by index.
    frame_images: Vec<wgpu::BindGroup>,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    clear: Rgba,
    scale: f32,
}

impl Renderer {
    /// Targets a window. `font_pt` is in logical points; the display scale is
    /// applied here so glyphs are rasterised at true device resolution.
    pub fn new(window: Arc<Window>, font_pt: f32) -> Result<Self> {
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;

        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance
            .create_surface(window)
            .context("failed to create surface for window")?;

        let (adapter, device, queue) = request_gpu(&instance, Some(&surface))?;
        let shared_instance = instance.clone();
        let shared_adapter = adapter.clone();

        let caps = surface.get_capabilities(&adapter);

        // Deliberately a non-sRGB format. Font rasterisers produce coverage
        // meant to be composited in gamma space; blending it in linear space
        // makes text look thin and washed out. A UNORM target fed sRGB values
        // keeps glyph weight as the rasteriser intended.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow!("surface is incompatible with the chosen adapter"))?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);

        let (width, height) = (config.width, config.height);
        let atlas = Rc::new(RefCell::new(Atlas::new(
            &device,
            &queue,
            (font_pt * scale).round(),
        )));
        Self::assemble(
            Target::Window { surface, config },
            shared_instance,
            shared_adapter,
            device,
            queue,
            atlas,
            width,
            height,
            format,
            scale,
        )
    }

    /// Builds a renderer for another window on this one's device, sharing
    /// the glyph atlas and pipeline. Only the surface and the per-window
    /// buffers are new.
    pub fn new_sharing(&self, window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let surface = self
            .instance
            .create_surface(window)
            .context("failed to create surface for the new window")?;

        let caps = surface.get_capabilities(&self.adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let mut config = surface
            .get_default_config(&self.adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow!("surface is incompatible with the adapter"))?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&self.device, &config);

        let (width, height) = (config.width, config.height);
        Self::assemble(
            Target::Window { surface, config },
            self.instance.clone(),
            self.adapter.clone(),
            self.device.clone(),
            self.queue.clone(),
            Rc::clone(&self.atlas),
            width,
            height,
            format,
            self.scale,
        )
    }

    /// Targets an offscreen texture. No window, no display, no surface — for
    /// tests and for generating reference images.
    pub fn offscreen(width: u32, height: u32, scale: f32, font_pt: f32) -> Result<Self> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let (adapter, device, queue) = request_gpu(&instance, None)?;

        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lithe.offscreen"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let atlas = Rc::new(RefCell::new(Atlas::new(
            &device,
            &queue,
            (font_pt * scale).round(),
        )));
        Self::assemble(
            Target::Offscreen { texture },
            instance,
            adapter,
            device,
            queue,
            atlas,
            width,
            height,
            format,
            scale,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        target: Target,
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        atlas: Rc<RefCell<Atlas>>,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        scale: f32,
    ) -> Result<Self> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lithe.quad"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lithe.globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lithe.atlas.sampler"),
            // Nearest, not linear: glyphs are rasterised at exact physical
            // size and placed on integer pixels, so filtering would only
            // blur them.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lithe.bind.layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lithe.bind"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas.borrow().view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lithe.pipeline.layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lithe.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32x4,
                        3 => Float32x4,
                        4 => Float32,
                        5 => Float32
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lithe.instances"),
            size: (INITIAL_QUAD_CAPACITY * std::mem::size_of::<Quad>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            target,
            instance,
            adapter,
            device,
            queue,
            width,
            height,
            pipeline,
            bind_group,
            globals,
            instances,
            instance_capacity: INITIAL_QUAD_CAPACITY,
            atlas,
            quads: Vec::with_capacity(INITIAL_QUAD_CAPACITY),
            batches: Vec::new(),
            frame_images: Vec::new(),
            layout,
            sampler,
            clear: Rgba::BLACK,
            scale,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.width = width;
        self.height = height;
        if let Target::Window { surface, config } = &mut self.target {
            config.width = width;
            config.height = height;
            surface.configure(&self.device, config);
        }
    }

    pub fn cell(&self) -> CellMetrics {
        self.atlas.borrow().metrics
    }

    /// Grid geometry at a chosen glyph size, for laying out the preview's
    /// headings. Measures that size on first ask and remembers it.
    pub fn cell_at(&self, size: Size) -> CellMetrics {
        self.atlas.borrow_mut().metrics_at(size)
    }

    /// Line geometry for a chosen face and size.
    pub fn cell_in(&self, size: Size, family: Family) -> CellMetrics {
        self.atlas.borrow_mut().metrics_in(size, family)
    }

    /// How wide `text` will be drawn, in physical pixels.
    ///
    /// The same per-glyph advances [`Frame::text_run`] moves the pen by,
    /// so a line measured here and drawn there cannot disagree — which is
    /// the whole reason the preview can wrap proportional text at all.
    /// Rasterises anything it has not seen, which is why it takes the
    /// queue: measuring a document is also what warms its glyphs.
    pub fn measure(&self, text: &str, face: Face, size: Size, family: Family) -> f32 {
        self.atlas
            .borrow_mut()
            .measure(&self.queue, text, face, size, family)
    }

    /// How wide a run of chrome text is. See [`Frame::ui_text`].
    pub fn ui_measure(&self, text: &str) -> f32 {
        self.measure(text, Face::Regular, Size::Body, Family::Prose)
    }

    /// Target size in physical pixels.
    pub fn size_px(&self) -> (f32, f32) {
        (self.width as f32, self.height as f32)
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Uploads decoded RGBA pixels as an image that can be drawn.
    ///
    /// Kept by the caller and reused across frames: decoding and
    /// uploading a photograph on every redraw would be absurd.
    pub fn upload_image(&self, rgba: &[u8], width: u32, height: u32) -> Result<Image> {
        if width == 0 || height == 0 || rgba.len() < (width * height * 4) as usize {
            return Err(anyhow!("image data does not match {width}x{height}"));
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lithe.image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Linear here, unlike the glyph atlas: a photograph scaled to fit
        // a window wants smoothing, where a glyph wants none.
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lithe.image.sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let _ = &self.sampler;
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lithe.image.bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        Ok(Image {
            bind_group,
            width,
            height,
        })
    }

    /// Begins a frame. Drawing is immediate-mode into a reused buffer, but
    /// nothing reaches the GPU until [`Frame::present`].
    pub fn frame(&mut self, clear: Rgba) -> Frame<'_> {
        self.quads.clear();
        self.batches.clear();
        self.frame_images.clear();
        self.clear = clear;
        Frame { r: self }
    }

    /// Reads the offscreen target back as tightly packed RGBA8. Errors when
    /// the renderer targets a window.
    pub fn read_rgba(&self) -> Result<Vec<u8>> {
        let Target::Offscreen { texture } = &self.target else {
            return Err(anyhow!("read_rgba is only valid for an offscreen renderer"));
        };

        // copy_texture_to_buffer requires rows aligned to 256 bytes, so the
        // staging buffer is padded and unpadded after mapping.
        let unpadded = self.width as usize * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let padded = unpadded.div_ceil(align) * align;

        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lithe.readback"),
            size: (padded * self.height as usize) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lithe.readback.encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| anyhow!("device poll failed during readback: {e:?}"))?;
        rx.recv()
            .context("readback channel closed")?
            .map_err(|e| anyhow!("buffer map failed: {e:?}"))?;

        let view = slice
            .get_mapped_range()
            .map_err(|e| anyhow!("mapped range unavailable: {e:?}"))?;
        let mut out = Vec::with_capacity(unpadded * self.height as usize);
        for row in view.chunks_exact(padded) {
            out.extend_from_slice(&row[..unpadded]);
        }
        drop(view);
        staging.unmap();
        Ok(out)
    }
}

fn request_gpu(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'static>>,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    // LowPower picks the integrated GPU where there is a choice. Drawing a
    // few thousand quads does not need the discrete one, and on a laptop the
    // battery difference is the whole point.
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: surface,
        force_fallback_adapter: false,
        ..Default::default()
    }))
    .map_err(|e| anyhow!("no suitable GPU adapter: {e}"))?;

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("lithe.device"),
        required_features: wgpu::Features::empty(),
        // downlevel_defaults keeps us honest about what we use and works on
        // anything from the last decade.
        required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    }))
    .context("failed to acquire GPU device")?;

    Ok((adapter, device, queue))
}

pub struct Frame<'a> {
    r: &'a mut Renderer,
}

impl Frame<'_> {
    /// Filled rectangle, physical pixels, origin top-left.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        if w <= 0.0 || h <= 0.0 || color.0[3] <= 0.0 {
            return;
        }
        let uv = self.r.atlas.borrow().opaque;
        self.r.quads.push(Quad {
            pos: [x, y],
            size: [w, h],
            uv: [uv.min[0], uv.min[1], uv.max[0], uv.max[1]],
            color: color.0,
            mode: 0.0,
            radius: 0.0,
            _pad: [0.0; 2],
        });
    }

    /// Draws `text` with its baseline at `baseline_y`, starting at `x`.
    /// Returns the x position just past the last cell.
    pub fn text(&mut self, x: f32, baseline_y: f32, text: &str, color: Rgba) -> f32 {
        self.text_in(x, baseline_y, text, color, Face::Regular)
    }

    /// As [`Frame::text`], in a chosen face.
    pub fn text_in(&mut self, x: f32, baseline_y: f32, text: &str, color: Rgba, face: Face) -> f32 {
        self.text_sized(x, baseline_y, text, color, face, Size::Body)
    }

    /// As [`Frame::text_in`], at a chosen size.
    ///
    /// Only the preview asks for anything but [`Size::Body`]; the editor
    /// itself is one size by design, since a uniform grid is what lets a
    /// screenful of text cost one instance per cell.
    pub fn text_sized(
        &mut self,
        x: f32,
        baseline_y: f32,
        text: &str,
        color: Rgba,
        face: Face,
        size: Size,
    ) -> f32 {
        // Disjoint field borrows, spelled out so the calls below do not fight
        // the borrow checker.
        let Renderer {
            atlas,
            queue,
            quads,
            ..
        } = &mut *self.r;
        let mut atlas = atlas.borrow_mut();

        let advance = atlas.metrics_at(size).advance;
        let mut pen = x;

        for ch in text.chars() {
            if let Some(g) = atlas.glyph(queue, ch, face, size) {
                quads.push(Quad {
                    pos: [
                        (pen + g.offset[0]).round(),
                        (baseline_y + g.offset[1]).round(),
                    ],
                    size: g.size,
                    uv: [g.uv.min[0], g.uv.min[1], g.uv.max[0], g.uv.max[1]],
                    color: color.0,
                    mode: 0.0,
                    radius: 0.0,
                    _pad: [0.0; 2],
                });
            }
            pen += advance;
        }
        pen
    }

    /// As [`Frame::text_sized`], in a chosen face.
    ///
    /// Eight arguments, because drawing one run of a document is eight
    /// facts: where, what, in what colour, weight, slant, size and
    /// face.
    ///
    /// The run is shaped as a whole rather than a glyph at a time,
    /// which is what makes prose prose: the shaper decides where each
    /// glyph sits, so `To` and `AVATAR` are kerned and `fi` may be one
    /// glyph. Returns the x it ended at, so a caller drawing a line of
    /// runs can hand the next one its start.
    #[allow(clippy::too_many_arguments)]
    pub fn text_run(
        &mut self,
        x: f32,
        baseline_y: f32,
        text: &str,
        color: Rgba,
        face: Face,
        size: Size,
        family: Family,
    ) -> f32 {
        if family == Family::Mono {
            return self.text_sized(x, baseline_y, text, color, face, size);
        }
        let Renderer {
            atlas,
            queue,
            quads,
            ..
        } = &mut *self.r;
        let mut atlas = atlas.borrow_mut();
        // One shaped run, which is also what `measure` reports: the
        // glyphs go where the shaper put them, kerning and all, and
        // the pen ends where the measurement said it would.
        let run = atlas.run(queue, text, face, size, family);
        for glyph in &run.glyphs {
            quads.push(Quad {
                pos: [
                    (x + glyph.x + glyph.offset[0]).round(),
                    (baseline_y + glyph.offset[1]).round(),
                ],
                size: glyph.size,
                uv: [
                    glyph.uv.min[0],
                    glyph.uv.min[1],
                    glyph.uv.max[0],
                    glyph.uv.max[1],
                ],
                color: color.0,
                mode: 0.0,
                radius: 0.0,
                _pad: [0.0; 2],
            });
        }
        x + run.width
    }

    /// How wide `text` would be drawn here. See [`Renderer::measure`].
    pub fn measure(&self, text: &str, face: Face, size: Size, family: Family) -> f32 {
        self.r.measure(text, face, size, family)
    }

    /// The grid's geometry, for a caller laying out around the text it
    /// is drawing.
    pub fn cell(&self) -> CellMetrics {
        self.r.cell()
    }

    /// Draws an image into a rectangle.
    ///
    /// Ends the current run of atlas quads and starts one bound to this
    /// image, so the frame becomes several draws instead of one. That is
    /// the cost of showing a picture, and it is paid only when there is
    /// one on screen.
    pub fn image(&mut self, image: &Image, x: f32, y: f32, w: f32, h: f32) {
        let r = &mut *self.r;
        r.batches.push(Batch {
            end: r.quads.len() as u32,
            image: None,
        });
        r.frame_images.push(image.bind_group.clone());
        let index = r.frame_images.len() - 1;

        r.quads.push(Quad {
            pos: [x, y],
            size: [w, h],
            uv: [0.0, 0.0, 1.0, 1.0],
            color: [1.0, 1.0, 1.0, 1.0],
            mode: 1.0,
            radius: 0.0,
            _pad: [0.0; 2],
        });
        r.batches.push(Batch {
            end: r.quads.len() as u32,
            image: Some(index),
        });
    }

    /// Chrome text: a tab label, a status bar, a panel row.
    ///
    /// The interface is set in the reading face, not the grid's. The
    /// grid's face is for the document, where a column means
    /// something; a tab label set in it is a terminal pretending to be
    /// an editor, which is most of what made this look its age.
    pub fn ui_text(&mut self, x: f32, baseline_y: f32, text: &str, color: Rgba) -> f32 {
        self.text_run(
            x,
            baseline_y,
            text,
            color,
            Face::Regular,
            Size::Body,
            Family::Prose,
        )
    }

    /// How wide [`Frame::ui_text`] would draw this.
    pub fn ui_measure(&self, text: &str) -> f32 {
        self.measure(text, Face::Regular, Size::Body, Family::Prose)
    }

    /// A filled rectangle with rounded corners.
    ///
    /// Drawn by the shader as a distance field rather than sampled, so
    /// it costs the same one quad a square rect does — which is what
    /// makes it affordable to put one behind every piece of inline
    /// code on the page.
    pub fn rounded(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Rgba) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let uv = self.r.atlas.borrow().opaque;
        self.r.quads.push(Quad {
            pos: [x, y],
            size: [w, h],
            uv: [uv.min[0], uv.min[1], uv.max[0], uv.max[1]],
            color: color.0,
            mode: 2.0,
            // Never more than half the shorter side, or opposite
            // corners overlap and the shape turns inside out.
            radius: radius.min(w.min(h) / 2.0).max(0.0),
            _pad: [0.0; 2],
        });
    }

    /// Convenience for the common case: text in a grid cell.
    pub fn cell_text(&mut self, col: f32, row: f32, text: &str, color: Rgba) -> f32 {
        let m = self.r.atlas.borrow().metrics;
        self.text(col * m.advance, row * m.line_height + m.ascent, text, color)
    }

    /// Uploads the frame and submits it. One pass, one draw.
    pub fn present(self) {
        let r = self.r;

        // Acquire the target view first; the window path can bail here.
        let acquired = match &r.target {
            Target::Window { surface, config } => {
                use wgpu::CurrentSurfaceTexture as Acquired;
                match surface.get_current_texture() {
                    // Suboptimal still draws correctly; a later resize fixes it.
                    Acquired::Success(f) | Acquired::Suboptimal(f) => Some(f),
                    // Stale surface (resize, display change). Reconfigure and
                    // drop this frame; the next redraw lands.
                    Acquired::Outdated | Acquired::Lost => {
                        surface.configure(&r.device, config);
                        return;
                    }
                    // Nothing to draw into, and nothing to fix.
                    Acquired::Timeout | Acquired::Occluded => return,
                    other => {
                        log::error!("surface acquire failed: {other:?}");
                        return;
                    }
                }
            }
            Target::Offscreen { .. } => None,
        };

        let view = match (&acquired, &r.target) {
            (Some(frame), _) => frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
            (None, Target::Offscreen { texture }) => {
                texture.create_view(&wgpu::TextureViewDescriptor::default())
            }
            _ => unreachable!("window target always yields an acquired texture"),
        };

        r.queue.write_buffer(
            &r.globals,
            0,
            bytemuck::bytes_of(&Globals {
                screen: [r.width as f32, r.height as f32],
                _pad: [0.0, 0.0],
            }),
        );

        // Grow by doubling. In practice this fires once, early.
        if r.quads.len() > r.instance_capacity {
            r.instance_capacity = r.quads.len().next_power_of_two();
            r.instances = r.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lithe.instances"),
                size: (r.instance_capacity * std::mem::size_of::<Quad>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !r.quads.is_empty() {
            r.queue
                .write_buffer(&r.instances, 0, bytemuck::cast_slice(&r.quads));
        }

        let mut encoder = r
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lithe.encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lithe.pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: r.clear.0[0] as f64,
                            g: r.clear.0[1] as f64,
                            b: r.clear.0[2] as f64,
                            a: r.clear.0[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            if !r.quads.is_empty() {
                pass.set_pipeline(&r.pipeline);
                pass.set_vertex_buffer(0, r.instances.slice(..));

                // One draw per texture. With no images on screen that is
                // the single draw it has always been.
                let mut start = 0u32;
                let total = r.quads.len() as u32;
                for batch in r.batches.iter().chain(std::iter::once(&Batch {
                    end: total,
                    image: None,
                })) {
                    if batch.end <= start {
                        continue;
                    }
                    let group = match batch.image {
                        Some(index) => &r.frame_images[index],
                        None => &r.bind_group,
                    };
                    pass.set_bind_group(0, group, &[]);
                    pass.draw(0..6, start..batch.end);
                    start = batch.end;
                }
            }
        }
        r.queue.submit(Some(encoder.finish()));

        if let Some(frame) = acquired {
            r.queue.present(frame);
        }
    }
}

#[cfg(test)]
mod shaping_tests {
    use super::*;

    fn renderer() -> Option<Renderer> {
        Renderer::offscreen(400, 200, 1.0, 16.0).ok()
    }

    /// The point of shaping a run rather than a glyph at a time. A
    /// capital-heavy word set character by character is several per
    /// cent too wide, and a heading — most capitals, largest size — is
    /// where that shows most.
    #[test]
    fn a_shaped_run_is_tighter_than_the_sum_of_its_letters() {
        let Some(r) = renderer() else { return };
        let one = |text: &str| r.measure(text, Face::Regular, Size::Body, Family::Prose);
        for word in ["AVATAR", "To the Wall", "Walking Away, Type Vo."] {
            let whole = one(word);
            let letters: f32 = word.chars().map(|c| one(&c.to_string())).sum();
            assert!(
                whole < letters,
                "{word:?} shaped {whole} but letter by letter {letters}"
            );
        }
    }

    /// The grid depends on every cell being exactly one advance wide,
    /// so the monospace face must *not* be kerned.
    #[test]
    fn the_grid_face_is_not_shaped() {
        let Some(r) = renderer() else { return };
        let cell = r.cell().advance;
        for text in ["AVATAR", "iiii", "fi fl"] {
            let width = r.measure(text, Face::Regular, Size::Body, Family::Mono);
            let want = cell * text.chars().count() as f32;
            assert!(
                (width - want).abs() < 0.01,
                "{text:?} measured {width}, the grid says {want}"
            );
        }
    }

    /// What measuring is for: a run reports the width it will be drawn
    /// at. Both now read one shaped array, so this cannot drift.
    #[test]
    fn measuring_a_run_agrees_with_drawing_it() {
        let Some(mut r) = renderer() else { return };
        let text = "Walking Away, Type Vo.";
        let want = r.measure(text, Face::Regular, Size::Body, Family::Prose);
        let mut frame = r.frame(Rgba::BLACK);
        let end = frame.text_run(
            10.0,
            20.0,
            text,
            Rgba::BLACK,
            Face::Regular,
            Size::Body,
            Family::Prose,
        );
        assert!(
            (end - 10.0 - want).abs() < 0.01,
            "drawn to {end}, measured {want} from 10"
        );
    }

    #[test]
    fn an_empty_run_measures_nothing_and_draws_nothing() {
        let Some(mut r) = renderer() else { return };
        assert_eq!(r.measure("", Face::Regular, Size::Body, Family::Prose), 0.0);
        let mut frame = r.frame(Rgba::BLACK);
        let end = frame.text_run(
            7.0,
            9.0,
            "",
            Rgba::BLACK,
            Face::Regular,
            Size::Body,
            Family::Prose,
        );
        assert_eq!(end, 7.0, "the pen stays where it was");
    }
}

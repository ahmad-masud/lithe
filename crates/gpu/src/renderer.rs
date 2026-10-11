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

use crate::atlas::{Atlas, CellMetrics, Face, Size};
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
    /// 0 for the glyph atlas, 1 for an image texture.
    mode: f32,
    _pad: [f32; 3],
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
                        4 => Float32
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
            _pad: [0.0; 3],
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
                    _pad: [0.0; 3],
                });
            }
            pen += advance;
        }
        pen
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
            _pad: [0.0; 3],
        });
        r.batches.push(Batch {
            end: r.quads.len() as u32,
            image: Some(index),
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

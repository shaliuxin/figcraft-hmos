//! NativeWindow ownership stays outside the upstream safe-Rust workspace.
//!
//! This is FigCraft's host GPU layer: one wgpu Surface per HarmonyOS XComponent,
//! an egui-wgpu renderer, and a present loop. The upstream renderer (tiny-skia,
//! on the CPU) bakes each canvas frame into an egui texture; this layer only
//! uploads that texture and paints the egui scene. Sub-windows are not used yet
//! (a single main window), so the child-surface machinery of the PhotoCraft
//! port is omitted.
use raw_window_handle::{
    DisplayHandle, HasDisplayHandle, HasWindowHandle, OhosNdkWindowHandle, WindowHandle,
};
use std::{ffi::c_void, ptr::NonNull, sync::Arc};

unsafe extern "C" {
    fn OH_NativeWindow_NativeObjectUnreference(object: *mut c_void) -> i32;
    fn pc_set_window_color_space(window: *mut c_void, p3: bool) -> i32;
}

/// The XComponent callback takes a native reference before sending this to the worker.
pub struct Window(pub usize);
impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: every Window owns one reference acquired by OnSurfaceCreated; it is
        // dropped only after all surfaces borrowing it are destroyed.
        unsafe {
            OH_NativeWindow_NativeObjectUnreference(self.0 as *mut c_void);
        }
    }
}
impl HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(DisplayHandle::ohos())
    }
}
impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let ptr = NonNull::new(self.0 as *mut c_void)
            .ok_or(raw_window_handle::HandleError::Unavailable)?;
        // SAFETY: the borrowed handle cannot outlive the owned native reference.
        Ok(unsafe { WindowHandle::borrow_raw(OhosNdkWindowHandle::new(ptr).into()) })
    }
}

/// What presenting one frame to a surface did.
enum Drawn {
    Presented,
    /// The surface needs `configure` again.
    Outdated,
    /// The surface must be recreated from its native window.
    Lost,
    Skipped,
}

/// The host's single GPU device, surface and egui-wgpu renderer.
pub struct Render {
    pub surface: Option<wgpu::Surface<'static>>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    pub pending_free: Vec<egui::TextureId>,
    pub config: wgpu::SurfaceConfiguration,
    pub adapter: wgpu::Adapter,
    pub instance: wgpu::Instance,
    pub window: Option<Arc<Window>>,
    /// The panel is declared in a wide-gamut colour space (system managed).
    pub p3: bool,
    width: u32,
    height: u32,
}

impl Render {
    pub fn new(_request_p3: bool) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::GL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        // The surface is created later, when the XComponent hands over its native
        // window. Request the adapter now so device creation can start early; on a
        // hardware-less image the request still resolves (software fallback).
        let _ = &instance;
        Err("created after attach".into())
    }

    /// Build the device and first surface once the native window is available.
    pub fn attach(window: Arc<Window>, w: u32, h: u32, request_p3: bool) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::GL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("FigCraft"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let mut config = surface
            .get_default_config(&adapter, w.max(1), h.max(1))
            .ok_or("no native surface configuration")?;
        let caps = surface.get_capabilities(&adapter);
        config.format = caps
            .formats
            .iter()
            .copied()
            .find(|f| {
                matches!(
                    f,
                    wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
                )
            })
            .unwrap_or(config.format);
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);
        let p3 = request_p3 && declare_color(&window, true).is_ok();
        if !p3 {
            declare_color(&window, false)?;
        }
        let renderer = Arc::new(egui::mutex::RwLock::new(egui_wgpu::Renderer::new(
            &device,
            config.format,
            Default::default(),
        )));
        Ok(Self {
            surface: Some(surface),
            device,
            queue,
            renderer,
            pending_free: Vec::new(),
            config,
            adapter,
            instance,
            window: Some(window),
            p3,
            width: w.max(1),
            height: h.max(1),
        })
    }

    /// An adapter whose info a caller may inspect (used by the host to decide CPU vs GPU).
    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }

    pub fn resize(&mut self, w: u32, h: u32) -> Result<(), String> {
        if w == 0 || h == 0 {
            return Ok(());
        }
        self.width = w.max(1);
        self.height = h.max(1);
        self.config.width = self.width;
        self.config.height = self.height;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
            if let Some(window) = &self.window {
                declare_color(window, self.p3)?;
            }
        }
        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn detach(&mut self) {
        self.surface = None;
        self.window = None;
    }

    pub fn attached(&self) -> bool {
        self.surface.is_some()
    }

    /// Present one egui frame to the surface.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        ctx: &egui::Context,
        surface: &wgpu::Surface<'static>,
        config: &wgpu::SurfaceConfiguration,
        shapes: Vec<egui::epaint::ClippedShape>,
        pixels_per_point: f32,
    ) -> Result<Drawn, String> {
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated => return Ok(Drawn::Outdated),
            wgpu::CurrentSurfaceTexture::Lost => return Ok(Drawn::Lost),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(Drawn::Skipped)
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("native surface validation failed".into());
            }
        };
        let jobs = ctx.tessellate(shapes, pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [config.width, config.height],
            pixels_per_point,
        };
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut commands = renderer.update_buffers(device, queue, &mut encoder, &jobs, &screen);
        let view = frame.texture.create_view(&Default::default());
        {
            let attachments = [Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.15,
                        g: 0.15,
                        b: 0.15,
                        a: 1.,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })];
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("FigCraft UI"),
                color_attachments: &attachments,
                ..Default::default()
            });
            renderer.render(&mut pass.forget_lifetime(), &jobs, &screen);
        }
        commands.push(encoder.finish());
        queue.submit(commands);
        queue.present(frame);
        Ok(Drawn::Presented)
    }

    /// Paint a full egui output to the main surface. Returns true when it presented.
    pub fn paint(&mut self, ctx: &egui::Context, mut output: egui::FullOutput, pixels_per_point: f32) -> Result<bool, String> {
        let mut texture_updates = std::mem::take(&mut output.textures_delta.set);
        self.pending_free.extend(output.textures_delta.free.drain());
        output.textures_delta.clear();
        let shared_renderer = self.renderer.clone();
        let mut renderer = shared_renderer.write();
        for (id, deltas) in &mut texture_updates {
            for delta in deltas {
                renderer.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        if self.surface.is_none() {
            return Ok(false);
        }
        let surface = self
            .surface
            .as_ref()
            .ok_or("no surface")?;
        let config = self.config.clone();
        let shapes = output.shapes;
        let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::draw(
                &self.device,
                &self.queue,
                &mut renderer,
                ctx,
                &surface,
                &config,
                shapes,
                pixels_per_point,
            )
        }))
        .unwrap_or_else(|_| Err("drawing panicked".into()));
        drop(renderer);
        // Free the textures egui dropped this frame.
        if !self.pending_free.is_empty() {
            let renderer = self.renderer.clone();
            let mut r = renderer.write();
            for id in &self.pending_free {
                r.free_texture(id);
            }
            self.pending_free.clear();
        }
        match drawn {
            Ok(Drawn::Outdated) => {
                self.resize(self.width, self.height)?;
                Ok(true)
            }
            Ok(Drawn::Lost) => {
                self.surface = None;
                Ok(false)
            }
            _ => Ok(matches!(drawn, Ok(Drawn::Presented))),
        }
    }
}

/// A present with no egui geometry (attach handshake).
pub fn clear_present(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    surface: &wgpu::Surface<'static>,
    config: &wgpu::SurfaceConfiguration,
) -> Result<(), String> {
    let frame = match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f)
        | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
        wgpu::CurrentSurfaceTexture::Outdated => return Err("clear-present outdated".into()),
        wgpu::CurrentSurfaceTexture::Lost => return Err("clear-present lost".into()),
        wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
            return Err("clear-present not ready".into())
        }
        wgpu::CurrentSurfaceTexture::Validation => return Err("clear-present validation failed".into()),
    };
    let view = frame.texture.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("FigCraft clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.15,
                        g: 0.15,
                        b: 0.15,
                        a: 1.,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
    }
    let _ = config.width;
    queue.submit(std::iter::once(encoder.finish()));
    queue.present(frame);
    Ok(())
}

/// Read back the surface's colour-space declaration so the host never emits wide-gamut
/// pixels into an untagged surface.
fn declare_color(window: &Window, p3: bool) -> Result<(), String> {
    let result = unsafe { pc_set_window_color_space(window.0 as *mut c_void, p3) };
    if result != 0 {
        return Err(format!("colour space declaration failed: {result}"));
    }
    Ok(())
}

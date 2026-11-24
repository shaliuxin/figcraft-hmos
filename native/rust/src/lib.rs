//! HarmonyOS-only FFI boundary for the FigCraft editor.
//!
//! The editor and every algorithm stay safe Rust; only this module touches the
//! platform. A C++ bridge forwards an XComponent's native window, VSync and input
//! to the Rust worker, which owns the egui context, the `FigApp` and the wgpu
//! surface and presents one frame per `pc_frame`.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]
mod bridge;
mod file_open;
mod input;
mod services;
#[cfg(target_env = "ohos")]
mod render;
#[cfg(any(target_env = "ohos", test))]
mod appearance;

#[cfg(target_env = "ohos")]
use figcraft_ui_egui::FigApp;
use serde::Deserialize;
use std::{
    sync::{OnceLock, mpsc},
    time::Duration,
};
#[cfg(target_env = "ohos")]
use std::time::Instant;

/// The HarmonyOS sandbox exposes no CJK system font to scan (`fontdb` only lists
/// the macOS/Windows font dirs), so the editor's Chinese UI would render as tofu.
/// Register the bundled SimHei as a lowest-priority fallback for the proportional
/// family and the "semibold" face the theme builds for headings: Latin text still
/// uses Inter, CJK glyphs fall back to SimHei. `add_font` is idempotent — it
/// no-ops once the font is present, so calling it every frame is safe and also
/// survives the one-time `set_fonts` the app does on its first frame.
#[cfg(target_env = "ohos")]
fn ensure_cjk_font(ctx: &egui::Context) {
    use egui::{FontData, FontFamily, FontInsert, FontPriority, InsertFontFamily};
    static SIMHEI: &[u8] = include_bytes!("../assets/simhei.ttf");
    ctx.add_font(FontInsert {
        name: "SimHei".to_owned(),
        data: FontData::from_static(SIMHEI),
        families: vec![
            InsertFontFamily {
                family: FontFamily::Proportional,
                priority: FontPriority::Lowest,
            },
            InsertFontFamily {
                family: FontFamily::Name("semibold".into()),
                priority: FontPriority::Lowest,
            },
        ],
    });
}

/// The launch configuration ArkTS hands over: the sandbox folders, the device
/// scale, the language and the system colour scheme.
#[derive(Deserialize)]
#[cfg_attr(not(target_env = "ohos"), allow(dead_code))]
struct Config {
    files: String,
    #[serde(default)]
    documents: String,
    #[serde(default = "default_scale")]
    scale: f32,
    #[serde(default)]
    language: String,
    #[serde(default, rename = "displayP3")]
    display_p3: bool,
    #[serde(default, rename = "systemTheme")]
    system_theme: String,
}
fn default_scale() -> f32 {
    1.
}

/// Messages the platform hands to the worker. Input, window lifecycle and frame
/// requests share one queue so the editor's state changes only on the worker.
#[cfg_attr(not(target_env = "ohos"), allow(dead_code))]
enum Message {
    Attach(usize, u32, u32),
    Resize(u32, u32),
    Detach,
    Frame,
    Active(bool),
    Input(input::Input),
    /// A picked file's name and bytes, to open through the engine.
    Open(String, Vec<u8>),
    /// A control-channel command (`{"id":..,"params":..}`) to run on the session.
    Command(String, serde_json::Value),
    Close,
    Wake(Duration),
}
static SEND: OnceLock<mpsc::Sender<Message>> = OnceLock::new();
fn send(message: Message) {
    if let Some(tx) = SEND.get() {
        let _ = tx.send(message);
    }
}

// SAFETY: C++ passes a valid pointer/length pair for the duration of each call.
// Copy immediately; never retain a borrowed C++ buffer.
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if len > isize::MAX as usize || (ptr.is_null() && len != 0) {
        return None;
    }
    if len == 0 {
        return Some(&[]);
    }
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn copy_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(data.len())
        .map_err(|e| format!("Not enough memory to open file: {e}"))?;
    bytes.extend_from_slice(data);
    Ok(bytes)
}

/// # Safety
/// `text`/`n` must be readable and the notification function supplied to
/// `pc_init` must remain valid for the process lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_init(ptr: *const u8, len: usize, notify: bridge::Notify) -> bool {
    let Some(data) = (unsafe { bytes(ptr, len) }) else {
        return false;
    };
    let Ok(config) = serde_json::from_slice::<Config>(data) else {
        return false;
    };
    if !config.scale.is_finite() || config.scale <= 0. || config.scale > 8. {
        return false;
    }
    bridge::init(notify);
    let (tx, rx) = mpsc::channel();
    if SEND.set(tx).is_err() {
        return true;
    }
    std::thread::Builder::new()
        .name("figcraft-native".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker(config, rx))).is_err()
            {
                bridge::notify(0, "error", "Native host initialization failed", &[]);
            }
        })
        .is_ok()
}

/// # Safety
/// Pointer/length pairs must reference valid, readable memory until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_input(ptr: *const u8, len: usize) {
    if let Some(data) = unsafe { bytes(ptr, len) }
        && let Ok(value) = serde_json::from_slice(data)
    {
        send(Message::Input(value));
    }
}

/// # Safety
/// Pointer/length pairs must reference valid, readable memory until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_reply(
    id: u64,
    ok: bool,
    text: *const u8,
    text_len: usize,
    data: *const u8,
    len: usize,
) {
    let Some(text) = (unsafe { bytes(text, text_len) }) else {
        bridge::reply(id, Err("Invalid reply text buffer".into()));
        return;
    };
    let Some(data) = (unsafe { bytes(data, len) }) else {
        bridge::reply(id, Err("Invalid reply data buffer".into()));
        return;
    };
    let text = String::from_utf8_lossy(text).into_owned();
    bridge::reply(
        id,
        if ok {
            copy_bytes(data).map(|data| (text, data))
        } else {
            Err(text)
        },
    );
}

/// # Safety
/// Pointer/length pairs must reference valid, readable memory until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_open(text: *const u8, n: usize, data: *const u8, len: usize) {
    if let (Some(text), Some(data)) = (unsafe { bytes(text, n) }, unsafe { bytes(data, len) }) {
        match copy_bytes(data) {
            Ok(data) => send(Message::Open(
                String::from_utf8_lossy(text).into_owned(),
                data,
            )),
            Err(error) => bridge::notify(0, "error", &error, &[]),
        }
    }
}

/// # Safety
/// Pointer/length pairs must reference valid, readable memory until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_command(ptr: *const u8, len: usize) {
    if let Some(data) = unsafe { bytes(ptr, len) }
        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(data)
        && let Some(id) = v.get("id").and_then(|v| v.as_str())
    {
        send(Message::Command(
            id.into(),
            v.get("params").cloned().unwrap_or_default(),
        ));
    }
}

/// # Safety
/// `window` must be a live OHNativeWindow with one acquired native reference.
/// Ownership of that reference transfers to the worker and its wgpu surface;
/// the caller must not release that reference after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_attach(window: usize, w: u32, h: u32) {
    if window != 0 {
        send(Message::Attach(window, w, h));
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn pc_resize(w: u32, h: u32) {
    send(Message::Resize(w, h));
}
#[unsafe(no_mangle)]
pub extern "C" fn pc_detach() {
    bridge::cancel();
    send(Message::Detach);
}
#[unsafe(no_mangle)]
pub extern "C" fn pc_frame() {
    send(Message::Frame);
}
#[unsafe(no_mangle)]
pub extern "C" fn pc_active(active: bool) {
    send(Message::Active(active));
}
#[unsafe(no_mangle)]
pub extern "C" fn pc_close() {
    send(Message::Close);
}

// The C++ bridge reuses the PhotoCraft surface. FigCraft is a single-window host, so the
// sub-window entry points stay inert. The file-descriptor entry points, however, are the path
// HarmonyOS actually uses: a picked or dropped file arrives as an open descriptor, so these are
// wired through the engine below.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_win_attach(_key: u32, _window: usize, _w: u32, _h: u32) {}
#[unsafe(no_mangle)]
pub extern "C" fn pc_win_resize(_key: u32, _w: u32, _h: u32) {}
#[unsafe(no_mangle)]
pub extern "C" fn pc_win_detach(_key: u32) {}

/// Duplicate the platform's descriptor, then read the bytes on a side thread: the render worker
/// may be blocked on this very picker reply, so reading a large file on it would deadlock.
unsafe fn open_file(id: Option<u64>, name: String, fd: i32) {
    let file = match unsafe { file_open::duplicate(fd) } {
        Ok(file) => file,
        Err(error) => {
            finish_file(id, Err(error));
            return;
        }
    };
    if let Err(error) = std::thread::Builder::new()
        .name("figcraft-file-open".into())
        .spawn(move || finish_file(id, file_open::read(file).map(|data| (name, data))))
    {
        finish_file(id, Err(format!("Cannot start file reader: {error}")));
    }
}

/// A picked file resolves the pending picker request; a dropped/opened file (no requester) opens
/// as a new document through the same `Message::Open` path the picker uses.
fn finish_file(id: Option<u64>, result: bridge::Reply) {
    if let Some(id) = id {
        bridge::reply(id, result);
    } else {
        match result {
            Ok((name, data)) => send(Message::Open(name, data)),
            Err(error) => bridge::notify(0, "error", &error, &[]),
        }
    }
}

/// A dropped file's name and inline bytes; routed through the open path so a dropped document
/// opens like a picker pick. (Precise canvas placement at `x`/`y` is a later refinement.)
/// # Safety
/// Pointer/length pairs must reference valid, readable memory until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_drop(text: *const u8, n: usize, data: *const u8, len: usize, _x: f32, _y: f32) {
    if let (Some(text), Some(data)) = (unsafe { bytes(text, n) }, unsafe { bytes(data, len) }) {
        match copy_bytes(data) {
            Ok(data) => send(Message::Open(String::from_utf8_lossy(text).into_owned(), data)),
            Err(error) => bridge::notify(0, "error", &error, &[]),
        }
    }
}

/// A dropped file handed over as a descriptor; duplicated, then read on a side thread.
/// # Safety
/// `text`/`n` must be readable and `fd` open until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_drop_file(text: *const u8, n: usize, fd: i32, _x: f32, _y: f32) {
    let Some(text) = (unsafe { bytes(text, n) }) else {
        return;
    };
    open_file(None, String::from_utf8_lossy(text).into_owned(), fd);
}

/// # Safety
/// `text`/`n` must be readable and `fd` open until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_reply_file(id: u64, text: *const u8, n: usize, fd: i32) {
    let Some(text) = (unsafe { bytes(text, n) }) else {
        bridge::reply(id, Err("Invalid file name buffer".into()));
        return;
    };
    open_file(Some(id), String::from_utf8_lossy(text).into_owned(), fd);
}

/// # Safety
/// `text`/`n` must be readable and `fd` open until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pc_open_file(text: *const u8, n: usize, fd: i32) {
    let Some(text) = (unsafe { bytes(text, n) }) else {
        bridge::notify(0, "error", "Invalid file name buffer", &[]);
        return;
    };
    open_file(None, String::from_utf8_lossy(text).into_owned(), fd);
}

#[cfg(target_env = "ohos")]
unsafe extern "C" {
    fn pc_request_vsync();
}

#[cfg(target_env = "ohos")]
fn worker(config: Config, rx: mpsc::Receiver<Message>) {
    bridge::notify(0, "stage", "Rust worker entered", &[]);
    // Interface language and system colour scheme, applied before the first frame.
    figcraft_ui_egui::i18n::set(&config.language, Some(config.language.as_str()));

    // Start on a fresh document, not the home screen.
    let mut session = figcraft_engine::Session::empty();
    if session.docs.is_empty() {
        let _ = session.execute("file.new", &serde_json::json!({}));
    }
    let mut fig = FigApp::new(
        session,
        Box::new(services::HostServices {
            language: config.language.clone(),
            system_dark: appearance::Appearance::from_token(&config.system_theme).is_dark(),
            sandbox_dir: Some(config.files.clone()),
        }),
    );
    // No network update checks or pending updates on a mobile build.
    fig.set_startup_update_check(false);
    fig.ui.home = false;

    bridge::notify(0, "stage", "editor ready", &[]);
    let ctx = egui::Context::default();
    let mut appearance = appearance::Appearance::from_token(&config.system_theme);
    ctx.set_request_repaint_callback(|info| send(Message::Wake(info.delay)));

    let mut render: Option<render::Render> = None;
    let mut active = true;
    let mut visible = false;
    let mut deadline: Option<Instant> = None;
    // The ArkTS host fades the startup overlay only on a `first_frame` notice (Index.ets
    // `revealEditor`). Emit it exactly once, after the first real paint.
    let mut first_frame = true;
    // Forward egui's final cursor to ArkUI only when it changes (a 2-in-1 pointer device
    // shows the shape the tool under the pointer needs).
    let mut cursor_sent: Option<egui::CursorIcon> = None;
    let started = Instant::now();
    let mut events = Vec::new();
    let mut input = input::State::default();

    loop {
        let wait = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        let message = match wait {
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(m) => m,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    deadline = None;
                    if active && visible {
                        unsafe {
                            pc_request_vsync();
                        }
                    }
                    continue;
                }
                Err(_) => break,
            },
            None => match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            },
        };
        let rendering_operation = matches!(
            &message,
            Message::Attach(..) | Message::Resize(..) | Message::Detach | Message::Frame
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<(), String> {
                match message {
                    Message::Attach(ptr, w, h) => {
                        bridge::notify(0, "stage", "creating native GPU surface", &[]);
                        visible = w > 0 && h > 0;
                        render = Some(render::Render::attach(
                            std::sync::Arc::new(render::Window(ptr)),
                            w,
                            h,
                            config.display_p3,
                        )?);
                        bridge::resume();
                        deadline = Some(Instant::now());
                        bridge::notify(0, "ready", "Native egui / wgpu GLES", &[]);
                    }
                    Message::Resize(w, h) => {
                        visible = w > 0 && h > 0;
                        if let Some(r) = render.as_mut() {
                            r.resize(w, h)?;
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::Detach => {
                        visible = false;
                        deadline = None;
                        if let Some(r) = render.as_mut() {
                            r.detach();
                        }
                        input.cancel(&mut events);
                    }
                    Message::Active(value) => {
                        active = value;
                        if active {
                            bridge::resume();
                            deadline = Some(Instant::now());
                        } else {
                            deadline = None;
                            input.cancel(&mut events);
                        }
                    }
                    Message::Input(value) => {
                        if value.kind == "system_theme" {
                            appearance = appearance::Appearance::from_token(&value.text);
                        } else {
                            input.apply(&value, config.scale, &mut events);
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::Open(name, data) => {
                        // A sandboxed app has no paths: write the picked bytes into the
                        // sandbox and let the engine open them by path.
                        let path = std::path::Path::new(&config.files).join(&name);
                        if let Some(dir) = path.parent() {
                            let _ = std::fs::create_dir_all(dir);
                        }
                        std::fs::write(&path, &data).map_err(|e| format!("write {name}: {e}"))?;
                        let _ = fig
                            .session
                            .execute("file.open", &serde_json::json!({ "path": path.to_string_lossy() }));
                        deadline = Some(Instant::now());
                    }
                    Message::Command(id, params) => {
                        let _ = fig.session.execute(&id, &params);
                        deadline = Some(Instant::now());
                    }
                    Message::Close => {
                        // A teardown request (system back button or egui's Close): let the host
                        // decide whether to keep the document (unsaved changes) or tear down.
                        bridge::resume();
                        bridge::notify(0, "close", "", &[]);
                        deadline = None;
                    }
                    Message::Frame => {
                        // Nothing attached yet: skip until the XComponent hands over a surface.
                        if render.is_none() {
                            return Ok(());
                        }
                        let (w, h) = render
                            .as_ref()
                            .map(|r| (r.width(), r.height()))
                            .unwrap_or((1, 1));
                        // The XComponent hands us physical pixels, but egui lays out and
                        // hit-tests in logical points. Divide by the device pixel ratio (and
                        // any UI zoom) so the viewport size and the (already scaled) pointer
                        // events live in the same coordinate space; otherwise a tap at pixel
                        // p lands at p·scale points — a severe, uniform offset.
                        let ppp = config.scale * ctx.zoom_factor();
                        let size = egui::vec2(w as f32 / ppp, h as f32 / ppp);
                        let mut raw = egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), size)),
                            time: Some(started.elapsed().as_secs_f64()),
                            events: std::mem::take(&mut events),
                            system_theme: appearance.current(),
                            ..Default::default()
                        };
                        let viewport = raw.viewports.entry(egui::ViewportId::ROOT).or_default();
                        viewport.native_pixels_per_point = Some(config.scale);
                        viewport.inner_rect = raw.screen_rect;
                        // Drive one egui frame, then paint whatever the scene produced.
                        let output = ctx.run_ui(raw, |ui| fig.ui(ui));
                        // Collect external-link requests the UI raised this frame (help links,
                        // "open in browser") and the final cursor icon, before painting consumes
                        // the output; the host acts on them on its side.
                        let open_urls: Vec<String> = output
                            .platform_output
                            .commands
                            .iter()
                            .filter_map(|command| match command {
                                egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
                                _ => None,
                            })
                            .collect();
                        let cursor = output.platform_output.cursor_icon;
                        // Install the bundled CJK face after the app's first-frame `set_fonts`
                        // so it is not overwritten; it sticks from the next frame on.
                        ensure_cjk_font(&ctx);
                        if let Some(r) = render.as_mut() {
                            r.paint(&ctx, output, ppp)?;
                        }
                        for url in open_urls {
                            bridge::notify(0, "url", &url, &[]);
                        }
                        // Forward the cursor only when it actually changes.
                        if cursor_sent != Some(cursor) {
                            cursor_sent = Some(cursor);
                            bridge::notify(0, "cursor", &format!("{cursor:?}"), &[]);
                        }
                        // The first real frame is the signal the ArkTS host waits for to fade
                        // the startup overlay and reveal the live editor.
                        if first_frame {
                            first_frame = false;
                            bridge::notify(0, "first_frame", "Native UI presented", &[]);
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::Wake(delay) => {
                        if active && visible {
                            if let Some(next) = Instant::now().checked_add(delay) {
                                deadline = Some(deadline.map_or(next, |d| d.min(next)));
                            }
                        }
                    }
                }
                Ok(())
            },
        ));
        let failure = match result {
            Ok(Ok(())) => None,
            Ok(Err(error)) if rendering_operation => Some(error),
            Ok(Err(error)) => {
                bridge::notify(0, "error", &error, &[]);
                None
            }
            Err(_) => Some("Native rendering panic; GPU drawing stopped".into()),
        };
        if let Some(reason) = failure {
            bridge::notify(
                0,
                "diagnostic",
                &format!("Native rendering stopped: {reason}"),
                &[],
            );
            render = None;
            deadline = None;
        }
    }
}

#[cfg(not(target_env = "ohos"))]
fn worker(_config: Config, _rx: mpsc::Receiver<Message>) {
    bridge::notify(0, "error", "Native host requires an OHOS target", &[]);
}

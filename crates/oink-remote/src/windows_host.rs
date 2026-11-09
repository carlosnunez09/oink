//! Windows screen capture and input injection.
//!
//! Capture uses Desktop Duplication, which waits for the screen to change and
//! costs little; if it cannot start (some remote or virtual displays), a GDI
//! screen copy is used. Input goes through `SendInput`.
//!
//! Limits of running as an ordinary program: the lock screen, the sign-in
//! screen and UAC prompts are on a separate desktop that can be neither
//! captured nor controlled, and windows of programs run as administrator
//! ignore the injected input unless the host itself runs as administrator.

use crate::{
    codec::Frame,
    host::{Capturer, DesktopRect, Injector, Platform},
    proto::Button,
};
use std::{io, thread, time::Duration};
use windows::{
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::{
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
                ID3D11DeviceContext, ID3D11Texture2D,
            },
            Dxgi::{
                CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIFactory1,
                IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
            },
            Gdi::{
                BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
                DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject,
            },
        },
        System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState},
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
                KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
                MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
                MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
                MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
            },
            WindowsAndMessaging::{
                GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
                SM_YVIRTUALSCREEN,
            },
        },
    },
    core::Interface,
};

fn other(error: windows::core::Error) -> io::Error {
    io::Error::other(error.to_string())
}

/// Call once at start, before anything asks for screen sizes: without it
/// Windows reports scaled sizes and positions on high-DPI displays.
pub fn init_process() {
    // SAFETY: plain call. It fails harmlessly if the awareness was already set.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Keep the computer from going to sleep for as long as the calling thread
/// lives, so it stays reachable while hosting.
pub fn stay_awake_on_this_thread() {
    // SAFETY: plain call; the state belongs to the calling thread.
    unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
}

/// Keeps the display on while a viewer is connected.
#[derive(Debug)]
struct DisplayOn;

impl DisplayOn {
    fn new() -> Self {
        // SAFETY: plain call; the state belongs to the calling thread, which also drops this.
        unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED) };
        Self
    }
}

impl Drop for DisplayOn {
    fn drop(&mut self) {
        // SAFETY: plain call on the thread that set the state.
        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    }
}

struct Monitor {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
    rect: DesktopRect,
}

fn rect_of(rect: RECT) -> DesktopRect {
    DesktopRect {
        left: rect.left,
        top: rect.top,
        width: (rect.right - rect.left).max(0) as u32,
        height: (rect.bottom - rect.top).max(0) as u32,
    }
}

/// The monitors that are part of the desktop, the primary one (at 0,0) first.
fn monitors() -> windows::core::Result<Vec<Monitor>> {
    // SAFETY: plain factory call.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut found = Vec::new();
    // SAFETY: enumeration calls on live interfaces; each ends with an error at the end of its list.
    unsafe {
        let mut adapter_index = 0;
        while let Ok(adapter) = factory.EnumAdapters1(adapter_index) {
            adapter_index += 1;
            let mut output_index = 0;
            while let Ok(output) = adapter.EnumOutputs(output_index) {
                output_index += 1;
                let Ok(description) = output.GetDesc() else { continue };
                if description.AttachedToDesktop.as_bool() {
                    found.push(Monitor {
                        adapter: adapter.clone(),
                        output,
                        rect: rect_of(description.DesktopCoordinates),
                    });
                }
            }
        }
    }
    found.sort_by_key(|monitor| (monitor.rect.left, monitor.rect.top) != (0, 0));
    Ok(found)
}

fn primary_rect() -> DesktopRect {
    // SAFETY: plain calls.
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    DesktopRect {
        left: 0,
        top: 0,
        width: width.max(1) as u32,
        height: height.max(1) as u32,
    }
}

/// The place of the monitor with this number (the primary one if there is no
/// such monitor) and the number of monitors.
fn monitor_rect(monitor: u8) -> (DesktopRect, u8, u8) {
    match monitors() {
        Ok(list) if !list.is_empty() => {
            let index = if (monitor as usize) < list.len() {
                monitor as usize
            } else {
                0
            };
            (list[index].rect, index as u8, list.len().min(255) as u8)
        }
        _ => (primary_rect(), 0, 1),
    }
}

struct Duplication {
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    duplication: IDXGIOutputDuplication,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    rect: DesktopRect,
    index: u8,
    count: u8,
}

impl Duplication {
    fn open(monitor: u8) -> windows::core::Result<Self> {
        let mut list = monitors()?;
        if list.is_empty() {
            return Err(windows::core::Error::empty());
        }
        let index = if (monitor as usize) < list.len() {
            monitor as usize
        } else {
            0
        };
        let count = list.len().min(255) as u8;
        let Monitor { adapter, output, rect } = list.swap_remove(index);
        let mut device = None;
        let mut context = None;
        // SAFETY: the out pointers are valid for the call; the adapter is live.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let (Some(device), Some(context)) = (device, context) else {
            return Err(windows::core::Error::empty());
        };
        // SAFETY: the output and the device are live and belong to the same adapter.
        let duplication = unsafe { output.cast::<IDXGIOutput1>()?.DuplicateOutput(&device)? };
        Ok(Self {
            context,
            device,
            duplication,
            staging: None,
            rect,
            index: index as u8,
            count,
        })
    }

    /// `Ok(false)`: nothing new within the time.
    fn next(&mut self, timeout: Duration, first: bool, frame: &mut Frame) -> windows::core::Result<bool> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: the out pointers are valid for the call.
        match unsafe {
            self.duplication
                .AcquireNextFrame(timeout.as_millis() as u32, &mut info, &mut resource)
        } {
            Ok(()) => {}
            Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(false),
            Err(error) => return Err(error),
        }
        // A frame with no present time carries only a pointer movement.
        let copied = if info.LastPresentTime == 0 && !first {
            Ok(false)
        } else {
            self.copy(resource, frame).map(|()| true)
        };
        // SAFETY: a frame was acquired above and is released exactly once.
        unsafe { self.duplication.ReleaseFrame()? };
        copied
    }

    fn copy(&mut self, resource: Option<IDXGIResource>, frame: &mut Frame) -> windows::core::Result<()> {
        let texture: ID3D11Texture2D = resource.ok_or_else(windows::core::Error::empty)?.cast()?;
        let mut description = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: the out pointer is valid for the call.
        unsafe { texture.GetDesc(&mut description) };
        let (width, height) = (description.Width, description.Height);
        if !matches!(&self.staging, Some((_, w, h)) if (*w, *h) == (width, height)) {
            let staging_description = D3D11_TEXTURE2D_DESC {
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
                ..description
            };
            let mut staging = None;
            // SAFETY: the description and the out pointer are valid for the call.
            unsafe {
                self.device
                    .CreateTexture2D(&staging_description, None, Some(&mut staging))?
            };
            self.staging = Some((staging.ok_or_else(windows::core::Error::empty)?, width, height));
        }
        let Some((staging, _, _)) = &self.staging else {
            return Err(windows::core::Error::empty());
        };
        frame.resize(width, height);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures are live and of the same size and format. While
        // mapped, `pData` points to `height` rows of `RowPitch` bytes, each at
        // least `width * 4` long (the desktop format is 32-bit BGRA); the rows
        // are read before `Unmap`.
        unsafe {
            self.context.CopyResource(staging, &texture);
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let row_bytes = width as usize * 4;
            for (y, row) in frame.data.chunks_exact_mut(row_bytes).enumerate() {
                let source = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
                std::ptr::copy_nonoverlapping(source, row.as_mut_ptr(), row_bytes);
            }
            self.context.Unmap(staging, 0);
        }
        Ok(())
    }
}

/// Desktop Duplication capture. The duplication is lost whenever the display
/// mode changes or the secure desktop appears; it is then opened again.
struct DxgiCapturer {
    monitor: u8,
    duplication: Option<Duplication>,
    rect: DesktopRect,
    index: u8,
    count: u8,
    first: bool,
    _display: DisplayOn,
}

impl DxgiCapturer {
    fn open(monitor: u8) -> windows::core::Result<Self> {
        let duplication = Duplication::open(monitor)?;
        let (rect, index, count) = (duplication.rect, duplication.index, duplication.count);
        Ok(Self {
            monitor,
            duplication: Some(duplication),
            rect,
            index,
            count,
            first: true,
            _display: DisplayOn::new(),
        })
    }
}

impl Capturer for DxgiCapturer {
    fn capture(&mut self, timeout: Duration, frame: &mut Frame) -> io::Result<bool> {
        let Some(duplication) = &mut self.duplication else {
            match Duplication::open(self.monitor) {
                Ok(duplication) => {
                    (self.rect, self.index, self.count) = (duplication.rect, duplication.index, duplication.count);
                    self.duplication = Some(duplication);
                    self.first = true;
                }
                // Not available right now (lock screen, UAC prompt, mode change in progress).
                Err(_) => thread::sleep(timeout),
            }
            return Ok(false);
        };
        match duplication.next(timeout, self.first, frame) {
            Ok(changed) => {
                self.first &= !changed;
                Ok(changed)
            }
            Err(_) => {
                self.duplication = None;
                Ok(false)
            }
        }
    }
    fn desktop_rect(&self) -> DesktopRect {
        self.rect
    }
    fn monitor(&self) -> (u8, u8) {
        (self.index, self.count)
    }
}

/// A 32-bit top-down bitmap selected into a memory device context.
struct Canvas {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *const u8,
    rect: DesktopRect,
}

impl Canvas {
    fn new(rect: DesktopRect) -> io::Result<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: rect.width as i32,
                // Negative: rows run top to bottom.
                biHeight: -(rect.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: plain GDI calls with valid arguments; every object created
        // here is released in `drop`, or right here if a later step fails.
        unsafe {
            let screen = GetDC(None);
            if screen.is_invalid() {
                return Err(io::Error::other("no screen device context"));
            }
            let memory = CreateCompatibleDC(Some(screen));
            let mut bits = std::ptr::null_mut();
            match CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(bitmap) if !memory.is_invalid() && !bits.is_null() => {
                    let previous = SelectObject(memory, bitmap.into());
                    Ok(Self {
                        screen,
                        memory,
                        bitmap,
                        previous,
                        bits: bits as *const u8,
                        rect,
                    })
                }
                created => {
                    if let Ok(bitmap) = created {
                        let _ = DeleteObject(bitmap.into());
                    }
                    let _ = DeleteDC(memory);
                    ReleaseDC(None, screen);
                    Err(io::Error::other("cannot create the capture bitmap"))
                }
            }
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: the objects were created in `new` and are released once.
        unsafe {
            SelectObject(self.memory, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.memory);
            ReleaseDC(None, self.screen);
        }
    }
}

/// Screen copy with GDI: works nearly everywhere, but copies the whole screen
/// every time whether it changed or not.
struct GdiCapturer {
    monitor: u8,
    canvas: Canvas,
    index: u8,
    count: u8,
    captures: u32,
    _display: DisplayOn,
}

impl GdiCapturer {
    fn open(monitor: u8) -> io::Result<Self> {
        let (rect, index, count) = monitor_rect(monitor);
        Ok(Self {
            monitor,
            canvas: Canvas::new(rect)?,
            index,
            count,
            captures: 0,
            _display: DisplayOn::new(),
        })
    }
}

impl Capturer for GdiCapturer {
    fn capture(&mut self, timeout: Duration, frame: &mut Frame) -> io::Result<bool> {
        // Look for a changed display mode about once a second.
        self.captures += 1;
        if self.captures.is_multiple_of(32) {
            let (rect, index, count) = monitor_rect(self.monitor);
            (self.index, self.count) = (index, count);
            if rect != self.canvas.rect {
                self.canvas = Canvas::new(rect)?;
            }
        }
        let rect = self.canvas.rect;
        // SAFETY: both device contexts are live and the bitmap has the size of `rect`.
        let copied = unsafe {
            BitBlt(
                self.canvas.memory,
                0,
                0,
                rect.width as i32,
                rect.height as i32,
                Some(self.canvas.screen),
                rect.left,
                rect.top,
                SRCCOPY | CAPTUREBLT,
            )
        };
        if copied.is_err() {
            // The lock screen or a UAC prompt is showing.
            thread::sleep(timeout);
            return Ok(false);
        }
        frame.resize(rect.width, rect.height);
        // SAFETY: the bitmap holds `width * height` 32-bit pixels at `bits`, and
        // GDI has finished drawing into it when `BitBlt` returns.
        let pixels = unsafe { std::slice::from_raw_parts(self.canvas.bits, frame.data.len()) };
        frame.data.copy_from_slice(pixels);
        Ok(true)
    }
    fn desktop_rect(&self) -> DesktopRect {
        self.canvas.rect
    }
    fn monitor(&self) -> (u8, u8) {
        (self.index, self.count)
    }
}

#[derive(Debug)]
struct SendInputInjector;

fn send(input: INPUT) {
    // SAFETY: one fully initialised INPUT of the stated size.
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}

fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) {
    send(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    });
}

impl Injector for SendInputInjector {
    fn mouse_move(&mut self, x: i32, y: i32) {
        // SAFETY: plain calls.
        let (left, top, width, height) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        // Absolute positions run 0..=65535 across the whole desktop.
        let along = |position: i32, start: i32, size: i32| {
            ((position - start) as i64 * 65535 / (size - 1).max(1) as i64).clamp(0, 65535) as i32
        };
        mouse(
            along(x, left, width),
            along(y, top, height),
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        );
    }

    fn button(&mut self, button: Button, down: bool) {
        let (flags, data) = match (button, down) {
            (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            // XBUTTON1 is 1 (back), XBUTTON2 is 2 (forward).
            (Button::Back, true) => (MOUSEEVENTF_XDOWN, 1),
            (Button::Back, false) => (MOUSEEVENTF_XUP, 1),
            (Button::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
            (Button::Forward, false) => (MOUSEEVENTF_XUP, 2),
        };
        mouse(0, 0, data, flags);
    }

    fn wheel(&mut self, dx: i16, dy: i16) {
        if dy != 0 {
            mouse(0, 0, dy as i32, MOUSEEVENTF_WHEEL);
        }
        if dx != 0 {
            mouse(0, 0, dx as i32, MOUSEEVENTF_HWHEEL);
        }
    }

    fn key(&mut self, scancode: u16, down: bool) {
        let mut flags = KEYEVENTF_SCANCODE;
        if scancode & 0xFF00 == crate::keymap::EXTENDED {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        let flags: KEYBD_EVENT_FLAGS = flags;
        send(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: scancode & 0xFF,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
}

/// The real computer. `gdi_only` skips Desktop Duplication.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsPlatform {
    pub gdi_only: bool,
}

impl Platform for WindowsPlatform {
    fn capturer(&self, monitor: u8) -> io::Result<Box<dyn Capturer>> {
        if !self.gdi_only {
            match DxgiCapturer::open(monitor) {
                Ok(capturer) => return Ok(Box::new(capturer)),
                Err(error) => eprintln!(
                    "Desktop Duplication is not available ({}); using GDI capture",
                    other(error)
                ),
            }
        }
        Ok(Box::new(GdiCapturer::open(monitor)?))
    }

    fn injector(&self) -> io::Result<Box<dyn Injector>> {
        Ok(Box::new(SendInputInjector))
    }
}

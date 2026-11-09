//! The viewer window: shows the remote screen scaled to fit and sends the
//! keyboard and mouse to it while the window has focus.
//!
//! Control-Option-Command-F (Ctrl-Alt-Win-F on Windows) switches full screen
//! and is not sent to the remote computer.

use crate::{
    auth::AccessKey,
    client::{Client, Event as ClientEvent, Options},
    keymap,
    proto::{Button, ClientMsg},
    scale,
};
use std::{num::NonZeroU32, rc::Rc, sync::Arc};
use tao::{
    dpi::LogicalSize,
    event::{ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    keyboard::{KeyCode, ModifiersState},
    window::{Fullscreen, WindowBuilder},
};

#[derive(Clone, Debug)]
pub struct ViewerOptions {
    /// Shown in the window title.
    pub host: String,
    pub connection: Options,
    /// Left Command acts as Ctrl, right Command as the Windows key.
    pub mac_layout: bool,
}

/// Wheel units for one line of a mouse wheel and for one pixel of a trackpad.
const WHEEL_LINE: f64 = 120.0;
const WHEEL_PIXEL: f64 = 3.0;

struct Input {
    client: Arc<Client>,
    mac_layout: bool,
    modifiers: ModifiersState,
    /// Scan codes and buttons sent as pressed and not yet released.
    keys: Vec<u16>,
    buttons: Vec<Button>,
    /// Trackpad movement smaller than one wheel unit, carried to the next event.
    wheel: (f64, f64),
}

impl Input {
    fn send(&self, message: ClientMsg) {
        // A failed send means the connection ended; the receiving thread reports that.
        let _ = self.client.send(message);
    }

    fn key(&mut self, key: KeyCode, down: bool) {
        let Some(scancode) = keymap::scancode(key, self.mac_layout) else {
            return;
        };
        if down {
            if !self.keys.contains(&scancode) {
                self.keys.push(scancode);
            }
        } else if let Some(index) = self.keys.iter().position(|held| *held == scancode) {
            self.keys.swap_remove(index);
        } else {
            // Pressed before the window had focus, or already released with the rest.
            return;
        }
        self.send(ClientMsg::Key { scancode, down });
        // macOS does not always report the release of a key that was pressed
        // together with Command. When Command goes up, release those keys too.
        if !down && matches!(key, KeyCode::SuperLeft | KeyCode::SuperRight) {
            let (modifiers, others): (Vec<u16>, Vec<u16>) =
                self.keys.iter().partition(|held| keymap::is_modifier(**held));
            self.keys = modifiers;
            for scancode in others {
                self.send(ClientMsg::Key { scancode, down: false });
            }
        }
    }

    fn button(&mut self, button: Button, down: bool) {
        let held = self.buttons.iter().position(|held| *held == button);
        match (down, held) {
            (true, None) => self.buttons.push(button),
            (false, Some(index)) => {
                self.buttons.swap_remove(index);
            }
            _ => return,
        }
        self.send(ClientMsg::MouseButton { button, down });
    }

    fn scroll(&mut self, delta: MouseScrollDelta) {
        let (x, y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x as f64 * WHEEL_LINE, y as f64 * WHEEL_LINE),
            MouseScrollDelta::PixelDelta(position) => (position.x * WHEEL_PIXEL, position.y * WHEEL_PIXEL),
            _ => return,
        };
        // The window reports "content moves right"; a wheel reports "scroll right".
        self.wheel = (self.wheel.0 - x, self.wheel.1 + y);
        let whole = (
            self.wheel.0.trunc().clamp(-32000.0, 32000.0),
            self.wheel.1.trunc().clamp(-32000.0, 32000.0),
        );
        if whole != (0.0, 0.0) {
            self.wheel = (self.wheel.0 - whole.0, self.wheel.1 - whole.1);
            self.send(ClientMsg::Wheel {
                dx: whole.0 as i16,
                dy: whole.1 as i16,
            });
        }
    }

    fn release_all(&mut self) {
        if !self.keys.is_empty() || !self.buttons.is_empty() {
            self.keys.clear();
            self.buttons.clear();
            self.send(ClientMsg::ReleaseAll);
        }
    }
}

/// Connect and show the remote screen until the window is closed or the
/// connection ends. `connected` runs once the computer has accepted the key.
/// Must be called on the main thread; returns only if connecting fails.
pub fn run(
    address: (String, u16),
    key: AccessKey,
    options: ViewerOptions,
    connected: impl FnOnce(),
) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoopBuilder::<ClientEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let client = Client::connect(
        (address.0.as_str(), address.1),
        &key,
        options.connection,
        move |event| {
            let _ = proxy.send_event(event);
        },
    )?;
    connected();
    let title = format!("{} - Oink Remote", options.host);
    let window = Rc::new(
        WindowBuilder::new()
            .with_title(&title)
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .build(&event_loop)?,
    );
    let context = softbuffer::Context::new(window.clone())?;
    let mut surface = softbuffer::Surface::new(&context, window.clone())?;
    let mut input = Input {
        client,
        mac_layout: options.mac_layout,
        modifiers: ModifiersState::empty(),
        keys: Vec::new(),
        buttons: Vec::new(),
        wheel: (0.0, 0.0),
    };
    let mut sized_to_picture = false;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(ClientEvent::Updated) => {
                if !sized_to_picture {
                    // Start at the remote screen's proportions, no larger than this display.
                    let (width, height) = {
                        let picture = input.client.picture().lock().unwrap();
                        (picture.width as f64, picture.height as f64)
                    };
                    if width > 0.0 {
                        sized_to_picture = true;
                        let available = window.current_monitor().map(|monitor| monitor.size());
                        let (max_w, max_h) = available.map_or((1600.0, 1000.0), |size| {
                            (size.width as f64 * 0.9, size.height as f64 * 0.85)
                        });
                        let shrink = (max_w / width).min(max_h / height).min(1.0);
                        window.set_inner_size(tao::dpi::PhysicalSize::new(width * shrink, height * shrink));
                    }
                }
                window.request_redraw();
            }
            Event::UserEvent(ClientEvent::Closed(reason)) => {
                if !reason.is_empty() {
                    eprintln!("Disconnected: {reason}");
                }
                *control_flow = ControlFlow::Exit;
            }
            Event::RedrawRequested(_) => {
                let size = window.inner_size();
                let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
                    return;
                };
                if surface.resize(width, height).is_err() {
                    return;
                }
                let Ok(mut buffer) = surface.buffer_mut() else { return };
                {
                    let picture = input.client.picture().lock().unwrap();
                    scale::blit(
                        &picture.pixels,
                        picture.width,
                        picture.height,
                        &mut buffer,
                        size.width as usize,
                        size.height as usize,
                    );
                }
                let _ = buffer.present();
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    input.release_all();
                    input.client.close();
                    *control_flow = ControlFlow::Exit;
                }
                WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => window.request_redraw(),
                WindowEvent::Focused(false) => input.release_all(),
                WindowEvent::ModifiersChanged(modifiers) => input.modifiers = modifiers,
                WindowEvent::KeyboardInput { event: key, .. } => {
                    let down = key.state == ElementState::Pressed;
                    let all = ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER;
                    if down && key.physical_key == KeyCode::KeyF && input.modifiers.contains(all) {
                        if !key.repeat {
                            input.release_all();
                            let next = window.fullscreen().is_none().then_some(Fullscreen::Borderless(None));
                            window.set_fullscreen(next);
                        }
                        return;
                    }
                    input.key(key.physical_key, down);
                }
                WindowEvent::CursorMoved { position, .. } => {
                    let size = window.inner_size();
                    let (width, height) = {
                        let picture = input.client.picture().lock().unwrap();
                        (picture.width, picture.height)
                    };
                    let place = scale::locate(
                        position.x,
                        position.y,
                        width,
                        height,
                        size.width as usize,
                        size.height as usize,
                    );
                    if let Some((x, y)) = place {
                        input.send(ClientMsg::MouseMove { x, y });
                    }
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    let button = match button {
                        MouseButton::Left => Button::Left,
                        MouseButton::Right => Button::Right,
                        MouseButton::Middle => Button::Middle,
                        MouseButton::Other(3) => Button::Back,
                        MouseButton::Other(4) => Button::Forward,
                        _ => return,
                    };
                    input.button(button, state == ElementState::Pressed);
                }
                WindowEvent::MouseWheel { delta, .. } => input.scroll(delta),
                _ => {}
            },
            _ => {}
        }
    })
}

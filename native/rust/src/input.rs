//! Host input: translate the C++ bridge's JSON input protocol into egui events.
//!
//! The C++ bridge emits one JSON object per native input sample. This parser keeps
//! the same wire format as the PhotoCraft port (pointer / mouse / key / axis / blur /
//! hover_leave) and produces `egui::Event`s that FigCraft's egui scene consumes.
//! Pen pressure/tilt and multi-touch pinch navigation are deferred to a later patch;
//! single-pointer interaction (the common path) is supported now.
use egui::{
    Event, Key, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, TouchPhase,
};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize, Default)]
pub struct Contact {
    pub id: u64,
    pub x: f32,
    pub y: f32,
}

#[derive(Deserialize, Default)]
pub struct Input {
    pub kind: String,
    #[serde(default)]
    pub win: u32,
    #[serde(default)]
    pub points: Vec<Contact>,
    #[serde(default)]
    pub zoom: f32,
    #[serde(default)]
    pub dx: f32,
    #[serde(default)]
    pub dy: f32,
    #[serde(default)]
    pub action: i32,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default)]
    pub button: i32,
    #[serde(default)]
    pub code: i32,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub rotation: u8,
    #[serde(default)]
    pub landscape: bool,
    #[serde(default)]
    pub focus: String,
    #[serde(default)]
    pub count: usize,
    #[serde(default)]
    pub start: usize,
    #[serde(default)]
    pub end: usize,
}

#[derive(Default)]
pub struct State {
    pressed: Option<PointerButton>,
    last_pointer: Option<Pos2>,
    touches: BTreeMap<u64, Pos2>,
    touch_pointer: Option<u64>,
    pub modifiers: Modifiers,
}

impl State {
    fn move_pointer(&mut self, pos: Pos2, events: &mut Vec<Event>) {
        self.last_pointer = Some(pos);
        events.push(Event::PointerMoved(pos));
    }

    /// A touch snapshot: map each active contact into an egui event.
    fn touch(&mut self, input: &Input, scale: f32, events: &mut Vec<Event>) {
        // The XComponent reports physical pixels; egui hit-tests in logical points, so
        // divide by the device pixel ratio. A zero/negative/inf scale would blow up the
        // coordinates, so fall back to 1:1 (pixel space) in that case.
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        // OHOS assigns the first (and often only) finger the contact id 0, so 0 is a
        // real identifier here, not a sentinel — a "no touch" sample is an empty points
        // array instead. Drop only contacts whose position is not finite.
        let next: BTreeMap<u64, Pos2> = input
            .points
            .iter()
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .map(|p| (p.id, Pos2::new(p.x / scale, p.y / scale)))
            .collect();
        // Release contacts that left the snapshot: a lifted finger must emit the matching
        // pointer release, or egui sees a press with no release and the click never completes.
        for id in self.touches.keys().copied().collect::<Vec<_>>() {
            if !next.contains_key(&id) {
                if let Some(pos) = self.touches.remove(&id) {
                    events.push(Event::PointerButton {
                        pos,
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Modifiers::NONE,
                    });
                }
            }
        }
        for (id, pos) in &next {
            if !self.touches.contains_key(id) {
                events.push(Event::PointerButton {
                    pos: *pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    // A touch has no button index; mark it as a generic press.
                    modifiers: Modifiers::NONE,
                });
                self.touches.insert(*id, *pos);
            } else {
                if let Some(old) = self.touches.get(id) {
                    if old != pos {
                        events.push(Event::PointerMoved(*pos));
                        self.touches.insert(*id, *pos);
                    }
                }
            }
        }
        if self.touches.is_empty() {
            self.touch_pointer = None;
        } else {
            self.touch_pointer = self.touches.keys().next().copied();
        }
    }

    /// A mouse or single-pointer sample.
    fn pointer(&mut self, input: &Input, scale: f32, events: &mut Vec<Event>) {
        // Physical pixels → logical points, same as touch.
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let pos = Pos2::new(input.x / scale, input.y / scale);
        let action = input.action;
        // 0 down, 1 up, 2 move, 3 cancel (matches the C++ bridge).
        if action == 2 {
            self.move_pointer(pos, events);
            return;
        }
        if action == 3 {
            self.pressed = None;
            events.push(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
            return;
        }
        let button = if input.button == 1 {
            PointerButton::Secondary
        } else if input.button == 2 {
            PointerButton::Middle
        } else {
            PointerButton::Primary
        };
        let pressed = action == 0;
        self.pressed = if pressed { Some(button) } else { None };
        self.move_pointer(pos, events);
        events.push(Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }

    fn key(&mut self, input: &Input, events: &mut Vec<Event>) {
        let pressed = input.action == 0;
        // On a non-Mac host the "command" slot mirrors the control key.
        let mods = Modifiers {
            ctrl: input.ctrl,
            shift: input.shift,
            alt: input.alt,
            mac_cmd: false,
            command: input.ctrl,
        };
        self.modifiers = mods;
        if let Some(name) = key_name(input.code) {
            events.push(Event::Key {
                key: name,
                physical_key: None,
                pressed,
                modifiers: mods,
                repeat: false,
            });
        }
        // Text input (a typed character) is emitted as a Text event, separately
        // from the modifier/key-down it accompanies.
        if pressed && !input.text.is_empty() && !input.ctrl && !input.alt {
            events.push(Event::Text(input.text.clone()));
        }
    }

    /// Wheel / axis sample. egui 0.37 reports scrolling through `MouseWheel`.
    fn axis(&mut self, input: &Input, scale: f32, events: &mut Vec<Event>) {
        // Scroll deltas are pixel distances; convert to points. The pinch `zoom` is a
        // cumulative ratio, not a distance, so it is left untouched.
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let zoom = if input.zoom.abs() > 0.0001 {
            input.zoom - 1.0
        } else {
            0.0
        };
        if input.dx != 0.0 || input.dy != 0.0 || zoom != 0.0 {
            events.push(Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(input.dx / scale, input.dy / scale) + egui::vec2(zoom, zoom) * 12.0,
                phase: TouchPhase::Move,
                source: MouseWheelSource::Unknown,
                modifiers: self.modifiers,
            });
        }
    }
}

/// The bridge sends a small key-name subset; unknown codes are ignored.
fn key_name(code: i32) -> Option<Key> {
    use egui::Key::*;
    // OH key codes used by the C++ bridge.
    let key = match code {
        2000 => Num0,
        2001 => Num1,
        2002 => Num2,
        2003 => Num3,
        2004 => Num4,
        2005 => Num5,
        2006 => Num6,
        2007 => Num7,
        2008 => Num8,
        2009 => Num9,
        2017 => A,
        2018 => B,
        2019 => C,
        2020 => D,
        2021 => E,
        2022 => F,
        2023 => G,
        2024 => H,
        2025 => I,
        2026 => J,
        2027 => K,
        2028 => L,
        2029 => M,
        2030 => N,
        2031 => O,
        2032 => P,
        2033 => Q,
        2034 => R,
        2035 => S,
        2036 => T,
        2037 => U,
        2038 => V,
        2039 => W,
        2040 => X,
        2041 => Y,
        2042 => Z,
        2043 => Comma,
        2044 => Minus,
        2050 => Space,
        2056 => Period,
        2057 => Slash,
        2058 => OpenBracket,
        2059 => CloseBracket,
        2060 => Semicolon,
        2061 => Quote,
        2062 => Backslash,
        2063 => Backtick,
        2064 => OpenBracket,
        2016 => Escape,
        67 => Backspace,
        68 => Enter,
        71 => Tab,
        70 => Delete,
        105 => ArrowLeft,
        106 => ArrowUp,
        107 => ArrowRight,
        108 => ArrowDown,
        132 => ControlLeft,
        133 => ShiftLeft,
        134 => AltLeft,
        135 => SuperLeft,
        136 => ShiftRight,
        137 => ControlRight,
        138 => AltRight,
        139 => SuperRight,
        158 => Home,
        159 => End,
        168 => PageUp,
        169 => PageDown,
        215 => Num0,
        216 => Num1,
        217 => Num2,
        218 => Num3,
        219 => Num4,
        220 => Num5,
        221 => Num6,
        222 => Num7,
        223 => Num8,
        224 => Num9,
        202 => Backspace,
        28 => Enter,
        9 => Tab,
        23 => Delete,
        37 => ArrowLeft,
        38 => ArrowUp,
        39 => ArrowRight,
        40 => ArrowDown,
        16 => ShiftLeft,
        17 => ControlLeft,
        18 => AltLeft,
        91 => SuperLeft,
        162 => ShiftRight,
        163 => ControlRight,
        165 => AltRight,
        112 => Home,
        117 => End,
        116 => PageUp,
        118 => PageDown,
        _ => return None,
    };
    Some(key)
}

impl State {
    /// Apply one input sample, appending the resulting events. `scale` is the device
    /// pixel ratio: pointer/touch/axis coordinates arrive in physical pixels and are
    /// divided by it to become the logical points egui hit-tests in.
    pub fn apply(&mut self, input: &Input, scale: f32, events: &mut Vec<Event>) {
        match input.kind.as_str() {
            "touch" => self.touch(input, scale, events),
            "pointer" => self.pointer(input, scale, events),
            "key" => self.key(input, events),
            "axis" => self.axis(input, scale, events),
            "blur" => {
                // A blur releases any held pointer and clears touch bookkeeping.
                for _ in &self.touches {
                    events.push(Event::PointerButton {
                        pos: self.last_pointer.unwrap_or_default(),
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Modifiers::NONE,
                    });
                }
                self.touches.clear();
                self.pressed = None;
                self.touch_pointer = None;
            }
            "hover_leave" => {
                // No cursor on the surface: egui treats this as the pointer gone.
                events.push(Event::PointerGone);
            }
            _ => {}
        }
    }

    /// Drop any in-progress contact (surface destroyed).
    pub fn cancel(&mut self, events: &mut Vec<Event>) {
        for _ in &self.touches {
            events.push(Event::PointerButton {
                pos: self.last_pointer.unwrap_or_default(),
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
        }
        self.touches.clear();
        self.pressed = None;
        self.touch_pointer = None;
    }
}

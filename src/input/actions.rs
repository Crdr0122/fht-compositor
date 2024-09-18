use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize, Serializer};
use smithay::backend::input::MouseButton;
use smithay::input::keyboard::{Keysym, ModifiersState};
use smithay::input::pointer::{CursorIcon, CursorImageStatus, Focus};
use smithay::utils::Serial;
use smithay::wayland::shell::xdg::XdgShellHandler;

use super::swap_tile_grab::SwapTileGrab;
use crate::config::CONFIG;
use crate::shell::PointerFocusTarget;
use crate::state::State;
use crate::utils::output::OutputExt;
use crate::utils::RectCenterExt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum Modifiers {
    ALT,
    CTRL,
    SHIFT,
    SUPER,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct FhtModifiersState {
    alt: bool,
    ctrl: bool,
    logo: bool,
    shift: bool,
}

impl From<ModifiersState> for FhtModifiersState {
    fn from(value: ModifiersState) -> Self {
        Self {
            alt: value.alt,
            ctrl: value.ctrl,
            logo: value.logo,
            shift: value.shift,
        }
    }
}

impl Serialize for FhtModifiersState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;

        if self.alt {
            seq.serialize_element(&Modifiers::ALT)?;
        }
        if self.ctrl {
            seq.serialize_element(&Modifiers::CTRL)?;
        }
        if self.logo {
            seq.serialize_element(&Modifiers::SUPER)?;
        }
        if self.shift {
            seq.serialize_element(&Modifiers::SHIFT)?;
        }

        seq.end()
    }
}

impl<'de> Deserialize<'de> for FhtModifiersState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mods = Vec::<Modifiers>::deserialize(deserializer)?;
        Ok(Self {
            alt: mods.contains(&Modifiers::ALT),
            ctrl: mods.contains(&Modifiers::CTRL),
            logo: mods.contains(&Modifiers::SUPER),
            shift: mods.contains(&Modifiers::SHIFT),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyAction {
    Quit,

    ReloadConfig,

    RunCommand(String),

    SelectNextLayout,

    SelectPreviousLayout,

    ChangeMwfact(f32),

    ChangeNmaster(i32),

    ChangeCfact(f32),

    MaximizeFocusedWindow,

    FullscreenFocusedWindow,

    FocusNextWindow,

    FocusPreviousWindow,

    SwapWithNextWindow,

    SwapWithPreviousWindow,

    FocusNextOutput,

    FocusPreviousOutput,

    CloseFocusedWindow,

    FocusWorkspace(usize),

    SendFocusedWindowToWorkspace(usize),

    ToggleDebugOverlayOnFocusedTile,

    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct KeyPattern(
    pub FhtModifiersState,
    #[serde(serialize_with = "ser::serialize_keysym")]
    #[serde(deserialize_with = "ser::deserialize_keysym")]
    pub Keysym,
);

mod ser {

    use serde::de::Unexpected;
    use serde::{Deserialize, Deserializer, Serializer};
    use smithay::input::keyboard::xkb::{self, keysyms};
    use smithay::input::keyboard::Keysym;

    pub fn serialize_keysym<S: Serializer>(
        keysym: &Keysym,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(smithay::input::keyboard::xkb::keysym_get_name(*keysym).as_str())
    }

    pub fn deserialize_keysym<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Keysym, D::Error> {
        let name = String::deserialize(deserializer)?;

        // From the xkb rust crate itself, they recommend searching with `KEY_NO_FLAGS` then search
        // with `CASE_INSENSITIVE` to be more precise in your search, since
        // `KEYSYM_CASE_INSENSITIVE` will always return the lowercase letter
        match xkb::keysym_from_name(&name, xkb::KEYSYM_NO_FLAGS).raw() {
            keysyms::KEY_NoSymbol => {
                match xkb::keysym_from_name(&name, xkb::KEYSYM_CASE_INSENSITIVE).raw() {
                    keysyms::KEY_NoSymbol => Err(<D::Error as serde::de::Error>::invalid_value(
                        Unexpected::Str(&name),
                        &"Invalid keysym!",
                    )),
                    keysym => Ok(keysym.into()),
                }
            }
            keysym => Ok(keysym.into()),
        }
    }
}

impl State {
    #[profiling::function]
    pub fn process_key_action(&mut self, action: KeyAction) {
        let Some(ref output) = self.fht.focus_state.output.clone() else {
            return;
        };
        let wset = self.fht.wset_mut_for(output);
        let active = wset.active_mut();

        match action {
            KeyAction::Quit => self
                .fht
                .stop
                .store(true, std::sync::atomic::Ordering::SeqCst),
            KeyAction::ReloadConfig => self.reload_config(),
            KeyAction::RunCommand(cmd) => crate::utils::spawn(cmd),
            KeyAction::SelectNextLayout => active.select_next_layout(true),
            KeyAction::SelectPreviousLayout => active.select_previous_layout(true),
            KeyAction::ChangeMwfact(delta) => active.change_mwfact(delta, true),
            KeyAction::ChangeNmaster(delta) => active.change_nmaster(delta, true),
            KeyAction::ChangeCfact(delta) => {
                let mut arrange = false;
                if let Some(tile) = active.focused_tile_mut() {
                    tile.change_cfact(delta);
                    arrange = true;
                }
                if arrange {
                    active.arrange_tiles(true);
                }
            }
            KeyAction::MaximizeFocusedWindow => {
                if let Some(window) = active.focused() {
                    let new_maximized = !window.maximized();
                    window.request_maximized(new_maximized);
                    active.arrange_tiles(true);
                }
            }
            KeyAction::FullscreenFocusedWindow => {
                if let Some(window) = active.focused() {
                    if window.fullscreen() {
                        window.request_fullscreen(false);
                    } else {
                        let toplevel = window.toplevel().clone();
                        self.fullscreen_request(toplevel, None);
                    }
                }
            }
            KeyAction::FocusNextWindow => {
                let new_focus = active.focus_next_window(true);
                if let Some(window) = new_focus {
                    if CONFIG.general.cursor_warps {
                        let center = active.window_geometry(&window).unwrap().center();
                        self.move_pointer(center.to_f64())
                    }
                    self.set_focus_target(Some(window.into()));
                }
            }
            KeyAction::FocusPreviousWindow => {
                let new_focus = active.focus_previous_window(true);
                if let Some(window) = new_focus {
                    if CONFIG.general.cursor_warps {
                        let center = active.window_geometry(&window).unwrap().center();
                        self.move_pointer(center.to_f64())
                    }
                    self.set_focus_target(Some(window.into()));
                }
            }
            KeyAction::SwapWithNextWindow => {
                active.swap_with_next_window(true);
                if let Some(window) = active.focused() {
                    if CONFIG.general.cursor_warps {
                        let center = active.window_geometry(&window).unwrap().center();
                        self.move_pointer(center.to_f64())
                    }
                    self.set_focus_target(Some(window.into()));
                }
            }
            KeyAction::SwapWithPreviousWindow => {
                active.swap_with_previous_window(true);
                if let Some(window) = active.focused() {
                    if CONFIG.general.cursor_warps {
                        let center = active.window_geometry(&window).unwrap().center();
                        self.move_pointer(center.to_f64())
                    }
                    self.set_focus_target(Some(window.into()));
                }
            }
            KeyAction::FocusNextOutput => {
                let outputs_len = self.fht.workspaces.len();
                if outputs_len < 2 {
                    return;
                }

                let current_output_idx = self
                    .fht
                    .outputs()
                    .position(|o| o == output)
                    .expect("Focused output is not registered");

                let mut next_output_idx = current_output_idx + 1;
                if next_output_idx == outputs_len {
                    next_output_idx = 0;
                }

                let output = self
                    .fht
                    .outputs()
                    .skip(next_output_idx)
                    .next()
                    .unwrap()
                    .clone();
                if CONFIG.general.cursor_warps {
                    let center = output.geometry().center();
                    self.move_pointer(center.to_f64());
                }
                self.fht.focus_state.output.replace(output).unwrap();
            }
            KeyAction::FocusPreviousOutput => {
                let outputs_len = self.fht.workspaces.len();
                if outputs_len < 2 {
                    return;
                }

                let current_output_idx = self
                    .fht
                    .outputs()
                    .position(|o| o == output)
                    .expect("Focused output is not registered");

                let next_output_idx = match current_output_idx.checked_sub(1) {
                    Some(idx) => idx,
                    None => outputs_len - 1,
                };

                let output = self
                    .fht
                    .outputs()
                    .skip(next_output_idx)
                    .next()
                    .unwrap()
                    .clone();
                if CONFIG.general.cursor_warps {
                    let center = output.geometry().center();
                    self.move_pointer(center.to_f64());
                }
                self.fht.focus_state.output.replace(output).unwrap();
            }
            KeyAction::CloseFocusedWindow => {
                let Some(window) = active.focused() else {
                    return;
                };
                window.toplevel().send_close();
            }
            KeyAction::FocusWorkspace(idx) => {
                if let Some(window) = wset.set_active_idx(idx, true) {
                    self.set_focus_target(Some(window.into()));
                };
            }
            KeyAction::SendFocusedWindowToWorkspace(idx) => {
                let Some(window) = active.focused() else {
                    return;
                };
                let (window, border_config) =
                    active.remove_tile(&window, true).unwrap().into_window();
                let new_focus = active.focused();
                let idx = idx.clamp(0, 9);
                wset.get_workspace_mut(idx)
                    .insert_window(window, border_config, true);

                if let Some(window) = new_focus {
                    self.set_focus_target(Some(window.into()));
                }
            }
            KeyAction::ToggleDebugOverlayOnFocusedTile => {
                // TODO:
                // let Some(tile) = active.focused_tile_mut() else {
                //     return;
                // };
                //
                // if tile.debug_overlay.take().is_none() {
                //     tile.debug_overlay = Some(crate::egui::EguiElement::new(tile.element.size()))
                // }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FhtMouseButton {
    Left,
    Middle,
    Right,
    Forward,
    Back,
}

impl From<MouseButton> for FhtMouseButton {
    fn from(value: MouseButton) -> Self {
        match value {
            MouseButton::Left => Self::Left,
            MouseButton::Middle => Self::Middle,
            MouseButton::Right => Self::Right,
            MouseButton::Forward => Self::Forward,
            MouseButton::Back => Self::Back,
            _ => Self::Left,
        }
    }
}

impl Into<MouseButton> for FhtMouseButton {
    fn into(self) -> MouseButton {
        match self {
            Self::Left => MouseButton::Left,
            Self::Middle => MouseButton::Middle,
            Self::Right => MouseButton::Right,
            Self::Forward => MouseButton::Forward,
            Self::Back => MouseButton::Back,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MouseAction {
    MoveTile,
    ResizeTile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct MousePattern(pub FhtModifiersState, pub FhtMouseButton);

impl State {
    #[profiling::function]
    pub fn process_mouse_action(&mut self, action: MouseAction, serial: Serial) {
        let pointer_loc = self.fht.pointer.current_location();

        match action {
            MouseAction::MoveTile => {
                if let Some((PointerFocusTarget::Window(window), _)) =
                    self.fht.focus_target_under(pointer_loc)
                {
                    self.fht.loop_handle.insert_idle(move |state| {
                        let pointer = state.fht.pointer.clone();
                        if !pointer.has_grab(serial) {
                            return;
                        }
                        let Some(start_data) = pointer.grab_start_data() else {
                            return;
                        };
                        if let Some(workspace) = state.fht.workspace_for_window_mut(&window) {
                            if workspace.start_interactive_swap(&window) {
                                state.fht.loop_handle.insert_idle(|state| {
                                    // TODO: Figure out why I have todo this inside a idle
                                    state.fht.interactive_grab_active = true;
                                    state.fht.cursor_theme_manager.set_image_status(
                                        CursorImageStatus::Named(CursorIcon::Grabbing),
                                    );
                                });
                                let grab = SwapTileGrab { window, start_data };
                                pointer.set_grab(state, grab, serial, Focus::Clear);
                            }
                        }
                    });
                }
            }
            _ => (),
            // MouseAction::ResizeTile => {
            //     if let Some((PointerFocusTarget::Window(window), _)) =
            //         self.fht.focus_target_under(pointer_loc)
            //     {
            //         let pointer_loc = self.fht.pointer.current_location();
            //         let Rectangle { loc, size } =
            //             self.fht.window_visual_geometry(&window).unwrap().to_f64();
            //
            //         let pointer_loc_in_window = pointer_loc - loc;
            //         if window.surface_under(pointer_loc_in_window,
            // WindowSurfaceType::ALL).is_none() {             return;
            //         }
            //
            //         // We divide the window into 9 sections, so that if you grab for example
            //         // somewhere in the middle of the bottom edge, you can only resize
            // vertically.         let mut edges = ResizeEdge::empty();
            //         if pointer_loc_in_window.x < size.w / 3. {
            //             edges |= ResizeEdge::LEFT;
            //         } else if 2. * size.w / 3. < pointer_loc_in_window.x {
            //             edges |= ResizeEdge::RIGHT;
            //         }
            //         if pointer_loc_in_window.y < size.h / 3. {
            //             edges |= ResizeEdge::TOP;
            //         } else if 2. * size.h / 3. < pointer_loc_in_window.y {
            //             edges |= ResizeEdge::BOTTOM;
            //         }
            //
            //         self.fht.loop_handle.insert_idle(move |state| {
            //             state.handle_resize_request(window, serial, edges)
            //         });
            //     }
            // }
        }
    }
}

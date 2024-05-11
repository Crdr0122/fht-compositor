pub mod cursor;
pub mod focus_target;
pub mod grabs;
pub mod window;
pub mod workspaces;

use smithay::desktop::{
    find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, PopupKind, Window, WindowSurfaceType
};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Monotonic, Point, Time};
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::Layer;
use smithay::wayland::shell::xdg::{PopupSurface, XdgToplevelSurfaceData};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;

pub use self::focus_target::{KeyboardFocusTarget, PointerFocusTarget};
use self::workspaces::tile::{WorkspaceElement, WorkspaceTile};
use self::workspaces::{Workspace, WorkspaceSwitchAnimation};
use crate::config::CONFIG;
use crate::state::{Fht};
use crate::utils::geometry::{
    Global, PointExt, PointGlobalExt, PointLocalExt, RectCenterExt, RectExt, RectGlobalExt,
    RectLocalExt, SizeExt,
};
use crate::utils::output::OutputExt;

impl Fht {
    /// Get the [`FocusTarget`] under the cursor.
    ///
    /// It checks the surface under the cursor using the following order:
    /// - [`Overlay`] layer shells.
    /// - [`Fullscreen`] windows on the active workspace.
    /// - [`Top`] layer shells.
    /// - Normal/Maximized windows on the active workspace.
    /// - [`Bottom`] layer shells.
    /// - [`Background`] layer shells.
    pub fn focus_target_under(
        &self,
        point: Point<f64, Global>,
    ) -> Option<(PointerFocusTarget, Point<i32, Global>)> {
        let output = self.focus_state.output.as_ref()?;
        let wset = self.wset_for(output);
        let layer_map = layer_map_for_output(output);

        let mut under = None;

        if let Some(layer) = layer_map.layer_under(Layer::Overlay, point.as_logical()) {
            let layer_loc = layer_map.layer_geometry(layer).unwrap().loc.as_local();
            under = Some((layer.clone().into(), layer_loc.to_global(output)))
        } else if let Some((fullscreen, loc)) = wset.current_fullscreen() {
            under = Some((fullscreen.clone().into(), loc))
        } else if let Some(layer) = layer_map.layer_under(Layer::Top, point.as_logical()) {
            let layer_loc = layer_map.layer_geometry(layer).unwrap().loc.as_local();
            under = Some((layer.clone().into(), layer_loc.to_global(output)))
        } else if let Some((window, loc)) = wset.window_under(point) {
            under = Some((window.clone().into(), loc))
        } else if let Some(layer) = layer_map
            .layer_under(Layer::Bottom, point.as_logical())
            .or_else(|| layer_map.layer_under(Layer::Background, point.as_logical()))
        {
            let layer_loc = layer_map.layer_geometry(layer).unwrap().loc.as_local();
            under = Some((layer.clone().into(), layer_loc.to_global(output)))
        }

        under
    }

    /// Find the window associated with this [`WlSurface`]
    pub fn find_window(&self, surface: &WlSurface) -> Option<&Window> {
        self.workspaces()
            .find_map(|(_, wset)| wset.find_window(surface))
    }

    /// Find the window associated with this [`WlSurface`]
    pub fn find_window_and_workspace(
        &self,
        surface: &WlSurface,
    ) -> Option<(&Window, &Workspace<Window>)> {
        self.workspaces()
            .find_map(|(_, wset)| wset.find_window_and_workspace(surface))
    }

    /// Find the window associated with this [`WlSurface`], and the output the window is mapped
    /// onto
    pub fn find_window_and_output(&self, surface: &WlSurface) -> Option<(&Window, &Output)> {
        self.workspaces()
            .find_map(|(_, wset)| wset.find_window(surface).map(|w| (w, &wset.output)))
    }

    /// Get a reference to the workspace holding this window
    pub fn ws_for(&self, window: &Window) -> Option<&Workspace<Window>> {
        self.workspaces().find_map(|(_, wset)| wset.ws_for(window))
    }

    /// Get a mutable reference to the workspace holding this window
    pub fn ws_mut_for(&mut self, window: &Window) -> Option<&mut Workspace<Window>> {
        self.workspaces_mut()
            .find_map(|(_, wset)| wset.ws_mut_for(window))
    }

    /// Find the first output where this [`WlSurface`] is visible.
    ///
    /// This checks everything from layer shells to windows to override redirect windows etc.
    pub fn visible_output_for_surface(&self, surface: &WlSurface) -> Option<&Output> {
        self.outputs()
            .find(|o| {
                // Is the surface a layer shell?
                let layer_map = layer_map_for_output(o);
                layer_map
                    .layer_for_surface(surface, WindowSurfaceType::ALL)
                    .is_some()
            })
            .or_else(|| {
                // Pending layer_surface?
                self.pending_layers.iter().find_map(|(_, (l, output))| {
                    let mut found = false;
                    l.with_surfaces(|s, _| {
                        if s == surface {
                            found = true;
                        }
                    });
                    found.then_some(output)
                })
            })
            .or_else(|| {
                // Mapped window?
                self.workspaces().find_map(|(o, wset)| {
                    let active = wset.active();
                    if active
                        .tiles
                        .iter()
                        .any(|w| w.element().wl_surface().as_ref() == Some(surface))
                    {
                        return Some(o);
                    }

                    // if active
                    //     .fullscreen
                    //     .as_ref()
                    //     .is_some_and(|f| f.inner.has_surface(surface, WindowSurfaceType::ALL))
                    // {
                    //     return Some(o);
                    // }

                    None
                })
            })
    }

    /// Find every output where this window (and it's subsurfaces) is displayed.
    pub fn visible_outputs_for_window(&self, window: &Window) -> Box<dyn Iterator<Item = &Output> + '_> {
        let Some(ws) = self.ws_for(window) else { return Box::new(std::iter::empty()) };
        let Some(window_geo) = ws.element_geometry(window)  else { return Box::new(std::iter::empty()) };
        Box::new(self.outputs()
            .filter(move |o| o.geometry().intersection(window_geo).is_some()))
    }

    /// Find every window that is curently displayed on this output
    #[profiling::function]
    pub fn visible_windows_for_output(
        &self,
        output: &Output,
    ) -> Box<dyn Iterator<Item = &Window> + '_> {
        let wset = self.wset_for(output);
        let mut windows = Box::new(std::iter::empty()) as Box<dyn Iterator<Item = &Window>>;

        if let Some(WorkspaceSwitchAnimation { target_idx, .. }) = wset.switch_animation.as_ref() {
            let target = &wset.workspaces[*target_idx];
            // if let Some(fullscreen) = target.fullscreen.as_ref().map(|f| &f.inner) {
            //     windows = Box::new(windows.chain(std::iter::once(fullscreen)));
            // } else {
            windows = Box::new(windows.chain(target.tiles.iter().map(WorkspaceTile::element)));
            // }
        }

        let active = wset.active();
        // if let Some(fullscreen) = active.fullscreen.as_ref().map(|f| &f.inner) {
        //     windows = Box::new(windows.chain(std::iter::once(fullscreen)))
        // } else {
        windows = Box::new(windows.chain(active.tiles.iter().map(WorkspaceTile::element)));
        // };

        Box::new(windows)
    }

    /// Prepapre a pending window to be mapped.
    pub fn prepare_pending_window(&mut self, window: Window) {
        let mut output = self.focus_state.output.clone().unwrap();
        let wl_surface = window.wl_surface().unwrap();

        // Get the matching mapping setting, if the user specified one.
        let workspace_idx = self.wset_for(&output).get_active_idx();
        let (title, app_id) = with_states(&wl_surface, |states| {
            let data = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap();
            (
                data.title.clone().unwrap_or_default(),
                data.app_id.clone().unwrap_or_default(),
            )
        });

        let map_settings = CONFIG
            .rules
            .iter()
            .find(|(rules, _)| {
                rules
                    .iter()
                    .any(|r| r.matches(&title, &app_id, workspace_idx))
            })
            .map(|(_, settings)| settings.clone())
            .unwrap_or_default();

        // Apply rules
        //
        // First start with the output since every operation (mapping,  fullscreening, etc...) will
        // be done relative to the output.
        if let Some(target_output) = map_settings
            .output
            .as_ref()
            .and_then(|name| self.outputs().find(|o| o.name().as_str() == name))
            .cloned()
        {
            output = target_output;
        }

        let wset = self.wset_mut_for(&output);

        let workspace_idx = match map_settings.workspace {
            None => wset.get_active_idx(),
            Some(idx) => idx.clamp(0, 9),
        };
        let workspace = &mut wset.workspaces[workspace_idx];
        let layout = workspace.get_active_layout();

        // Pre compute window geometry for insertion.
        // Bogus tile so we can use the arrange_tiles
        let mut tile = WorkspaceTile::new(window);
        let inner_gaps = CONFIG.general.inner_gaps;
        let outer_gaps = CONFIG.general.outer_gaps;

        let usable_geo = layer_map_for_output(&wset.output)
            .non_exclusive_zone()
            .as_local();
        let mut tile_area = usable_geo;
        tile_area.size -= (2 * outer_gaps, 2 * outer_gaps).into();
        tile_area.loc += (outer_gaps, outer_gaps).into();

        let tiles_len = workspace.tiles.len() + 1;
        layout.arrange_tiles(
            workspace.tiles.iter_mut().chain(std::iter::once(&mut tile)),
            tiles_len,
            tile_area,
            inner_gaps,
        );

        let WorkspaceTile {
            element: window, ..
        } = tile;

        // Client side-decorations
        let allow_csd = map_settings
            .allow_csd
            .unwrap_or(CONFIG.decoration.allow_csd);
        let toplevel = window.toplevel().unwrap();
        toplevel.with_pending_state(|state| {
            if allow_csd {
                state.decoration_mode = Some(DecorationMode::ClientSide)
            } else {
                state.decoration_mode = Some(DecorationMode::ServerSide)
            }
        });

        toplevel.send_configure();
        self.unmapped_windows.push((window, output, workspace_idx));
    }

    /// Map a pending window, if it's found.
    pub fn map_window(&mut self, window: Window, output: Output, workspace_idx: usize) {
        let loop_handle = self.loop_handle.clone();
        let wset = self.wset_mut_for(&output);
        let is_active = workspace_idx == wset.get_active_idx();
        let workspace = &mut wset.workspaces[workspace_idx];

        workspace.insert_element(window.clone(), None);

        // From using the compositor opening a window when a switch is being done feels more
        // natural when the window gets focus, even if focus_new_windows is none.
        let is_switching = wset.switch_animation.is_some();
        let should_focus = (CONFIG.general.focus_new_windows || is_switching) && is_active;

        if should_focus {
            let center = workspace.element_geometry(&window).unwrap().center();

            loop_handle.insert_idle(move |state| {
                if CONFIG.general.cursor_warps {
                    state.move_pointer(center.to_f64());
                }
                state.set_focus_target(Some(window.clone().into()));
            });
        }
    }

    /// Unconstraint a popup.
    ///
    /// Basically changes its geometry and location so that it doesn't overflow outside of the
    /// parent window's output.
    pub fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some((window, workspace)) = self.find_window_and_workspace(&root) else {
            return;
        };

        let mut outputs_for_window = self.visible_outputs_for_window(window);
        if outputs_for_window.next().is_none() {
            return;
        }

        let mut outputs_geo = outputs_for_window
            .next()
            .unwrap_or_else(|| self.outputs().next().unwrap())
            .geometry();
        for output in outputs_for_window {
            outputs_geo = outputs_geo.merge(output.geometry());
        }

        // The target (aka the popup) geometry should be relative to the parent (aka the window's)
        // geometry, based on the xdg_shell protocol requirements.
        let mut target = outputs_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone())).as_global();
        target.loc -= workspace.element_location(window).unwrap();

        popup.with_pending_state(|state| {
            state.geometry = state
                .positioner
                .get_unconstrained_geometry(target.as_logical());
        });
    }

    /// Advance all the active animations for this given output
    pub fn advance_animations(&mut self, output: &Output, current_time: Time<Monotonic>) -> bool {
        let mut animations_running = false;
        let wset = self.wset_mut_for(output);
        if let Some(WorkspaceSwitchAnimation { target_idx, .. }) =
            wset.switch_animation.take_if(|a| a.animation.is_finished())
        {
            wset.active_idx
                .store(target_idx, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(animation) = wset.switch_animation.as_mut() {
            animation.animation.set_current_time(current_time);
            animations_running = true;
        }
        let workspace = wset.active();

        animations_running
    }

    /// Get an interator over all the windows registered in the compositor.
    pub fn all_windows(&self) -> impl Iterator<Item = &Window> + '_ {
        self.workspaces.values().flat_map(|wset| {
            let workspaces = &wset.workspaces;
            workspaces
                .iter()
                .flat_map(|ws| ws.tiles.iter().map(|tile| tile.element()))
        })
    }
}

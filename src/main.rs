mod bar;
mod config;
mod icons;
mod menu;
mod modules;
mod niri;
mod render;
mod tray;

use std::collections::HashSet;
use std::time::Duration;

use calloop::EventLoop;
use calloop::channel;
use calloop::timer::{TimeoutAction, Timer};
use calloop_wayland_source::WaylandSource;
use chrono::{Local, Timelike};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_positioner;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, PointerEvent, PointerEventKind, PointerHandler,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::xdg::popup::{Popup, PopupConfigure, PopupHandler};
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowHandler};
use smithay_client_toolkit::shell::xdg::{XdgPositioner, XdgShell};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_registry, registry_handlers};
use system_tray::client::ActivateRequest;
use tokio::sync::mpsc::UnboundedSender;

use crate::bar::{Context, Hit, ModuleSlot, Target};
use crate::config::{BAR_HEIGHT, CLOCK_CLICK};
use crate::icons::IconCache;
use crate::modules::Runner;
use crate::niri::Workspace;
use crate::render::{Canvas, Text};
use crate::tray::{Request, TrayItem};

/// Updates from the worker threads.
pub enum Msg {
    Module(usize, String),
    Workspaces(Vec<Workspace>),
    Tray(Vec<TrayItem>),
}

struct Bar {
    output: wl_output::WlOutput,
    name: String,
    layer: LayerSurface,
    width: u32,
    scale: i32,
    configured: bool,
    hits: Vec<Hit>,
}

struct OpenMenu {
    address: String,
    menu_path: String,
    /// Surface of the bar it was opened from.
    parent: wl_surface::WlSurface,
    anchor_x: f32,
    popup: Popup,
    expanded: HashSet<i32>,
    hovered: Option<i32>,
    /// Logical size last requested from the compositor.
    size: (u32, u32),
    scale: i32,
    /// False until the compositor has configured the current size.
    configured: bool,
    token: u32,
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    xdg_shell: XdgShell,
    shm: Shm,
    pool: SlotPool,
    cursor_shape: Option<CursorShapeManager>,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    cursor: Option<WpCursorShapeDeviceV1>,

    bars: Vec<Bar>,
    menu: Option<OpenMenu>,
    bars_dirty: bool,
    menu_dirty: bool,
    /// Scroll accumulated over the target being scrolled.
    scroll: Option<(Target, f64)>,
    /// Minute last shown by the clock.
    clock_minute: i64,

    text: Text,
    icons: IconCache,
    modules: Vec<ModuleSlot>,
    workspaces: Vec<Workspace>,
    tray: Vec<TrayItem>,
    tray_tx: UnboundedSender<Request>,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    modules::install_exit_handler();

    let conn = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init(&conn)?;
    let qh = queue.handle();

    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn, queue).insert(handle.clone()).map_err(|e| anyhow::anyhow!("{e}"))?;

    let (tx, rx) = channel::channel::<Msg>();
    handle
        .insert_source(rx, |event, _, app| {
            if let channel::Event::Msg(msg) = event {
                app.handle(msg);
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    handle
        .insert_source(Timer::from_duration(clock_tick()), |_, _, app| {
            if current_minute() != app.clock_minute {
                app.bars_dirty = true;
            }
            TimeoutAction::ToDuration(clock_tick())
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let modules = config::modules()
        .into_iter()
        .enumerate()
        .map(|(i, config)| ModuleSlot { runner: Runner::spawn(i, &config, tx.clone()), config, raw: String::new() })
        .collect();
    niri::spawn(tx.clone());
    let tray_tx = tray::spawn(tx);

    let shm = Shm::bind(&globals, &qh)?;
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh)?,
        layer_shell: LayerShell::bind(&globals, &qh)?,
        xdg_shell: XdgShell::bind(&globals, &qh)?,
        pool: SlotPool::new(1920 * BAR_HEIGHT as usize * 4, &shm)?,
        shm,
        cursor_shape: CursorShapeManager::bind(&globals, &qh).ok(),
        seat: None,
        pointer: None,
        cursor: None,
        bars: Vec::new(),
        menu: None,
        bars_dirty: false,
        menu_dirty: false,
        scroll: None,
        clock_minute: 0,
        text: Text::new(),
        icons: IconCache::default(),
        modules,
        workspaces: Vec::new(),
        tray: Vec::new(),
        tray_tx,
    };

    let result = loop {
        if let Err(e) = event_loop.dispatch(None, &mut app) {
            break e;
        }
        app.flush();
    };
    // e.g. the compositor went away; don't leave watch pipelines behind.
    modules::kill_all();
    Err(result.into())
}

fn current_minute() -> i64 {
    Local::now().timestamp().div_euclid(60)
}

/// Wakes at the next minute boundary, or sooner: the timer is monotonic and
/// stops during suspend, so short ticks catch up quickly after resume.
fn clock_tick() -> Duration {
    let now = Local::now();
    let elapsed = Duration::new(now.second() as u64, now.nanosecond() % 1_000_000_000);
    let next_minute = Duration::from_secs(60).saturating_sub(elapsed) + Duration::from_millis(20);
    next_minute.min(Duration::from_secs(5))
}

fn present(pool: &mut SlotPool, surface: &wl_surface::WlSurface, canvas: &Canvas) {
    let (w, h) = (canvas.pixmap.width() as i32, canvas.pixmap.height() as i32);
    let (buffer, data) = match pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) {
        Ok(b) => b,
        Err(e) => {
            log::error!("failed to allocate buffer: {e}");
            return;
        }
    };
    canvas.copy_to_shm(data);
    surface.set_buffer_scale(canvas.scale as i32);
    surface.damage_buffer(0, 0, w, h);
    if let Err(e) = buffer.attach_to(surface) {
        log::error!("failed to attach buffer: {e}");
        return;
    }
    surface.commit();
}

impl App {
    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Module(i, raw) => {
                if self.modules[i].raw != raw {
                    self.modules[i].raw = raw;
                    self.bars_dirty = true;
                }
            }
            Msg::Workspaces(workspaces) => {
                self.workspaces = workspaces;
                self.bars_dirty = true;
            }
            Msg::Tray(items) => {
                self.tray = items;
                self.bars_dirty = true;
                self.menu_dirty = true;
            }
        }
    }

    /// Redraws whatever changed since the last pass through the event loop.
    fn flush(&mut self) {
        if std::mem::take(&mut self.bars_dirty) {
            self.clock_minute = current_minute();
            for i in 0..self.bars.len() {
                self.draw_bar(i);
            }
        }
        if std::mem::take(&mut self.menu_dirty) {
            self.draw_menu();
        }
    }

    fn draw_bar(&mut self, i: usize) {
        let bar = &mut self.bars[i];
        if !bar.configured {
            return;
        }
        let Some(mut canvas) = Canvas::new(bar.width, BAR_HEIGHT, bar.scale as u32) else { return };
        let mut cx = Context {
            text: &mut self.text,
            icons: &mut self.icons,
            modules: &self.modules,
            workspaces: &self.workspaces,
            tray: &self.tray,
            now: Local::now(),
        };
        bar.hits = bar::draw(&mut canvas, &mut cx, bar.width as f32, &bar.name);
        present(&mut self.pool, bar.layer.wl_surface(), &canvas);
    }

    fn create_bar(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let Some(info) = self.output_state.info(&output) else { return };
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(qh, surface, Layer::Top, Some("simpbar"), Some(&output));
        layer.set_anchor(Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, BAR_HEIGHT);
        layer.set_exclusive_zone(BAR_HEIGHT as i32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        self.bars.push(Bar {
            output,
            name: info.name.unwrap_or_default(),
            layer,
            width: 0,
            scale: info.scale_factor.max(1),
            configured: false,
            hits: Vec::new(),
        });
    }

    fn bar_index(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.bars.iter().position(|b| b.layer.wl_surface() == surface)
    }

    fn is_menu(&self, surface: &wl_surface::WlSurface) -> bool {
        self.menu.as_ref().is_some_and(|m| m.popup.wl_surface() == surface)
    }

    fn on_bar_press(&mut self, qh: &QueueHandle<Self>, i: usize, x: f64, button: u32, serial: u32) {
        let Some(hit) = bar::hit_test(&self.bars[i].hits, x).cloned() else { return };
        match (hit.target, button) {
            (Target::Workspace(id), BTN_LEFT) => niri::focus_workspace(id),
            (Target::Clock, BTN_LEFT) => modules::exec_detached(CLOCK_CLICK),
            (Target::Module(m), BTN_LEFT) => modules::exec_detached(self.modules[m].config.click),
            (Target::Tray(address), _) => {
                let Some(item) = self.tray.iter().find(|t| t.address == address) else { return };
                if button == BTN_MIDDLE {
                    self.tray_request(Request::Activate(ActivateRequest::Secondary { address, x: 0, y: 0 }));
                } else if button == BTN_RIGHT || item.item_is_menu {
                    if let Some(menu_path) = item.menu_path.clone() {
                        self.open_menu(qh, i, address, menu_path, hit.x + hit.w / 2.0, serial);
                    }
                } else if button == BTN_LEFT {
                    self.tray_request(Request::Activate(ActivateRequest::Default { address, x: 0, y: 0 }));
                }
            }
            _ => {}
        }
    }

    fn on_bar_scroll(&mut self, i: usize, x: f64, vertical: AxisScroll) {
        let Some(hit) = bar::hit_test(&self.bars[i].hits, x) else {
            self.scroll = None;
            return;
        };
        // Accumulate in Qt angleDelta units (120 per wheel notch, positive is
        // up) so high-resolution wheels and touchpads step once per notch.
        let units = if vertical.value120 != 0 {
            -vertical.value120 as f64
        } else if vertical.discrete != 0 {
            -vertical.discrete as f64 * 120.0
        } else {
            // Touchpads report pixels; treat 15px like one notch.
            -vertical.absolute * 8.0
        };
        let acc = match &mut self.scroll {
            Some((target, acc)) if *target == hit.target => acc,
            scroll => &mut scroll.insert((hit.target.clone(), 0.0)).1,
        };
        *acc += units;
        let steps = (*acc / 120.0).trunc() as i32;
        *acc -= steps as f64 * 120.0;
        if vertical.stop {
            *acc = 0.0;
        }
        if steps == 0 {
            return;
        }
        let delta = steps * 120;
        match &hit.target {
            Target::Module(m) => {
                let slot = &self.modules[*m];
                if !slot.config.scroll_up.is_empty() {
                    let cmd = if delta > 0 { slot.config.scroll_up } else { slot.config.scroll_down };
                    slot.runner.action(cmd);
                }
            }
            Target::Tray(address) => {
                let address = address.clone();
                self.tray_request(Request::Scroll { address, delta });
            }
            _ => {}
        }
    }

    fn tray_request(&self, req: Request) {
        let _ = self.tray_tx.send(req);
    }

    fn positioner(&self, bar_width: u32, anchor_x: f32, (w, h): (u32, u32)) -> Option<XdgPositioner> {
        let p = XdgPositioner::new(&self.xdg_shell).ok()?;
        let max_x = (bar_width as f32 - w as f32 - 4.0).max(4.0);
        let x = (anchor_x - w as f32 / 2.0).clamp(4.0, max_x).round() as i32;
        p.set_size(w as i32, h as i32);
        p.set_anchor_rect(x, 0, w as i32, 1);
        p.set_anchor(xdg_positioner::Anchor::Top);
        p.set_gravity(xdg_positioner::Gravity::Top);
        p.set_offset(0, -4);
        p.set_constraint_adjustment(
            xdg_positioner::ConstraintAdjustment::SlideX | xdg_positioner::ConstraintAdjustment::SlideY,
        );
        Some(p)
    }

    /// Logical size the menu for `address` needs with `expanded` submenus open.
    fn menu_size(&self, address: &str, expanded: &HashSet<i32>) -> Option<(u32, u32)> {
        let item = self.tray.iter().find(|t| t.address == address)?;
        let items = item.menu.as_ref().map_or(&[][..], |m| &m.submenus[..]);
        let (w, h) = menu::size(&menu::layout(items, expanded));
        Some((w.ceil() as u32, h.ceil() as u32))
    }

    fn open_menu(
        &mut self,
        qh: &QueueHandle<Self>,
        bar: usize,
        address: String,
        menu_path: String,
        anchor_x: f32,
        serial: u32,
    ) {
        self.menu = None;
        let expanded = HashSet::new();
        let Some(size) = self.menu_size(&address, &expanded) else { return };
        let bar = &self.bars[bar];
        let Some(positioner) = self.positioner(bar.width, anchor_x, size) else { return };
        let surface = self.compositor.create_surface(qh);
        let popup = match Popup::from_surface(None, &positioner, qh, surface, &self.xdg_shell) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to create menu popup: {e}");
                return;
            }
        };
        bar.layer.get_popup(popup.xdg_popup());
        if let Some(seat) = &self.seat {
            popup.xdg_popup().grab(seat, serial);
        }
        popup.wl_surface().commit();

        self.tray_request(Request::AboutToShow { address: address.clone(), menu_path: menu_path.clone(), id: 0 });
        self.menu = Some(OpenMenu {
            address,
            menu_path,
            parent: bar.layer.wl_surface().clone(),
            anchor_x,
            popup,
            expanded,
            hovered: None,
            size,
            scale: bar.scale,
            configured: false,
            token: 0,
        });
    }

    fn draw_menu(&mut self) {
        let Some(m) = &self.menu else { return };
        let item = self.tray.iter().find(|t| t.address == m.address);
        if item.is_none_or(|t| t.menu_path.is_none()) {
            self.menu = None;
            return;
        }
        if !m.configured {
            return;
        }
        let size = self.menu_size(&m.address, &m.expanded).unwrap_or(m.size);
        if size != m.size && m.popup.xdg_popup().version() >= 3 {
            // Wait for the compositor to place the resized popup before drawing.
            let bar_width = self.bars.iter().find(|b| b.layer.wl_surface() == &m.parent).map_or(0, |b| b.width);
            if let Some(positioner) = self.positioner(bar_width, m.anchor_x, size) {
                let m = self.menu.as_mut().unwrap();
                m.token += 1;
                m.size = size;
                m.configured = false;
                m.popup.reposition(&positioner, m.token);
                m.popup.wl_surface().commit();
                return;
            }
        }

        let m = self.menu.as_mut().unwrap();
        m.size = size;
        let item = self.tray.iter().find(|t| t.address == m.address).unwrap();
        let items = item.menu.as_ref().map_or(&[][..], |t| &t.submenus[..]);
        let rows = menu::layout(items, &m.expanded);
        let Some(mut canvas) = Canvas::new(size.0, size.1, m.scale.max(1) as u32) else { return };
        menu::draw(&mut canvas, &mut self.text, &mut self.icons, &rows, &m.expanded, m.hovered);
        present(&mut self.pool, m.popup.wl_surface(), &canvas);
    }

    fn on_menu_motion(&mut self, x: f64, y: f64) {
        let Some(m) = &self.menu else { return };
        let Some(item) = self.tray.iter().find(|t| t.address == m.address) else { return };
        let items = item.menu.as_ref().map_or(&[][..], |t| &t.submenus[..]);
        let rows = menu::layout(items, &m.expanded);
        let hovered = menu::row_at(&rows, x, y).filter(|r| !r.is_separator()).map(|r| r.item.id);
        if hovered != m.hovered {
            self.menu.as_mut().unwrap().hovered = hovered;
            self.menu_dirty = true;
        }
    }

    fn on_menu_click(&mut self, x: f64, y: f64) {
        let Some(m) = &self.menu else { return };
        let Some(item) = self.tray.iter().find(|t| t.address == m.address) else { return };
        let items = item.menu.as_ref().map_or(&[][..], |t| &t.submenus[..]);
        let rows = menu::layout(items, &m.expanded);
        let Some(row) = menu::row_at(&rows, x, y) else { return };
        if row.is_separator() || !row.item.enabled {
            return;
        }
        let (id, has_children) = (row.item.id, row.has_children());
        let (address, menu_path) = (m.address.clone(), m.menu_path.clone());
        if has_children {
            let m = self.menu.as_mut().unwrap();
            if !m.expanded.remove(&id) {
                m.expanded.insert(id);
                self.tray_request(Request::AboutToShow { address, menu_path, id });
            }
            self.menu_dirty = true;
        } else {
            self.tray_request(Request::Activate(ActivateRequest::MenuItem { address, menu_path, submenu_id: id }));
            self.menu = None;
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if let Some(i) = self.bar_index(surface) {
            self.bars[i].scale = factor.max(1);
            self.bars_dirty = true;
        } else if let Some(m) = self.menu.as_mut().filter(|m| m.popup.wl_surface() == surface) {
            m.scale = factor.max(1);
            self.menu_dirty = true;
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.create_bar(qh, output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        let Some(info) = self.output_state.info(&output) else { return };
        if let Some(bar) = self.bars.iter_mut().find(|b| b.output == output) {
            bar.name = info.name.unwrap_or_default();
            self.bars_dirty = true;
        }
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if let Some(i) = self.bars.iter().position(|b| b.output == output) {
            let bar = self.bars.remove(i);
            if self.menu.as_ref().is_some_and(|m| &m.parent == bar.layer.wl_surface()) {
                self.menu = None;
            }
        }
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self.menu.as_ref().is_some_and(|m| &m.parent == layer.wl_surface()) {
            self.menu = None;
        }
        self.bars.retain(|b| &b.layer != layer);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(bar) = self.bars.iter_mut().find(|b| &b.layer == layer) {
            bar.width = configure.new_size.0;
            bar.configured = true;
            self.bars_dirty = true;
        }
    }
}

impl PopupHandler for App {
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup, config: PopupConfigure) {
        if let Some(m) = self.menu.as_mut().filter(|m| m.popup.wl_surface() == popup.wl_surface()) {
            let stale = matches!(config.kind, smithay_client_toolkit::shell::xdg::popup::ConfigureKind::Reposition { token } if token != m.token);
            if !stale {
                m.configured = true;
                self.menu_dirty = true;
            }
        }
    }

    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
        if self.is_menu(popup.wl_surface()) {
            self.menu = None;
        }
    }
}

// Required to bind xdg_wm_base; the bar never creates toplevel windows.
impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {}
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, _: WindowConfigure, _: u32) {}
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.seat.get_or_insert(seat);
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => {
                    self.cursor = self.cursor_shape.as_ref().map(|m| m.get_shape_device(&pointer, qh));
                    self.pointer = Some(pointer);
                }
                Err(e) => log::error!("failed to get pointer: {e}"),
            }
        }
        self.seat.get_or_insert(seat);
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer {
            if let Some(cursor) = self.cursor.take() {
                cursor.destroy();
            }
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.seat.as_ref() == Some(&seat) {
            self.seat = None;
        }
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let (x, y) = event.position;
            let bar = self.bar_index(&event.surface);
            let on_menu = self.is_menu(&event.surface);
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    if let Some(cursor) = &self.cursor {
                        cursor.set_shape(serial, Shape::Default);
                    }
                    if on_menu {
                        self.on_menu_motion(x, y);
                    }
                }
                PointerEventKind::Leave { .. } if on_menu => {
                    if let Some(m) = self.menu.as_mut().filter(|m| m.hovered.is_some()) {
                        m.hovered = None;
                        self.menu_dirty = true;
                    }
                }
                PointerEventKind::Motion { .. } if on_menu => self.on_menu_motion(x, y),
                PointerEventKind::Press { button, serial, .. } => {
                    if let Some(i) = bar {
                        self.on_bar_press(qh, i, x, button, serial);
                    }
                }
                PointerEventKind::Release { button: BTN_LEFT, .. } if on_menu => self.on_menu_click(x, y),
                PointerEventKind::Axis { vertical, .. } => {
                    if let Some(i) = bar {
                        self.on_bar_scroll(i, x, vertical);
                    }
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);

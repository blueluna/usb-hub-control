use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use nusb::{DeviceInfo, MaybeFuture, Speed};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use usb_hub_control::{DEVICE_CLASS_HUB, Hub, LogicalPowerSwitchingMode};

type InfoMap = BTreeMap<Vec<u8>, DeviceInfo>;

/// Time between automatic refreshes of the selected hub's ports
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

fn key(info: &DeviceInfo) -> Vec<u8> {
    let mut key = vec![info.busnum()];
    key.extend(info.port_chain());
    key
}

fn location(info: &DeviceInfo) -> String {
    format!(
        "{}-{}",
        info.busnum(),
        info.port_chain()
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<String>>()
            .join(".")
    )
}

fn is_super_speed(info: &DeviceInfo) -> bool {
    matches!(info.speed(), Some(Speed::Super | Speed::SuperPlus))
}

fn list_devices() -> Result<InfoMap, String> {
    let devices = nusb::list_devices()
        .wait()
        .map_err(|e| format!("List devices failed, {}", e))?;
    Ok(devices.map(|info| (key(&info), info)).collect())
}

/// A physical hub, with the USB 2 and USB 3 halves fused
struct HubEntry {
    key: Vec<u8>,
    peer_key: Option<Vec<u8>>,
    label: String,
    vid_pid: String,
    switching: LogicalPowerSwitchingMode,
}

/// One half, USB 2 or USB 3, of a port
struct PortHalf {
    hub_key: Vec<u8>,
    port: u8,
    super_speed: bool,
    powered: Option<bool>,
    overcurrent: bool,
    device: Option<DeviceInfo>,
}

/// A physical port, with its halves sorted USB 2 first
struct PortRow {
    port: u8,
    halves: Vec<PortHalf>,
}

impl PortRow {
    fn any_powered(&self) -> bool {
        self.halves.iter().any(|half| half.powered == Some(true))
    }

    fn device(&self) -> Option<&DeviceInfo> {
        self.halves.iter().find_map(|half| half.device.as_ref())
    }
}

/// List hubs, fused with their other speed half, in tree order
fn load_hubs(info_map: &InfoMap) -> (Vec<HubEntry>, Vec<String>) {
    let mut hubs = vec![];
    let mut errors = vec![];
    let mut visited = BTreeSet::new();
    // USB 2 halves first, so fused hubs are listed by their USB 2 location
    for super_speed in [false, true] {
        for (key, info) in info_map.iter() {
            if key.len() < 2
                || info.class() != DEVICE_CLASS_HUB
                || is_super_speed(info) != super_speed
                || visited.contains(key)
            {
                continue;
            }
            visited.insert(key.clone());
            let hub = match Hub::from_device_info(info) {
                Ok(hub) => hub,
                Err(e) => {
                    errors.push(format!("{}: {}", location(info), e));
                    continue;
                }
            };
            let peer_key = hub
                .peer_port(1, info_map.values())
                .map(|(peer_info, _)| self::key(&peer_info));
            let mut label = format!(
                "{:indent$}{}",
                "",
                location(info),
                indent = (key.len() - 2) * 2
            );
            if let Some(peer_key) = &peer_key {
                visited.insert(peer_key.clone());
                if let Some(peer_info) = info_map.get(peer_key) {
                    label.push_str(&format!(" + {}", location(peer_info)));
                }
            }
            hubs.push(HubEntry {
                key: key.clone(),
                peer_key,
                label,
                vid_pid: format!("{:04x}:{:04x}", info.vendor_id(), info.product_id()),
                switching: hub.descriptor().logical_power_switching_mode(),
            });
        }
    }
    // Keep children right below their parent hub, USB 2 and USB 3 trees interleaved by location
    hubs.sort_by(|a, b| a.key.cmp(&b.key));
    (hubs, errors)
}

fn port_half(hub: &Hub, port: u8, info_map: &InfoMap) -> PortHalf {
    let hub_key = key(&hub.info());
    let (powered, overcurrent, connection) = match hub.port_status(port) {
        Ok(status) => (
            Some(status.powered()),
            status.overcurrent(),
            status.connection(),
        ),
        Err(_) => (None, false, true),
    };
    let device = if connection {
        let mut child_key = hub_key.clone();
        child_key.push(port);
        info_map.get(&child_key).cloned()
    } else {
        None
    };
    PortHalf {
        hub_key,
        port,
        super_speed: hub.is_super_speed(),
        powered,
        overcurrent,
        device,
    }
}

fn load_ports(entry: &HubEntry, info_map: &InfoMap) -> Result<Vec<PortRow>, String> {
    let info = info_map
        .get(&entry.key)
        .ok_or("Hub is gone, press r to reload")?;
    let hub = Hub::from_device_info(info).map_err(|e| format!("{}: {}", location(info), e))?;
    let peer_hub = entry
        .peer_key
        .as_ref()
        .and_then(|key| info_map.get(key))
        .and_then(|info| Hub::from_device_info(info).ok());

    let mut rows = vec![];
    for port in 1..=hub.port_count() {
        let mut halves = vec![port_half(&hub, port, info_map)];
        if peer_hub.is_some()
            && let Some((peer_info, peer_port)) = hub.peer_port(port, info_map.values())
            && let Ok(peer) = Hub::from_device_info(&peer_info)
        {
            halves.push(port_half(&peer, peer_port, info_map));
        }
        halves.sort_by_key(|half| half.super_speed);
        rows.push(PortRow { port, halves });
    }
    Ok(rows)
}

/// Switch port power on all halves of a port
fn switch_power(row: &PortRow, on: bool, info_map: &InfoMap) -> Result<(), String> {
    let mut halves = row.halves.iter().collect::<Vec<_>>();
    // VBUS only drops once both halves are off. Switch the USB 2 half off first and on last,
    // so the device does not fall back to USB 2 while the SuperSpeed half is still on.
    halves.sort_by_key(|half| half.super_speed != on);
    for half in halves {
        let info = info_map.get(&half.hub_key).ok_or("Hub is gone")?;
        let hub = Hub::from_device_info(info).map_err(|e| e.to_string())?;
        hub.set_port_power(half.port, on)
            .map_err(|e| format!("{} port {}: {}", location(info), half.port, e))?;
    }
    Ok(())
}

#[derive(PartialEq)]
enum Focus {
    Hubs,
    Ports,
}

struct App {
    info_map: InfoMap,
    hubs: Vec<HubEntry>,
    hub_state: ListState,
    ports: Vec<PortRow>,
    port_state: ListState,
    focus: Focus,
    message: String,
    last_refresh: Instant,
}

impl App {
    fn new() -> Self {
        let mut app = App {
            info_map: InfoMap::new(),
            hubs: vec![],
            hub_state: ListState::default(),
            ports: vec![],
            port_state: ListState::default(),
            focus: Focus::Hubs,
            message: String::new(),
            last_refresh: Instant::now(),
        };
        app.reload();
        app
    }

    fn selected_hub(&self) -> Option<&HubEntry> {
        self.hub_state.selected().and_then(|i| self.hubs.get(i))
    }

    /// Reload all hubs, keeping the selected hub when it is still there
    fn reload(&mut self) {
        let selected = self.selected_hub().map(|hub| hub.key.clone());
        match list_devices() {
            Ok(info_map) => self.info_map = info_map,
            Err(e) => {
                self.message = e;
                return;
            }
        }
        let (hubs, errors) = load_hubs(&self.info_map);
        self.hubs = hubs;
        self.message = match errors.len() {
            0 => String::new(),
            n => format!("{} hub(s) could not be opened, {}", n, errors.join(", ")),
        };
        let index = selected
            .and_then(|key| self.hubs.iter().position(|hub| hub.key == key))
            .or(if self.hubs.is_empty() { None } else { Some(0) });
        self.hub_state.select(index);
        self.refresh_ports();
    }

    /// Re-read the port status of the selected hub
    fn refresh_ports(&mut self) {
        self.last_refresh = Instant::now();
        if let Ok(info_map) = list_devices() {
            self.info_map = info_map;
        }
        self.ports = match self.selected_hub() {
            Some(entry) => match load_ports(entry, &self.info_map) {
                Ok(ports) => ports,
                Err(e) => {
                    self.message = e;
                    vec![]
                }
            },
            None => vec![],
        };
        let index = match self.port_state.selected() {
            _ if self.ports.is_empty() => None,
            Some(i) => Some(i.min(self.ports.len() - 1)),
            None => Some(0),
        };
        self.port_state.select(index);
    }

    fn move_selection(&mut self, delta: isize) {
        let (state, len) = match self.focus {
            Focus::Hubs => (&mut self.hub_state, self.hubs.len()),
            Focus::Ports => (&mut self.port_state, self.ports.len()),
        };
        if len == 0 {
            return;
        }
        let current = state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len as isize - 1) as usize;
        if state.selected() != Some(next) {
            state.select(Some(next));
            if self.focus == Focus::Hubs {
                self.port_state.select(Some(0));
                self.refresh_ports();
            }
        }
    }

    fn toggle_power(&mut self) {
        let Some(row) = self.port_state.selected().and_then(|i| self.ports.get(i)) else {
            return;
        };
        let on = !row.any_powered();
        self.message = match switch_power(row, on, &self.info_map) {
            Ok(()) => format!("Port {} {}", row.port, if on { "on" } else { "off" }),
            Err(e) => format!("Failed to switch port {}, {}", row.port, e),
        };
        self.refresh_ports();
    }

    /// Handle a key, returns false to quit
    fn handle_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Hubs,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter
                if self.focus == Focus::Hubs && !self.ports.is_empty() =>
            {
                self.focus = Focus::Ports;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Hubs if !self.ports.is_empty() => Focus::Ports,
                    _ => Focus::Hubs,
                }
            }
            KeyCode::Char(' ') | KeyCode::Enter if self.focus == Focus::Ports => {
                self.toggle_power()
            }
            KeyCode::Char('r') => self.reload(),
            _ => {}
        }
        true
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [main, status] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());

        let hub_width = self
            .hubs
            .iter()
            .map(|hub| hub.label.len() + hub.vid_pid.len() + 1)
            .max()
            .unwrap_or(10) as u16
            + 4;
        let [hubs_area, ports_area, power_area, device_area] = Layout::horizontal([
            Constraint::Max(hub_width),
            Constraint::Length(10),
            Constraint::Length(15),
            Constraint::Min(20),
        ])
        .areas(main);

        let block = |title: &str, focused: bool| {
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .title(format!(" {} ", title));
            if focused {
                block.border_style(Style::new().fg(Color::Cyan))
            } else {
                block
            }
        };
        let highlight = |focused: bool| {
            if focused {
                Style::new().bg(Color::Cyan).fg(Color::Black)
            } else {
                Style::new().add_modifier(Modifier::REVERSED)
            }
        };

        let hub_items = self.hubs.iter().map(|hub| {
            let style = if hub.switching == LogicalPowerSwitchingMode::IndividualPort {
                Style::new()
            } else {
                Style::new().dim()
            };
            ListItem::new(Line::from(vec![
                Span::raw(hub.label.clone()),
                Span::raw(" "),
                Span::raw(hub.vid_pid.clone()).dark_gray(),
            ]))
            .style(style)
        });
        let hubs_focused = self.focus == Focus::Hubs;
        frame.render_stateful_widget(
            List::new(hub_items)
                .block(block("Hubs", hubs_focused))
                .highlight_style(highlight(hubs_focused)),
            hubs_area,
            &mut self.hub_state,
        );

        // The three port columns share the selection, so their rows stay aligned
        let ports_focused = self.focus == Focus::Ports;
        let port_items = self
            .ports
            .iter()
            .map(|row| ListItem::new(format!("Port {}", row.port)));
        let power_items = self.ports.iter().map(|row| {
            let mut spans = vec![];
            for (i, half) in row.halves.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw(" / "));
                }
                spans.push(match half.powered {
                    Some(true) => Span::raw("on").green(),
                    Some(false) => Span::raw("off").red(),
                    None => Span::raw("?").yellow(),
                });
                if half.overcurrent {
                    spans.push(Span::raw(" OC").red().bold());
                }
            }
            ListItem::new(Line::from(spans))
        });
        let device_items = self.ports.iter().map(|row| match row.device() {
            Some(info) => ListItem::new(describe_device(info)),
            None => ListItem::new(Span::raw("-").dark_gray()),
        });

        let mut state = self.port_state.clone();
        frame.render_stateful_widget(
            List::new(port_items)
                .block(block("Ports", ports_focused))
                .highlight_style(highlight(ports_focused)),
            ports_area,
            &mut state,
        );
        let mut state = self.port_state.clone();
        let power_title = if self.ports.iter().any(|row| row.halves.len() > 1) {
            "USB2 / USB3"
        } else {
            "Power"
        };
        frame.render_stateful_widget(
            List::new(power_items)
                .block(block(power_title, ports_focused))
                .highlight_style(highlight(ports_focused)),
            power_area,
            &mut state,
        );
        frame.render_stateful_widget(
            List::new(device_items)
                .block(block("Device", ports_focused))
                .highlight_style(highlight(ports_focused)),
            device_area,
            &mut self.port_state,
        );

        let help = "↑↓ select  ←→/tab focus  space toggle power  r reload  q quit";
        let line = if self.message.is_empty() {
            Line::from(Span::raw(help).dark_gray())
        } else {
            Line::from(vec![
                Span::raw(self.message.clone()).yellow(),
                Span::raw("  "),
                Span::raw(help).dark_gray(),
            ])
        };
        frame.render_widget(Paragraph::new(line), status);
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> std::io::Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            let timeout = REFRESH_INTERVAL.saturating_sub(self.last_refresh.elapsed());
            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press
                    && !self.handle_key(key.code)
                {
                    return Ok(());
                }
            } else {
                self.refresh_ports();
            }
        }
    }
}

fn describe_device(info: &DeviceInfo) -> String {
    let mut parts = vec![
        format!("{:03}:{:03}", info.busnum(), info.device_address()),
        format!("{:04x}:{:04x}", info.vendor_id(), info.product_id()),
    ];
    if info.class() == DEVICE_CLASS_HUB {
        parts.push(format!("hub {}", location(info)));
    }
    parts.extend(
        [
            info.manufacturer_string(),
            info.product_string(),
            info.serial_number(),
        ]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .map(str::to_string),
    );
    if let Some(speed) = info.speed() {
        parts.push(format!("({:?})", speed));
    }
    parts.join(" ")
}

fn main() -> std::io::Result<()> {
    env_logger::init();
    let app = App::new();
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

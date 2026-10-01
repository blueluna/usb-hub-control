use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::time::Duration;

use clap::Parser;
use nusb::{DeviceInfo, MaybeFuture, Speed};
use regex::Regex;

use usb_hub_control::{
    DEVICE_CLASS_HUB, DevicePower, Error, Hub, LogicalPowerSwitchingMode,
    OverCurrentProtectionMode, PortStatus,
};

type InfoMap = BTreeMap<Vec<u8>, DeviceInfo>;

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

fn parse_location(location: &str) -> Option<Vec<u8>> {
    let location_regex =
        Regex::new(r"^(?<busnum>[[:digit:]]+)-(?<chain>(?:(?:[[:digit:]]+)[.])*(?:[[:digit:]]+))$")
            .unwrap();
    let captures = location_regex.captures(location)?;
    let mut key = vec![captures["busnum"].parse::<u8>().ok()?];
    for v in captures["chain"].split('.') {
        key.push(v.parse::<u8>().ok()?);
    }
    Some(key)
}

fn is_super_speed(info: &DeviceInfo) -> bool {
    matches!(info.speed(), Some(Speed::Super | Speed::SuperPlus))
}

fn describe_device(info: &DeviceInfo) -> String {
    format!(
        "{:03}:{:03} {:04x}:{:04x} {} {} {}",
        info.busnum(),
        info.device_address(),
        info.vendor_id(),
        info.product_id(),
        info.manufacturer_string().unwrap_or(""),
        info.product_string().unwrap_or(""),
        info.serial_number().unwrap_or("")
    )
}

fn describe_hub_header(info: &DeviceInfo, hub: &Hub) -> String {
    let container_id_str = hub
        .container_id()
        .map(|c| c.0.iter().map(|b| format!("{:02x}", b)).collect::<String>())
        .unwrap_or_default();

    format!(
        "{} {:04x}:{:04x} {:02x} {:02x} {:02x} {:04x} {} {}",
        location(info),
        info.vendor_id(),
        info.product_id(),
        info.class(),
        info.subclass(),
        info.protocol(),
        info.device_version(),
        hub.port_count(),
        container_id_str,
    )
}

fn describe_status(status: &PortStatus) -> String {
    let connection = if status.connection() {
        " connection"
    } else {
        ""
    };
    let enabled = if status.enabled() { " enabled" } else { "" };
    let overcurrent = if status.overcurrent() {
        " overcurrent"
    } else {
        ""
    };
    let powered = if status.powered() { " powered" } else { "" };
    format!(
        "{:04x}{}{}{}{}",
        status.0, connection, enabled, overcurrent, powered
    )
}

/// Status of one half of a port, `connection` is true when a device may be attached
fn port_status(hub: &Hub, port: u8) -> (String, bool) {
    match hub.port_status(port) {
        Ok(status) => (describe_status(&status), status.connection()),
        Err(e) => {
            eprintln!(
                "Port status {} port {} failed, {}",
                location(&hub.info()),
                port,
                e
            );
            ("????".to_string(), true)
        }
    }
}

/// Describe a hub, fused with its other speed half when there is one
fn describe_hub<W: Write>(
    output: &mut W,
    key: &Vec<u8>,
    info_map: &InfoMap,
    visited: &mut BTreeSet<Vec<u8>>,
) -> Result<(), Error> {
    let info = match info_map.get(key) {
        Some(info) => info,
        None => return Ok(()),
    };
    let align = info.port_chain().len().saturating_sub(1) * 2;

    let hub = Hub::from_device_info(info)?;
    visited.insert(key.clone());

    let peer_hub = hub
        .peer_port(1, info_map.values())
        .and_then(|(peer_info, _)| Hub::from_device_info(&peer_info).ok());

    let mut header = describe_hub_header(info, &hub);
    if let Some(peer_hub) = &peer_hub {
        visited.insert(self::key(&peer_hub.info()));
        header.push_str(" + ");
        header.push_str(&describe_hub_header(&peer_hub.info(), peer_hub));
    }
    let _ = writeln!(output, "{}", header);

    for port in 1..=hub.port_count() {
        let (status, connection) = port_status(&hub, port);
        let _ = write!(output, "{:align$} {} {}", "", port, status);

        let mut children = vec![];
        if connection {
            let mut child_key = key.clone();
            child_key.push(port);
            children.push(child_key);
        }

        if let Some(peer_hub) = &peer_hub
            && let Some((peer_info, peer_port)) = hub.peer_port(port, info_map.values())
        {
            let peer_key = self::key(&peer_info);
            let opened;
            let peer = if peer_key == self::key(&peer_hub.info()) {
                peer_hub
            } else {
                opened = Hub::from_device_info(&peer_info)?;
                &opened
            };
            let (status, connection) = port_status(peer, peer_port);
            if peer_port == port {
                let _ = write!(output, " / {}", status);
            } else {
                let _ = write!(output, " / port {} {}", peer_port, status);
            }
            if connection {
                let mut child_key = peer_key;
                child_key.push(peer_port);
                children.push(child_key);
            }
        }

        let mut devices = vec![];
        let mut child_hub = None;
        for child_key in children {
            if let Some(child) = info_map.get(&child_key) {
                if child.class() != DEVICE_CLASS_HUB {
                    devices.push(describe_device(child));
                } else if child_hub.is_none() && !visited.contains(&child_key) {
                    child_hub = Some(child_key);
                }
            }
        }
        for device in devices {
            let _ = write!(output, " {}", device);
        }
        match child_hub {
            Some(child_key) => {
                let _ = write!(output, " ");
                describe_hub(output, &child_key, info_map, visited)?;
            }
            None => {
                let _ = writeln!(output);
            }
        }
    }
    Ok(())
}

fn matches_device(info: &DeviceInfo, pattern: &str) -> bool {
    let id = format!("{:04x}:{:04x}", info.vendor_id(), info.product_id());
    if id.eq_ignore_ascii_case(pattern) {
        return true;
    }
    let pattern = pattern.to_lowercase();
    [
        info.manufacturer_string(),
        info.product_string(),
        info.serial_number(),
    ]
    .iter()
    .flatten()
    .any(|s| s.to_lowercase().contains(&pattern))
}

/// Print hub location and port of devices matching `pattern`
fn find(info_map: &InfoMap, pattern: &str) -> Result<(), String> {
    let mut found = false;
    for info in info_map.values() {
        if !matches_device(info, pattern) {
            continue;
        }
        let Some((port, hub_chain)) = info.port_chain().split_last() else {
            continue;
        };
        if hub_chain.is_empty() {
            eprintln!(
                "{} is on a root hub port, which can not be switched",
                describe_device(info)
            );
            continue;
        }
        let mut hub_key = vec![info.busnum()];
        hub_key.extend(hub_chain);
        let hub_location = info_map
            .get(&hub_key)
            .map(location)
            .ok_or("Hub not found")?;
        println!("-l {} -p {}  {}", hub_location, port, describe_device(info));
        found = true;
    }
    if found {
        Ok(())
    } else {
        Err(format!("No device matching {}", pattern))
    }
}

fn describe_hub_power(hub: &Hub) -> String {
    let descriptor = hub.descriptor();
    let mut parts = vec![];

    match hub.self_powered() {
        Ok(true) => parts.push("self-powered".to_string()),
        Ok(false) => parts.push("bus-powered".to_string()),
        Err(e) => parts.push(format!("power source unknown ({})", e)),
    }
    parts.push(format!(
        "power switching {}",
        match descriptor.logical_power_switching_mode() {
            LogicalPowerSwitchingMode::IndividualPort => "individual",
            LogicalPowerSwitchingMode::Common => "ganged",
            LogicalPowerSwitchingMode::None => "none",
        }
    ));
    parts.push(format!(
        "over-current protection {}",
        match descriptor.over_current_protection_mode() {
            OverCurrentProtectionMode::IndividualPort => "individual",
            OverCurrentProtectionMode::Global => "global",
            OverCurrentProtectionMode::None => "none",
        }
    ));
    parts.push(format!(
        "power on to power good {} ms",
        descriptor.power_on_to_power_good_ms()
    ));
    if !hub.is_super_speed() {
        parts.push(format!(
            "hub controller {} mA",
            descriptor.hub_controller_current()
        ));
    }
    if let Ok(power) = DevicePower::from_device_info(&hub.info()) {
        parts.push(format!("bMaxPower {} mA", power.max_power_ma));
    }
    if descriptor.compound_device() {
        parts.push("compound device".to_string());
    }
    parts.join(", ")
}

/// Power state of one half of a port
struct PortPower {
    powered: Option<bool>,
    over_current: String,
    budget: u16,
    device: Option<DeviceInfo>,
}

fn port_power(hub: &Hub, port: u8, self_powered: bool, info_map: &InfoMap) -> PortPower {
    let (powered, over_current, connection) = match hub.port_status_change(port) {
        Ok((status, change)) => {
            let mut over_current = if status.overcurrent() {
                "OVER-CURRENT".to_string()
            } else {
                "ok".to_string()
            };
            if change.overcurrent() {
                over_current.push_str(" changed");
            }
            (Some(status.powered()), over_current, status.connection())
        }
        Err(e) => (None, format!("? ({})", e), false),
    };
    let over_current = match hub.over_current_count(port) {
        Some(count) if count > 0 => format!("{} ({}x)", over_current, count),
        _ => over_current,
    };
    let device = if connection {
        let mut child_key = key(&hub.info());
        child_key.push(port);
        info_map.get(&child_key).cloned()
    } else {
        None
    };
    PortPower {
        powered,
        over_current,
        budget: hub.port_current_budget(self_powered),
        device,
    }
}

/// Show power related information for all ports on a hub, fused with its other speed half
fn power_info(info_map: &InfoMap, hub_location: &str) -> Result<(), String> {
    let key = parse_location(hub_location).ok_or("Invalid location")?;
    let info = info_map.get(&key).ok_or("Hub not found")?;
    let hub = Hub::from_device_info(info).map_err(|e| e.to_string())?;
    let peer_hub = hub
        .peer_port(1, info_map.values())
        .and_then(|(peer_info, _)| Hub::from_device_info(&peer_info).ok());

    let mut hubs = vec![&hub];
    hubs.extend(peer_hub.as_ref());
    hubs.sort_by_key(|hub| hub.is_super_speed());

    for hub in &hubs {
        let info = hub.info();
        println!(
            "{} {:04x}:{:04x} USB {}: {}",
            location(&info),
            info.vendor_id(),
            info.product_id(),
            if hub.is_super_speed() { 3 } else { 2 },
            describe_hub_power(hub)
        );
    }
    println!();
    println!(
        "{:<4}  {:<9}  {:<13}  {:<20}  {:<20}  device",
        "port", "power", "budget", "over-current", "bMaxPower"
    );

    let self_powered = |hub: &Hub| hub.self_powered().unwrap_or(false);
    let mut total = 0;
    for port in 1..=hub.port_count() {
        let mut halves = vec![(port_power(&hub, port, self_powered(&hub), info_map), &hub)];
        if let Some((peer_info, peer_port)) = hub.peer_port(port, info_map.values()) {
            let peer = Hub::from_device_info(&peer_info).map_err(|e| e.to_string())?;
            halves.push((
                port_power(&peer, peer_port, self_powered(&peer), info_map),
                peer_hub.as_ref().unwrap_or(&hub),
            ));
        }
        halves.sort_by_key(|(_, hub)| hub.is_super_speed());

        let join = |f: &dyn Fn(&PortPower) -> String| {
            halves
                .iter()
                .map(|(half, _)| f(half))
                .collect::<Vec<_>>()
                .join(" / ")
        };
        let power = join(&|half| match half.powered {
            Some(true) => "on".to_string(),
            Some(false) => "off".to_string(),
            None => "?".to_string(),
        });
        let budget = format!("{} mA", join(&|half| half.budget.to_string()));
        let over_current = join(&|half| half.over_current.clone());

        let mut max_power = String::new();
        let mut device = String::new();
        for (half, _) in &halves {
            let Some(info) = &half.device else {
                continue;
            };
            match DevicePower::from_device_info(info) {
                Ok(power) => {
                    total += power.max_power_ma;
                    max_power = format!("{} mA", power.max_power_ma);
                    if power.self_powered {
                        max_power.push_str(" self");
                    }
                    if power.max_power_ma > half.budget {
                        max_power.push_str(" OVER BUDGET");
                    }
                }
                Err(e) => max_power = format!("? ({})", e),
            }
            device = describe_device(info);
        }

        println!(
            "{:<4}  {:<9}  {:<13}  {:<20}  {:<20}  {}",
            port, power, budget, over_current, max_power, device
        );
    }
    println!();
    println!("Devices on this hub request {} mA in total", total);
    Ok(())
}

fn list(info_map: &InfoMap) -> Result<(), Error> {
    let mut buffer = Vec::new();
    let mut visited = BTreeSet::new();
    // USB 2 halves first, so fused hubs are listed by their USB 2 location
    for super_speed in [false, true] {
        for (key, info) in info_map.iter() {
            if key.len() == 2
                && info.class() == DEVICE_CLASS_HUB
                && is_super_speed(info) == super_speed
                && !visited.contains(key)
            {
                describe_hub(&mut buffer, key, info_map, &mut visited)?;
            }
        }
    }
    let output = std::str::from_utf8(buffer.as_slice()).unwrap().to_string();
    println!("{}", output);
    Ok(())
}

/// List hubs only, with the USB 2 and USB 3 halves of a hub fused
fn list_hubs(info_map: &InfoMap) -> Result<(), Error> {
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
            let hub = Hub::from_device_info(info)?;
            visited.insert(key.clone());

            let mut line = format!(
                "{:align$}{}",
                "",
                describe_hub_header(info, &hub),
                align = (key.len() - 2) * 2
            );
            if let Some((peer_info, _)) = hub.peer_port(1, info_map.values()) {
                let peer = Hub::from_device_info(&peer_info)?;
                visited.insert(self::key(&peer_info));
                line.push_str(" + ");
                line.push_str(&describe_hub_header(&peer_info, &peer));
            }
            let switching = match hub.descriptor().logical_power_switching_mode() {
                LogicalPowerSwitchingMode::IndividualPort => "individual",
                LogicalPowerSwitchingMode::Common => "ganged",
                LogicalPowerSwitchingMode::None => "none",
            };
            println!("{} {}", line.trim_end(), switching);
        }
    }
    Ok(())
}

/// Switch port power on a hub port, and on its peer port unless `single`
fn switch_power(
    info_map: &InfoMap,
    hub_location: &str,
    port: u8,
    on: bool,
    single: bool,
) -> Result<(), String> {
    let key = parse_location(hub_location).ok_or("Invalid location")?;
    let info = info_map.get(&key).ok_or("Hub not found")?;
    let hub = Hub::from_device_info(info).map_err(|e| e.to_string())?;

    let mut halves = vec![(hub, port)];
    if !single && let Some((peer_info, peer_port)) = halves[0].0.peer_port(port, info_map.values())
    {
        let peer = Hub::from_device_info(&peer_info).map_err(|e| e.to_string())?;
        halves.push((peer, peer_port));
    }

    // VBUS only drops once both halves are off. Switch the USB 2 half off first and on last,
    // so the device does not fall back to USB 2 while the SuperSpeed half is still on.
    halves.sort_by_key(|(hub, _)| hub.is_super_speed() != on);

    for (hub, port) in halves {
        println!(
            "{} port {} {}",
            location(&hub.info()),
            port,
            if on { "on" } else { "off" }
        );
        hub.set_port_power(port, on)
            .map_err(|e| format!("Failed to switch port, {}", e))?;
    }
    Ok(())
}

/// USB hub port power control
#[derive(clap::Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// List hubs and ports, with the USB 2 and USB 3 halves of a hub fused
    List,
    /// List hubs only, without their ports
    ListHub,
    /// Find the hub location and port of devices by vid:pid, or by text in manufacturer,
    /// product or serial
    Find { pattern: String },
    /// Show power related information for all ports on a hub
    PowerInfo {
        #[arg(short, long)]
        location: String,
    },
    /// Switch port power off, or on with --on
    Power {
        #[arg(short, long)]
        port: u8,

        #[arg(short, long)]
        on: bool,

        #[arg(short, long)]
        location: String,

        /// Only switch the given hub, not the peer port on the other USB 2/3 half
        #[arg(short, long)]
        single: bool,
    },
    /// Switch port power off and on again
    Cycle {
        #[arg(short, long)]
        port: u8,

        #[arg(short, long)]
        location: String,

        /// Only switch the given hub, not the peer port on the other USB 2/3 half
        #[arg(short, long)]
        single: bool,

        /// Time in milliseconds to keep the port off
        #[arg(short, long, default_value_t = 2000)]
        delay: u64,
    },
}

fn main() {
    env_logger::init();
    let args = Args::parse();

    let device_iter = nusb::list_devices().wait().unwrap();
    let info_map = device_iter
        .map(|info| (key(&info), info))
        .collect::<InfoMap>();

    let result = match args.command {
        Some(Commands::Power {
            port,
            on,
            location,
            single,
        }) => switch_power(&info_map, &location, port, on, single),
        Some(Commands::Cycle {
            port,
            location,
            single,
            delay,
        }) => switch_power(&info_map, &location, port, false, single).and_then(|()| {
            std::thread::sleep(Duration::from_millis(delay));
            switch_power(&info_map, &location, port, true, single)
        }),
        Some(Commands::Find { pattern }) => find(&info_map, &pattern),
        Some(Commands::PowerInfo { location }) => power_info(&info_map, &location),
        Some(Commands::ListHub) => list_hubs(&info_map).map_err(|e| format!("List failed, {}", e)),
        Some(Commands::List) | None => list(&info_map).map_err(|e| format!("List failed, {}", e)),
    };
    if let Err(e) = result {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

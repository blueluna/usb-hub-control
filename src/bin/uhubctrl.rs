use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::time::Duration;

use clap::Parser;
use nusb::{DeviceInfo, MaybeFuture, Speed};
use regex::Regex;

use usb_hub_control::{DEVICE_CLASS_HUB, Error, Hub, PortStatus};

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
    /// Find the hub location and port of devices by vid:pid, or by text in manufacturer,
    /// product or serial
    Find { pattern: String },
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
        Some(Commands::List) | None => list(&info_map).map_err(|e| format!("List failed, {}", e)),
    };
    if let Err(e) = result {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}

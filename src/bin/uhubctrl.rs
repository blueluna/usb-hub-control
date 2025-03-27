use std::collections::BTreeMap;
use std::io::Write;

use clap::Parser;
use nusb::MaybeFuture;
use regex::Regex;

use usb_hub_control::{Error, Hub};

const DEVICE_CLASS_HUB: u8 = 0x09;

fn describe_device<W: Write>(
    output: &mut W,
    key: &Vec<u8>,
    info_map: &BTreeMap<Vec<u8>, nusb::DeviceInfo>,
) -> Result<(), nusb::Error> {
    let info = match info_map.get(key) {
        Some(info) => info,
        None => return Ok(()),
    };
    let _ = write!(
        output,
        "{:03}:{:03} {:04x}:{:04x} {} {} {}",
        info.busnum(),
        info.device_address(),
        info.vendor_id(),
        info.product_id(),
        info.manufacturer_string().unwrap_or(""),
        info.product_string().unwrap_or(""),
        info.serial_number().unwrap_or("")
    );
    Ok(())
}

fn describe_hub<W: Write>(
    output: &mut W,
    key: &Vec<u8>,
    info_map: &BTreeMap<Vec<u8>, nusb::DeviceInfo>,
) -> Result<(), Error> {
    let info = match info_map.get(key) {
        Some(info) => info,
        None => return Ok(()),
    };
    let align = info.port_chain().len().saturating_sub(1) * 2;

    let hub = Hub::from_device_info(info)?;

    let key_string = format!(
        "{}.{}",
        info.busnum(),
        info.port_chain()
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<String>>()
            .join("-")
    );

    let container_id_str = if let Some(c) = hub.container_id() {
        let c = c.0;
        format!(
            "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            c[0],
            c[1],
            c[2],
            c[3],
            c[4],
            c[5],
            c[6],
            c[7],
            c[8],
            c[9],
            c[10],
            c[11],
            c[12],
            c[13],
            c[14],
            c[15]
        )
    } else {
        String::new()
    };

    let _ = writeln!(
        output,
        "{} {:04x}:{:04x} {:02x} {:02x} {:02x} {:04x} {} {}",
        key_string,
        info.vendor_id(),
        info.product_id(),
        info.class(),
        info.subclass(),
        info.protocol(),
        info.device_version(),
        hub.port_count(),
        container_id_str,
    );

    for port in 1..=hub.port_count() {
        let mut port_key = key.clone();
        port_key.push(port);
        let connection = match hub.port_status(port) {
            Ok(status) => {
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
                let _ = write!(
                    output,
                    "{:align$} {} {:04x}{}{}{}{} ",
                    "", port, status.0, connection, enabled, overcurrent, powered
                );
                status.connection()
            }
            Err(e) => {
                eprintln!("Port status {} failed, {}", port, e);
                true
            }
        };
        if connection {
            match info_map.get(&port_key) {
                Some(device_info) => {
                    if device_info.class() != DEVICE_CLASS_HUB {
                        describe_device(output, &port_key, info_map)?;
                        let _ = writeln!(output);
                    } else {
                        describe_hub(output, &port_key, info_map)?;
                    }
                }
                None => {
                    let _ = writeln!(output);
                }
            }
        } else {
            let _ = writeln!(output);
        }
    }
    Ok(())
}

fn list(info_map: &BTreeMap<Vec<u8>, nusb::DeviceInfo>) -> Result<(), Error> {
    let mut buffer = Vec::new();
    for (key, info) in info_map.iter() {
        if key.len() == 2 && info.class() == DEVICE_CLASS_HUB {
            describe_hub(&mut buffer, key, info_map)?;
        }
    }
    let output = std::str::from_utf8(buffer.as_slice()).unwrap().to_string();
    println!("{}", output);
    Ok(())
}

/// Simple program to greet a person
#[derive(clap::Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    List,
    Power {
        #[arg(short, long)]
        port: u8,

        #[arg(short, long)]
        on: bool,

        #[arg(short, long)]
        location: Option<String>,
    },
}

fn main() {
    env_logger::init();
    let args = Args::parse();

    let device_iter = nusb::list_devices().wait().unwrap();
    let mut info_map = BTreeMap::new();
    for info in device_iter {
        let bus_num = info.busnum();
        let mut key = vec![bus_num];
        key.extend(info.port_chain());
        info_map.insert(key, info);
    }

    match args.command {
        Some(Commands::Power { port, on, location }) => {
            let location_regex = Regex::new(
                r"^(?<busnum>[[:digit:]]+)-(?<chain>(?:(?:[[:digit:]]+)[.])*(?:[[:digit:]]+))$",
            )
            .unwrap();
            let key = if let Some(location) = location {
                if let Some(captures) = location_regex.captures(location.as_str()) {
                    if let (Some(b), Some(c)) = (captures.name("busnum"), captures.name("chain")) {
                        let busnum = b.as_str().parse::<u8>().unwrap();
                        let chain = c
                            .as_str()
                            .split('.')
                            .filter_map(|v| v.parse::<u8>().ok())
                            .collect::<Vec<u8>>();
                        let mut key = vec![busnum];
                        key.extend(chain);
                        Some(key)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(k) = key {
                if let Some(info) = info_map.get(&k) {
                    let hub = Hub::from_device_info(info).unwrap();
                    println!(
                        "PORT {} {} KEY {:?} {:02x} {:02x}",
                        port,
                        if on { "on" } else { "off" },
                        k,
                        info.busnum(),
                        info.device_address()
                    );
                    if let Err(e) = hub.set_port_power(port, on) {
                        eprint!("Failed to switch port, {}", e);
                    }
                }
            }
        }
        _ => match list(&info_map) {
            Ok(()) => (),
            Err(ref e) => {
                eprintln!("List failed, {}", e);
            }
        },
    }
}

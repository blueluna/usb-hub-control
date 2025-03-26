use std::time::Duration;
use std::collections::BTreeMap;

use nusb::MaybeFuture;
use nusb::transfer::{ControlType, Recipient, Control};

const DEVICE_CLASS_HUB: u8 = 0x09;
const USB_VERSION_3_0: u16 = 0x0300;
const DESCRIPTOR_TYPE_HUB: u8 = 0x29;
const DESCRIPTOR_TYPE_SUPERSPEED_HUB: u8 = 0x2a;

const HUB_CHARACTERISTICS_LPSM_MASK: u16 = 0x0003;
const HUB_CHARACTERISTICS_LPSM_COMMON: u16 = 0x0000;
const HUB_CHARACTERISTICS_LPSM_INDIVIDUAL_PORT: u16 = 0x0001;
const HUB_CHARACTERISTICS_LPSM_NO: u16 = 0x0002;

const PORT_STATUS_CONNECTION: u16 = 0x0001;
const PORT_STATUS_ENABLE: u16 = 0x0002;
// const PORT_STATUS_SUSPEND: u16 = 0x0004;
// const PORT_STATUS_OVERCURRENT: u16 = 0x0008;
// const PORT_STATUS_RESET: u16 = 0x0010;
// const PORT_STATUS_L1: u16 = 0x0020;

fn get_port_status(device: &nusb::Device, port: u8)-> Result<(u16, u16), nusb::Error>
{
    const STANDARD_REQUEST_GET_STATUS: u8 = 0x00;

    let mut buf = vec![0; 4];
    let len = device.control_in_blocking(
        Control {
            control_type: ControlType::Class,
            recipient: Recipient::Other,
            request: STANDARD_REQUEST_GET_STATUS,
            value: 0,
            index: (port as u16),
        },
        &mut buf,
        Duration::from_secs(5),
    )?;
    buf.truncate(len);
    if len == 4 {
        let port_status = u16::from_le_bytes(buf[0..=1].try_into().unwrap());
        let port_change = u16::from_le_bytes(buf[2..=3].try_into().unwrap());
        Ok((port_status, port_change))
    }
    else {
        Ok((0, 0))
    }
}

fn list() -> Result<(), nusb::Error>
{
    let device_iter = nusb::list_devices().wait()?;
    let mut devices: Vec<nusb::DeviceInfo> = device_iter.collect();
    devices.sort_by_key(|v| {
        (v.busnum(), v.device_address())
    });
    let mut device_tree = BTreeMap::new();
    for info in devices {
        let port_chain = info.port_chain().iter().map(|v| v.to_string()).collect::<Vec<String>>().join("-");
        let key = format!("{}-{}", info.busnum(), port_chain);
        device_tree.insert(key, info);
    }
    for (key, info) in device_tree.iter() {
        let align = info.port_chain().len() * 2;
        if info.class() == DEVICE_CLASS_HUB {
            let device = info.open().wait()?;
            let descriptor = device.device_descriptor();
            let descriptor_type = if descriptor.usb_version() > USB_VERSION_3_0 { DESCRIPTOR_TYPE_SUPERSPEED_HUB } else { DESCRIPTOR_TYPE_HUB };
            let port_count = {
                const STANDARD_REQUEST_GET_DESCRIPTOR: u8 = 0x06;

                let mut buf = vec![0; 4096];
                let len = device.control_in_blocking(
                    Control {
                        control_type: ControlType::Class,
                        recipient: Recipient::Device,
                        request: STANDARD_REQUEST_GET_DESCRIPTOR,
                        value: ((descriptor_type as u16) << 8),
                        index: 0,
                    },
                    &mut buf,
                    Duration::from_secs(5),
                )?;

                buf.truncate(len);
                
                if len >= 9 {
                    let hub_characteristics = u16::from_le_bytes(buf[3..=4].try_into().unwrap());
                    let port_count = buf[2];

                    let logical_power_switching_mode = hub_characteristics & HUB_CHARACTERISTICS_LPSM_MASK;
                    let p_mode = match logical_power_switching_mode {
                        HUB_CHARACTERISTICS_LPSM_INDIVIDUAL_PORT => "port",
                        HUB_CHARACTERISTICS_LPSM_COMMON => "common",
                        HUB_CHARACTERISTICS_LPSM_NO => "disabled",
                        _ => "invalid",
                    };

                    println!("{:align$} {} {:03}:{:03} {:04x}:{:04x} ports {} {:04x} {}", "", key, info.busnum(), info.device_address(), info.vendor_id(), info.product_id(), port_count, hub_characteristics,  p_mode);
                    if logical_power_switching_mode == HUB_CHARACTERISTICS_LPSM_INDIVIDUAL_PORT {
                        port_count
                    }
                    else {
                        0
                    }
                }
                else {
                    println!("{}",len);
                    0
                }
            };
            if port_count > 0 {
                for port in 1..=port_count {
                    match get_port_status(&device, port) {
                        Ok((status, _)) => {
                            let connection = if (status & PORT_STATUS_CONNECTION) == PORT_STATUS_CONNECTION {
                                "connection"
                            } else { "" };
                            let enabled = if (status & PORT_STATUS_ENABLE) == PORT_STATUS_ENABLE {
                                "enabled"
                            } else { "" };
                            println!("{:align$}     {} {:04x} {} {}", "", port, status, connection, enabled);
                        }
                        Err(e) => {
                            eprintln!("Port status {} failed, {}", port, e);
                        },
                    }
                }
            }
        }
        else {
            println!("{:align$} {} {:03}:{:03} {:04x}:{:04x} {} {}", "", key, info.busnum(), info.device_address(), info.vendor_id(), info.product_id(), info.manufacturer_string().unwrap_or(""), info.product_string().unwrap_or(""));
        }
    }
    Ok(())
}

fn main() {
    env_logger::init();
    
    match list() {
        Ok(()) => (),
        Err(ref e) => {
            eprintln!("List failed, {}", e);
        }
    }
}
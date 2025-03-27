use std::hash::Hash;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{Control, ControlType, Recipient};
use nusb::{Device, DeviceInfo};

mod error;

pub use error::Error;

pub const DEVICE_CLASS_HUB: u8 = 0x09;
pub const USB_VERSION_3_0: u16 = 0x0300;

pub const DESCRIPTOR_TYPE_HUB: u8 = 0x29;
pub const DESCRIPTOR_TYPE_SUPERSPEED_HUB: u8 = 0x2a;

// USB hub port status
pub const PORT_STATUS_CONNECTION: u16 = 0x0001;
pub const PORT_STATUS_ENABLE: u16 = 0x0002;
pub const PORT_STATUS_SUSPEND: u16 = 0x0004;
pub const PORT_STATUS_OVERCURRENT: u16 = 0x0008;
pub const PORT_STATUS_RESET: u16 = 0x0010;
pub const PORT_STATUS_L1: u16 = 0x0020;

pub struct Hub {
    info: DeviceInfo,
    device: Device,
    hub_descriptor: HubDescriptor,
}

impl Hub {
    pub fn from_device_info(info: &DeviceInfo) -> Result<Self, Error> {
        let device = info.open().wait()?;
        let hub_descriptor = Self::get_hub_description(&device)?;
        if info.class() != DEVICE_CLASS_HUB {
            Err(Error::InvalidDeviceClass)
        } else {
            Ok(Self {
                info: info.clone(),
                device,
                hub_descriptor,
            })
        }
    }

    fn get_hub_description(device: &Device) -> Result<HubDescriptor, Error> {
        const STANDARD_REQUEST_GET_DESCRIPTOR: u8 = 0x06;

        const DESCRIPTOR_TYPE_HUB: u8 = 0x29;
        const DESCRIPTOR_TYPE_SUPERSPEED_HUB: u8 = 0x2a;

        let descriptor = device.device_descriptor();
        let (descriptor_type, request_size) = if descriptor.usb_version() > USB_VERSION_3_0 {
            (DESCRIPTOR_TYPE_SUPERSPEED_HUB, 12)
        } else {
            (DESCRIPTOR_TYPE_HUB, 9)
        };
        let mut buf = vec![0; request_size];
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

        if len != request_size {
            return Err(Error::InvalidRespone);
        }
        buf.truncate(len);

        let port_count = if buf[2] <= 15 { buf[2] } else { 0 };
        let characteristics = u16::from_le_bytes(buf[3..=4].try_into().unwrap());

        Ok(HubDescriptor {
            port_count,
            characteristics,
        })
    }

    pub fn info(&self) -> DeviceInfo {
        self.info.clone()
    }

    pub fn port_count(&self) -> u8 {
        self.hub_descriptor.port_count()
    }

    pub fn port_status(&self, port: u8) -> Result<PortStatus, Error> {
        const STANDARD_REQUEST_GET_STATUS: u8 = 0x00;

        if port > self.hub_descriptor.port_count() {
            return Err(Error::InvalidPort);
        }

        let mut buf = vec![0; 4];
        let len = self.device.control_in_blocking(
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
        if len == 4 {
            let port_status = u16::from_le_bytes(buf[0..=1].try_into().unwrap());
            let _port_change = u16::from_le_bytes(buf[2..=3].try_into().unwrap());
            Ok(PortStatus(port_status))
        } else {
            Err(Error::UsbTransferError(
                nusb::transfer::TransferError::Fault,
            ))
        }
    }

    pub fn set_port_power(&self, port: u8, on: bool) -> Result<(), Error> {
        if self.hub_descriptor.logical_power_switching_mode()
            != LogicalPowerSwitchingMode::IndividualPort
        {
            return Err(Error::InvalidPort);
        }
        if port > self.hub_descriptor.port_count() {
            return Err(Error::InvalidPort);
        }

        const STANDARD_REQUEST_CLEAR_FEATURE: u8 = 0x01;
        const STANDARD_REQUEST_SET_FEATURE: u8 = 0x03;
        const USB_PORT_FEATURE_POWER: u16 = 0x0008;

        let request = if on {
            STANDARD_REQUEST_SET_FEATURE
        } else {
            STANDARD_REQUEST_CLEAR_FEATURE
        };

        let buf = vec![];
        let _ = self.device.control_out_blocking(
            Control {
                control_type: ControlType::Class,
                recipient: Recipient::Other,
                request: request,
                value: USB_PORT_FEATURE_POWER,
                index: (port as u16),
            },
            &buf,
            Duration::from_secs(5),
        )?;
        Ok(())
    }
}

impl Hash for Hub {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.info.bus_id().hash(state);
        self.info.busnum().hash(state);
        self.info.port_chain().hash(state);
    }
}

pub struct PortStatus(pub u16);

impl PortStatus {
    pub fn connection(&self) -> bool {
        self.0 & PORT_STATUS_CONNECTION == PORT_STATUS_CONNECTION
    }

    pub fn enabled(&self) -> bool {
        self.0 & PORT_STATUS_ENABLE == PORT_STATUS_ENABLE
    }

    pub fn suspended(&self) -> bool {
        self.0 & PORT_STATUS_SUSPEND == PORT_STATUS_SUSPEND
    }

    pub fn overcurrent(&self) -> bool {
        self.0 & PORT_STATUS_OVERCURRENT == PORT_STATUS_OVERCURRENT
    }

    pub fn reset(&self) -> bool {
        self.0 & PORT_STATUS_RESET == PORT_STATUS_RESET
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum LogicalPowerSwitchingMode {
    None,
    Common,
    IndividualPort,
}

#[derive(Clone, Copy, PartialEq)]
pub struct HubDescriptor {
    port_count: u8,
    characteristics: u16,
}

impl HubDescriptor {
    pub fn port_count(&self) -> u8 {
        self.port_count
    }

    pub fn logical_power_switching_mode(&self) -> LogicalPowerSwitchingMode {
        const HUB_CHARACTERISTICS_LPSM_MASK: u16 = 0x0003;
        const HUB_CHARACTERISTICS_LPSM_COMMON: u16 = 0x0000;
        const HUB_CHARACTERISTICS_LPSM_INDIVIDUAL_PORT: u16 = 0x0001;

        match self.characteristics & HUB_CHARACTERISTICS_LPSM_MASK {
            HUB_CHARACTERISTICS_LPSM_INDIVIDUAL_PORT => LogicalPowerSwitchingMode::IndividualPort,
            HUB_CHARACTERISTICS_LPSM_COMMON => LogicalPowerSwitchingMode::Common,
            _ => LogicalPowerSwitchingMode::None,
        }
    }
}

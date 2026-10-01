#![deny(missing_docs)]

//! USB hub control

use std::hash::Hash;
use std::time::Duration;

use log::trace;
use nusb::MaybeFuture;
use nusb::transfer::{Control, ControlType, Recipient};
use nusb::{Device, DeviceInfo};

mod error;

pub use error::Error;

/// USB version 3.0 code
pub const USB_VERSION_3_0: u16 = 0x0300;

/// USB hub device class
pub const DEVICE_CLASS_HUB: u8 = 0x09;

/// USB hub
pub struct Hub {
    info: DeviceInfo,
    device: Device,
    hub_descriptor: HubDescriptor,
    super_speed: bool,
    container_id: Option<ContainerId>,
}

impl Hub {
    /// Create a Hub from DeviceInfo
    pub fn from_device_info(info: &DeviceInfo) -> Result<Self, Error> {
        if info.class() != DEVICE_CLASS_HUB {
            Err(Error::InvalidDeviceClass)
        } else {
            let device = info.open().wait()?;
            let descriptor = device.device_descriptor();
            let super_speed = descriptor.usb_version() >= USB_VERSION_3_0;
            let hub_descriptor = Self::get_hub_description(&device, super_speed)?;

            let lpsm = hub_descriptor.logical_power_switching_mode();
            let lpsm_str = match lpsm {
                LogicalPowerSwitchingMode::Common => "common",
                LogicalPowerSwitchingMode::IndividualPort => "individual",
                _ => "unknown",
            };

            let container_id = match Self::get_bos_description(&device) {
                Ok(bos) => bos.container_id(),
                Err(_) => None,
            };

            trace!(
                "HUB {:02x} {:02x} {:04x} {}",
                info.busnum(),
                info.device_address(),
                descriptor.usb_version(),
                lpsm_str
            );

            Ok(Self {
                info: info.clone(),
                device,
                hub_descriptor,
                super_speed,
                container_id,
            })
        }
    }

    fn get_hub_description(device: &Device, super_speed: bool) -> Result<HubDescriptor, Error> {
        const STANDARD_REQUEST_GET_DESCRIPTOR: u8 = 0x06;

        const DESCRIPTOR_TYPE_HUB: u8 = 0x29;
        const DESCRIPTOR_TYPE_SUPERSPEED_HUB: u8 = 0x2a;

        let (descriptor_type, request_size) = if super_speed {
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
            power_on_to_power_good: buf[5],
            hub_controller_current: buf[6],
        })
    }

    fn get_bos_description(device: &Device) -> Result<BinaryObjectStoreDescriptor, Error> {
        const STANDARD_REQUEST_GET_DESCRIPTOR: u8 = 0x06;

        // Binary device Object Store (BOS)
        const DESCRIPTOR_TYPE_BOS: u8 = 0x0f;

        let mut buf = vec![0; 4096];
        let len = device.control_in_blocking(
            Control {
                control_type: ControlType::Standard,
                recipient: Recipient::Device,
                request: STANDARD_REQUEST_GET_DESCRIPTOR,
                value: ((DESCRIPTOR_TYPE_BOS as u16) << 8),
                index: 0,
            },
            &mut buf,
            Duration::from_secs(5),
        )?;

        buf.truncate(len);

        if len >= 5 {
            Ok(BinaryObjectStoreDescriptor::from_data(&buf))
        } else {
            Err(Error::InvalidRespone)
        }
    }

    /// Get DeviceInfo for Hub
    pub fn info(&self) -> DeviceInfo {
        self.info.clone()
    }

    /// Get Hub port count
    pub fn port_count(&self) -> u8 {
        self.hub_descriptor.port_count()
    }

    /// Get Hub container id
    pub fn container_id(&self) -> Option<ContainerId> {
        self.container_id.clone()
    }

    /// Hub is the SuperSpeed (USB 3) half of a hub
    pub fn is_super_speed(&self) -> bool {
        self.super_speed
    }

    /// Find the port on the other half of the physical hub that shares a connector with `port`.
    ///
    /// A USB 3 hub shows up as two hubs, one USB 2 and one SuperSpeed, and each physical
    /// connector has a port on both. On Linux the kernel's port peer links in sysfs are used.
    /// Otherwise the hub with the same container id and the other speed is used, assuming the
    /// same port number. Not all ports have a peer, some hubs have fewer SuperSpeed ports.
    pub fn peer_port<'a>(
        &self,
        port: u8,
        devices: impl IntoIterator<Item = &'a DeviceInfo> + Clone,
    ) -> Option<(DeviceInfo, u8)> {
        #[cfg(target_os = "linux")]
        if self.sysfs_port_dir(port).is_some() {
            return self.sysfs_peer_port(port, devices);
        }
        self.container_id_peer_port(port, devices)
    }

    /// Sysfs directory of a port, e.g. 5-1.3:1.0/5-1.3-port4
    #[cfg(target_os = "linux")]
    fn sysfs_port_dir(&self, port: u8) -> Option<std::path::PathBuf> {
        let path = self.info.sysfs_path();
        let name = path.file_name()?.to_str()?;
        let port_dir = format!("{}-port{}", name, port);

        std::fs::read_dir(path)
            .ok()?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(':'))
            .map(|entry| entry.path().join(&port_dir))
            .find(|path| path.is_dir())
    }

    #[cfg(target_os = "linux")]
    fn sysfs_peer_port<'a>(
        &self,
        port: u8,
        devices: impl IntoIterator<Item = &'a DeviceInfo>,
    ) -> Option<(DeviceInfo, u8)> {
        let port_dir = self.sysfs_port_dir(port)?;
        let peer = std::fs::read_link(port_dir.join("peer")).ok()?;
        let peer = peer.file_name()?.to_str()?;
        trace!("Port {} peer {}", port_dir.display(), peer);

        let (hub_name, peer_port) = peer.rsplit_once("-port")?;
        let peer_port = peer_port.parse::<u8>().ok()?;
        let (busnum, chain) = if let Some(bus) = hub_name.strip_prefix("usb") {
            (bus.parse::<u8>().ok()?, vec![])
        } else {
            let (bus, chain) = hub_name.split_once('-')?;
            let chain = chain
                .split('.')
                .map(|v| v.parse::<u8>().ok())
                .collect::<Option<Vec<u8>>>()?;
            (bus.parse::<u8>().ok()?, chain)
        };

        devices
            .into_iter()
            .find(|info| info.busnum() == busnum && info.port_chain() == chain.as_slice())
            .map(|info| (info.clone(), peer_port))
    }

    fn container_id_peer_port<'a>(
        &self,
        port: u8,
        devices: impl IntoIterator<Item = &'a DeviceInfo>,
    ) -> Option<(DeviceInfo, u8)> {
        let container_id = self.container_id.as_ref()?;
        let candidates = devices
            .into_iter()
            .filter(|info| {
                info.class() == DEVICE_CLASS_HUB
                    && info.port_chain().len() == self.info.port_chain().len()
                    && !(info.busnum() == self.info.busnum()
                        && info.port_chain() == self.info.port_chain())
            })
            .filter_map(|info| Hub::from_device_info(info).ok())
            .filter(|hub| {
                hub.super_speed != self.super_speed
                    && hub.container_id.as_ref() == Some(container_id)
                    && port <= hub.port_count()
            })
            .map(|hub| hub.info)
            .collect::<Vec<_>>();

        // Hubs built from several hub chips may share a container id, prefer the same chain
        let peer = match candidates
            .iter()
            .find(|info| info.port_chain() == self.info.port_chain())
        {
            Some(info) => info.clone(),
            None if candidates.len() == 1 => candidates[0].clone(),
            None => return None,
        };
        Some((peer, port))
    }

    /// Get Hub port status
    pub fn port_status(&self, port: u8) -> Result<PortStatus, Error> {
        self.port_status_change(port).map(|(status, _)| status)
    }

    /// Get Hub port status and port change
    pub fn port_status_change(&self, port: u8) -> Result<(PortStatus, PortChange), Error> {
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
            let port_change = u16::from_le_bytes(buf[2..=3].try_into().unwrap());
            Ok((
                PortStatus::from_field(port_status, self.super_speed),
                PortChange(port_change),
            ))
        } else {
            Err(Error::UsbTransferError(
                nusb::transfer::TransferError::Fault,
            ))
        }
    }

    /// Get Hub descriptor
    pub fn descriptor(&self) -> HubDescriptor {
        self.hub_descriptor
    }

    /// Hub is currently self-powered, from the device status
    pub fn self_powered(&self) -> Result<bool, Error> {
        const STANDARD_REQUEST_GET_STATUS: u8 = 0x00;
        const DEVICE_STATUS_SELF_POWERED: u16 = 0x0001;

        let mut buf = vec![0; 2];
        let len = self.device.control_in_blocking(
            Control {
                control_type: ControlType::Standard,
                recipient: Recipient::Device,
                request: STANDARD_REQUEST_GET_STATUS,
                value: 0,
                index: 0,
            },
            &mut buf,
            Duration::from_secs(5),
        )?;
        if len != 2 {
            return Err(Error::InvalidRespone);
        }
        Ok(u16::from_le_bytes(buf[0..=1].try_into().unwrap()) & DEVICE_STATUS_SELF_POWERED != 0)
    }

    /// Maximum current in mA a device may draw from a port, as specified for this hub
    pub fn port_current_budget(&self, self_powered: bool) -> u16 {
        match (self.super_speed, self_powered) {
            (false, false) => 100,
            (false, true) => 500,
            (true, false) => 150,
            (true, true) => 900,
        }
    }

    /// Number of over-current events the kernel has seen on a port
    #[cfg(target_os = "linux")]
    pub fn over_current_count(&self, port: u8) -> Option<u32> {
        let count = std::fs::read_to_string(self.sysfs_port_dir(port)?.join("over_current_count"));
        count.ok()?.trim().parse().ok()
    }

    /// Number of over-current events the kernel has seen on a port
    #[cfg(not(target_os = "linux"))]
    pub fn over_current_count(&self, _port: u8) -> Option<u32> {
        None
    }

    /// Set port power
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

        trace!("Set port power {}", if on { "on" } else { "off" });

        let _ = self.device.control_out_blocking(
            Control {
                control_type: ControlType::Class,
                recipient: Recipient::Other,
                request,
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

/// USB port status
pub struct PortStatus(pub u16);

impl PortStatus {
    /// Create port status from field value
    pub fn from_field(value: u16, super_speed: bool) -> Self {
        PortStatus(value | if super_speed { Self::SUPER_SPEED } else { 0 })
    }

    /// This field reflects whether or not a device is currently connected to this port.
    #[inline(always)]
    pub fn connection(&self) -> bool {
        self.0 & Self::CONNECTION == Self::CONNECTION
    }

    /// This field indicates whether the port is enabled.
    #[inline(always)]
    pub fn enabled(&self) -> bool {
        self.0 & Self::ENABLE == Self::ENABLE
    }
    /// This field indicates whether or not the device on this port is suspended.
    /// Setting this field causes the device to suspend by not propagating bus traffic downstream.
    /// This field may be reset by a request or by resume signaling from the device attached to the port.
    #[inline(always)]
    pub fn suspended(&self) -> bool {
        self.0 & Self::SUSPEND == Self::SUSPEND
    }
    /// If the hub reports over-current conditions on a per-port basis,
    /// this field indicates that the current drain on the port exceeds the specified maximum.
    #[inline(always)]
    pub fn overcurrent(&self) -> bool {
        self.0 & Self::OVERCURRENT == Self::OVERCURRENT
    }
    /// This field is set when the host wishes to reset the attached device.
    /// It remains set until the reset signaling is turned off by the hub.
    #[inline(always)]
    pub fn reset(&self) -> bool {
        self.0 & Self::RESET == Self::RESET
    }
    /// This field reflects a ports logical, power control state.
    /// Because hubs can implement different methods of port power switching,
    /// this field may or may not represent whether power is applied to the port.
    /// The device descriptor reports the type of power switching implemented by the hub.
    #[inline(always)]
    pub fn powered(&self) -> bool {
        if self.super_speed() {
            self.0 & Self::SS_POWER == Self::SS_POWER
        } else {
            self.0 & Self::POWER == Self::POWER
        }
    }

    #[inline(always)]
    fn super_speed(&self) -> bool {
        self.0 & Self::SUPER_SPEED == Self::SUPER_SPEED
    }

    // USB hub port status
    const CONNECTION: u16 = 0x0001;
    const ENABLE: u16 = 0x0002;
    const SUSPEND: u16 = 0x0004;
    const OVERCURRENT: u16 = 0x0008;
    const RESET: u16 = 0x0010;
    const POWER: u16 = 0x0100;
    const SS_POWER: u16 = 0x0200;
    // Non-standard extension to the field
    const SUPER_SPEED: u16 = 0x8000;
}

/// Logical Power Switching Mode
#[derive(Clone, Copy, PartialEq)]
pub enum LogicalPowerSwitchingMode {
    /// Unknown
    None,
    /// Ganged power switching (all ports power at once)
    Common,
    /// Individual port power switching
    IndividualPort,
}

/// USB hub descriptor
#[derive(Clone, Copy, PartialEq)]
pub struct HubDescriptor {
    port_count: u8,
    characteristics: u16,
    power_on_to_power_good: u8,
    hub_controller_current: u8,
}

impl HubDescriptor {
    /// Number of USB hub ports
    pub fn port_count(&self) -> u8 {
        self.port_count
    }

    /// Logical Power Switching Mode supported by the hub
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

    /// Over-current Protection Mode of the hub
    pub fn over_current_protection_mode(&self) -> OverCurrentProtectionMode {
        match (self.characteristics >> 3) & 0x0003 {
            0 => OverCurrentProtectionMode::Global,
            1 => OverCurrentProtectionMode::IndividualPort,
            _ => OverCurrentProtectionMode::None,
        }
    }

    /// Hub is part of a compound device
    pub fn compound_device(&self) -> bool {
        self.characteristics & 0x0004 != 0
    }

    /// Time in ms from switching port power on until power is good on the port
    pub fn power_on_to_power_good_ms(&self) -> u16 {
        u16::from(self.power_on_to_power_good) * 2
    }

    /// Maximum current requirement of the hub controller electronics.
    ///
    /// In mA for a USB 2 hub. A SuperSpeed hub reports the raw value, as the unit is not mA.
    pub fn hub_controller_current(&self) -> u8 {
        self.hub_controller_current
    }
}

/// Over-current Protection Mode
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OverCurrentProtectionMode {
    /// Over-current is reported for all ports together
    Global,
    /// Over-current is reported per port
    IndividualPort,
    /// No over-current protection
    None,
}

/// USB port change
pub struct PortChange(pub u16);

impl PortChange {
    /// The over-current state of the port has changed since it was last cleared
    pub fn overcurrent(&self) -> bool {
        self.0 & 0x0008 != 0
    }
}

/// Power information of a device, from its active configuration
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DevicePower {
    /// Maximum current in mA the device draws from the bus (bMaxPower)
    pub max_power_ma: u16,
    /// Device can be self-powered (bmAttributes)
    pub self_powered: bool,
    /// Device supports remote wakeup (bmAttributes)
    pub remote_wakeup: bool,
}

impl DevicePower {
    const ATTRIBUTE_SELF_POWERED: u8 = 0x40;
    const ATTRIBUTE_REMOTE_WAKEUP: u8 = 0x20;

    /// Get power information of a device.
    ///
    /// On Linux this is read from sysfs, which works without access to the device itself.
    pub fn from_device_info(info: &DeviceInfo) -> Result<Self, Error> {
        #[cfg(target_os = "linux")]
        {
            let read = |name: &str| std::fs::read_to_string(info.sysfs_path().join(name));
            if let (Ok(max_power), Ok(attributes)) = (read("bMaxPower"), read("bmAttributes")) {
                // The kernel already scales bMaxPower to mA, e.g. "100mA"
                let max_power_ma = max_power
                    .trim()
                    .trim_end_matches("mA")
                    .parse()
                    .map_err(|_| Error::InvalidRespone)?;
                let attributes =
                    u8::from_str_radix(attributes.trim(), 16).map_err(|_| Error::InvalidRespone)?;
                return Ok(Self::from_attributes(max_power_ma, attributes));
            }
        }

        let device = info.open().wait()?;
        let super_speed = matches!(
            info.speed(),
            Some(nusb::Speed::Super | nusb::Speed::SuperPlus)
        );
        let configuration = device
            .active_configuration()
            .map_err(|_| Error::InvalidRespone)?;
        // bMaxPower is in units of 2 mA, or 8 mA when operating at SuperSpeed
        let unit = if super_speed { 8 } else { 2 };
        Ok(Self::from_attributes(
            u16::from(configuration.max_power()) * unit,
            configuration.attributes(),
        ))
    }

    fn from_attributes(max_power_ma: u16, attributes: u8) -> Self {
        Self {
            max_power_ma,
            self_powered: attributes & Self::ATTRIBUTE_SELF_POWERED != 0,
            remote_wakeup: attributes & Self::ATTRIBUTE_REMOTE_WAKEUP != 0,
        }
    }
}

#[derive(Debug, PartialEq)]
enum DeviceCapabilityType {
    WirelessUsb,
    Usb20Extension,
    SuperSpeedUsb,
    ContainerId,
    Platform,
    PowerDeliveryCapability,
    BatteryInfoCapability,
    PowerDeliveryConsumerPortCapability,
    PowerDeliveryProviderPortCapability,
    SuperSpeedPlus,
    PrecisionTimeMeasurement,
    WirelessUsbExtensions,
    Billboard,
    Authentication,
    BillboardExtensions,
    ConfigurationSummary,
    FwStatus,
    Reserved,
}

impl From<u8> for DeviceCapabilityType {
    fn from(value: u8) -> Self {
        match value {
            0x01 => Self::WirelessUsb,
            0x02 => Self::Usb20Extension,
            0x03 => Self::SuperSpeedUsb,
            0x04 => Self::ContainerId,
            0x05 => Self::Platform,
            0x06 => Self::PowerDeliveryCapability,
            0x07 => Self::BatteryInfoCapability,
            0x08 => Self::PowerDeliveryConsumerPortCapability,
            0x09 => Self::PowerDeliveryProviderPortCapability,
            0x0a => Self::SuperSpeedPlus,
            0x0b => Self::PrecisionTimeMeasurement,
            0x0c => Self::WirelessUsbExtensions,
            0x0d => Self::Billboard,
            0x0e => Self::Authentication,
            0x0f => Self::BillboardExtensions,
            0x10 => Self::ConfigurationSummary,
            0x11 => Self::FwStatus,
            _ => Self::Reserved,
        }
    }
}

/// Container Id
#[derive(Clone, PartialEq)]
pub struct ContainerId(pub [u8; 16]);

const BOS_MAX: usize = 256;

/// USB hub descriptor
#[derive(Clone, PartialEq)]
pub struct BinaryObjectStoreDescriptor {
    data: [u8; BOS_MAX],
    length: usize,
}

impl BinaryObjectStoreDescriptor {
    /// Create BOS
    pub fn from_data(data: &[u8]) -> Self {
        assert!(data.len() >= 5);
        assert!(data[0] == 5);
        assert!(data[1] == 0x0f);
        let total = u16::from_le_bytes(data[2..=3].try_into().unwrap());
        assert!(usize::from(total) == data.len());

        let mut bos = BinaryObjectStoreDescriptor {
            data: [0; BOS_MAX],
            length: data.len(),
        };
        bos.data[..bos.length].copy_from_slice(data);
        bos
    }

    /// Get container id
    pub fn container_id(&self) -> Option<ContainerId> {
        let buf = &self.data[..self.length];
        let count = buf[4];

        let mut part = &buf[5..];

        for _ in 0..count {
            const DESCRIPTOR_TYPE_DEVICE_CAPABILITY: u8 = 0x10;
            let length = part[0];
            let descriptor_type = part[1];
            assert!(descriptor_type == DESCRIPTOR_TYPE_DEVICE_CAPABILITY);
            let device_capability_type = DeviceCapabilityType::from(part[2]);
            if device_capability_type == DeviceCapabilityType::ContainerId && length == 20 {
                let mut cid = [0u8; 16];
                cid.copy_from_slice(&part[4..20]);
                return Some(ContainerId(cid));
            }
            part = &part[usize::from(length)..];
        }
        None
    }
}

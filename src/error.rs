#[derive(Debug)]
pub enum Error {
    UsbError(nusb::Error),
    UsbTransferError(nusb::transfer::TransferError),
    IoError(std::io::Error),
    InvalidDeviceClass,
    InvalidRespone,
    InvalidPort,
}

impl From<nusb::Error> for Error {
    fn from(error: nusb::Error) -> Self {
        Error::UsbError(error)
    }
}

impl From<nusb::transfer::TransferError> for Error {
    fn from(error: nusb::transfer::TransferError) -> Self {
        Error::UsbTransferError(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UsbError(e) => write!(f, "{}", e),
            Self::UsbTransferError(e) => write!(f, "{}", e),
            Self::IoError(e) => write!(f, "{}", e),
            Self::InvalidDeviceClass => write!(f, "Invalid class"),
            Self::InvalidRespone => write!(f, "Invalid response"),
            Self::InvalidPort => write!(f, "Invalid port"),
        }
    }
}

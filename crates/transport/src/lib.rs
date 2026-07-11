mod replay;
mod serial;

#[cfg(target_os = "linux")]
mod socketcan_transport;

pub use replay::ReplayCanSource;
pub use serial::SerialCanSource;

#[cfg(target_os = "linux")]
pub use socketcan_transport::SocketCanSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    Stream,
    Obd2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedInterface {
    pub name: String,
    pub manufacturer: String,
    pub path: String,
}

pub fn list_connected_interfaces() -> Vec<ConnectedInterface> {
    #[cfg(target_os = "linux")]
    {
        let mut interfaces = serial::list_serial_interfaces();
        interfaces.extend(socketcan_transport::list_socketcan_interfaces());
        interfaces
    }

    #[cfg(not(target_os = "linux"))]
    {
        serial::list_serial_interfaces()
    }
}

use anyhow::{Context, Result};
use car_logger_application::CanFrameSource;
use car_logger_domain::CanFrame;
use socketcan::{CanFrame as SocketCanFrame, CanSocket, EmbeddedFrame, Socket};
use std::fs;

use crate::ConnectedInterface;

pub struct SocketCanSource {
    socket: CanSocket,
}

pub fn list_socketcan_interfaces() -> Vec<ConnectedInterface> {
    fs::read_dir("/sys/class/net")
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.starts_with("can") || name.starts_with("vcan"))
                .map(|name| ConnectedInterface {
                    path: name.clone(),
                    name,
                    manufacturer: "SocketCAN".to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

impl SocketCanSource {
    pub fn open(interface_name: &str) -> Result<Self> {
        let socket = CanSocket::open(interface_name).with_context(|| {
            format!("SocketCANインターフェースを開けませんでした: {interface_name}")
        })?;

        Ok(Self { socket })
    }

    fn convert_frame(frame: SocketCanFrame) -> CanFrame {
        let id = match frame.id() {
            embedded_can::Id::Standard(id) => u32::from(id.as_raw()),
            embedded_can::Id::Extended(id) => id.as_raw(),
        };

        CanFrame::new(
            id,
            frame.is_extended(),
            frame.is_remote_frame(),
            frame.data().to_vec(),
        )
    }
}

impl CanFrameSource for SocketCanSource {
    fn receive(&mut self) -> Result<CanFrame> {
        let frame = self
            .socket
            .read_frame()
            .context("SocketCANフレームの受信に失敗しました")?;

        Ok(Self::convert_frame(frame))
    }
}

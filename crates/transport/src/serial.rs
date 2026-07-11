use std::{io::Read, time::Duration};

use anyhow::{Context, Result};
use car_logger_application::CanFrameSource;
use car_logger_domain::CanFrame;
use serialport::{SerialPort, SerialPortType};

use crate::ConnectedInterface;

pub struct SerialCanSource {
    port: Box<dyn SerialPort>,
}

pub fn list_serial_interfaces() -> Vec<ConnectedInterface> {
    serialport::available_ports()
        .map(|ports| {
            ports
                .into_iter()
                .map(|port| {
                    let manufacturer = match port.port_type {
                        SerialPortType::UsbPort(info) => info
                            .manufacturer
                            .unwrap_or_else(|| "Unknown USB".to_string()),
                        SerialPortType::BluetoothPort => "Bluetooth".to_string(),
                        SerialPortType::PciPort => "PCI serial".to_string(),
                        SerialPortType::Unknown => "Unknown".to_string(),
                    };

                    ConnectedInterface {
                        name: port.port_name.clone(),
                        manufacturer,
                        path: port.port_name,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

impl SerialCanSource {
    pub fn open(port_name: &str, baud_rate: u32) -> Result<Self> {
        let port = serialport::new(port_name, baud_rate)
            .timeout(Duration::from_millis(100))
            .open()
            .with_context(|| format!("シリアルポートを開けませんでした: {port_name}"))?;

        Ok(Self { port })
    }
}

impl CanFrameSource for SerialCanSource {
    fn receive(&mut self) -> Result<CanFrame> {
        let mut buffer = [0_u8; 64];
        let read_size = self
            .port
            .read(&mut buffer)
            .context("シリアルデータの読み取りに失敗しました")?;

        // 現時点では仮実装。
        // 実際にはELM327、SLCAN、独自バイナリ形式などを解析する。
        Ok(CanFrame::new(0, false, false, buffer[..read_size].to_vec()))
    }
}

use anyhow::Result;
use car_logger_domain::CanFrame;

/// CANフレームの取得元。
///
/// 実装例:
/// - SerialCanSource
/// - ReplayCanSource
/// - SocketCanSource
pub trait CanFrameSource: Send {
    fn receive(&mut self) -> Result<CanFrame>;
}

/// CANフレームの保存先。
///
/// 実装例:
/// - DuckdbCanFrameRepository
/// - StorageRepository
/// - InMemoryCanFrameRepository
pub trait CanFrameRepository: Send {
    fn save(&mut self, frame: &CanFrame) -> Result<()>;

    fn save_batch(&mut self, frames: &[CanFrame]) -> Result<()> {
        for frame in frames {
            self.save(frame)?;
        }

        Ok(())
    }
}

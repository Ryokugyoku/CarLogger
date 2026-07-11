use std::collections::VecDeque;

use anyhow::{Result, bail};
use car_logger_application::CanFrameSource;
use car_logger_domain::CanFrame;

pub struct ReplayCanSource {
    frames: VecDeque<CanFrame>,
}

impl ReplayCanSource {
    pub fn new(frames: Vec<CanFrame>) -> Self {
        Self {
            frames: frames.into(),
        }
    }
}

impl CanFrameSource for ReplayCanSource {
    fn receive(&mut self) -> Result<CanFrame> {
        let Some(frame) = self.frames.pop_front() else {
            bail!("再生対象のCANフレームがありません");
        };

        Ok(frame)
    }
}

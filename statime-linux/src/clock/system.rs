use super::{LinuxClock, PortTimestampToTime};
use statime::{Clock, OverlayClock, SharedClock};

pub trait PortClock:
    Clock<Error = <LinuxClock as Clock>::Error> + PortTimestampToTime + Send + Sync
{
    fn clone_box(&self) -> Box<dyn PortClock>;
}
impl PortClock for LinuxClock {
    fn clone_box(&self) -> Box<dyn PortClock> {
        Box::new(self.clone())
    }
}
impl PortClock for SharedClock<OverlayClock<LinuxClock>> {
    fn clone_box(&self) -> Box<dyn PortClock> {
        Box::new(self.clone())
    }
}
pub type BoxedClock = Box<dyn PortClock>;
pub type SharedOverlayClock = SharedClock<OverlayClock<LinuxClock>>;

#[derive(Debug, Clone)]
pub enum SystemClock {
    Linux(LinuxClock),
    Overlay(SharedOverlayClock),
}
impl SystemClock {
    pub fn clone_boxed(&self) -> BoxedClock {
        match self {
            Self::Linux(clock) => Box::new(clock.clone()),
            Self::Overlay(clock) => Box::new(clock.clone()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ClockSyncMode {
    #[default]
    FromSystem,
    ToSystem,
}

pub mod eth;
pub mod ip;

use rand::rngs::StdRng;
use statime::{
    config::ClockIdentity,
    filters::KalmanFilter,
    port::{InBmca, Port},
    PtpInstanceState,
};
use std::sync::RwLock;

use crate::clock::system::BoxedClock;

pub type BmcaPort = Port<
    'static,
    InBmca,
    Option<Vec<ClockIdentity>>,
    StdRng,
    BoxedClock,
    KalmanFilter,
    RwLock<PtpInstanceState>,
>;

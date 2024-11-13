#![allow(async_fn_in_trait)]

pub mod json;

use crate::config::Config;
use async_trait::async_trait;
use statime::{
    config::TimePropertiesDS,
    observability::{current::CurrentDS, default::DefaultDS, parent::ParentDS, PathTraceDS},
};
use tokio::sync::watch::Receiver;

/// Observable version of the InstanceState struct
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ObservableInstanceState {
    /// A concrete implementation of the PTP Default dataset (IEEE1588-2019
    /// section 8.2.1)
    pub default_ds: DefaultDS,
    /// A concrete implementation of the PTP Current dataset (IEEE1588-2019
    /// section 8.2.2)
    pub current_ds: CurrentDS,
    /// A concrete implementation of the PTP Parent dataset (IEEE1588-2019
    /// section 8.2.3)
    pub parent_ds: ParentDS,
    /// A concrete implementation of the PTP Time Properties dataset
    /// (IEEE1588-2019 section 8.2.4)
    pub time_properties_ds: TimePropertiesDS,
    /// A concrete implementation of the PTP Path Trace dataset (IEEE1588-2019
    /// section 16.2.2)
    pub path_trace_ds: PathTraceDS,
    /// Offset in seconds for each port
    pub offset_s: Vec<f64>,
}

#[async_trait]
pub trait Observer: Send + Sync {
    async fn observe(
        &self,
        config: &Config,
        instance_state_receiver: Receiver<ObservableInstanceState>,
    ) -> std::io::Result<()>;

    async fn get_state(&self) -> ObservableInstanceState;
}

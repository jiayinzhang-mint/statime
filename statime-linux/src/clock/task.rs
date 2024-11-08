use super::{
    system::{ClockSyncMode, SharedOverlayClock, SystemClock},
    timer::Timer,
    LinuxClock,
};
use statime::{
    filters::{Filter, KalmanConfiguration, KalmanFilter},
    port::Measurement,
    Clock,
};
use std::pin::pin;

pub fn start_clock_task(
    clock: LinuxClock,
    system_clock: SystemClock,
) -> tokio::sync::watch::Sender<ClockSyncMode> {
    let (mode_sender, mode_receiver) = tokio::sync::watch::channel(ClockSyncMode::FromSystem);

    match system_clock {
        SystemClock::Linux(system_clock) => {
            tokio::spawn(clock_task(clock, system_clock, None, mode_receiver));
        }
        SystemClock::Overlay(overlay_clock) => {
            tokio::spawn(clock_task(
                clock,
                overlay_clock.clone(),
                Some(overlay_clock),
                mode_receiver,
            ));
        }
    }

    mode_sender
}

async fn clock_task<C: Clock<Error = impl core::fmt::Debug>>(
    mut clock: LinuxClock,
    mut system_clock: C,
    system_clock_overlay: Option<SharedOverlayClock>,
    mut mode_receiver: tokio::sync::watch::Receiver<ClockSyncMode>,
) {
    let mut measurement_timer = pin!(Timer::new());
    let mut update_timer = pin!(Timer::new());

    measurement_timer.as_mut().reset(std::time::Duration::ZERO);

    let mut filter = KalmanFilter::new(KalmanConfiguration::default());

    let mut current_mode = *mode_receiver.borrow_and_update();
    loop {
        tokio::select! {
            () = &mut measurement_timer => {
                let (raw_t1, t2, raw_t3) = clock.system_offset().expect("Unable to determine offset from system clock");
                let (t1, t3) = match &system_clock_overlay {
                    Some(shared) => {
                        let overlay = shared.0.lock().expect("shared clock lock is tainted");
                        (overlay.time_from_underlying(raw_t1), overlay.time_from_underlying(raw_t3))
                    },
                    None => (raw_t1, raw_t3)
                };

                log::debug!("Interclock measurement: {} {} {}", t1, t2, t3);

                let delay = (t3-t1)/2;
                let offset_a = t2 - t1;
                let offset_b = t3 - t2;

                let update = match current_mode {
                    ClockSyncMode::FromSystem => {
                        let m = Measurement {
                            event_time: t2,
                            offset: Some(offset_a - delay),
                            delay: Some(delay),
                            peer_delay: None,
                            raw_sync_offset: Some(offset_a),
                            raw_delay_offset: Some(-offset_b),
                        };
                        filter.measurement(m, &mut clock)
                    },
                    ClockSyncMode::ToSystem => {
                        let m = Measurement {
                            event_time: t1+delay,
                            offset: Some(offset_b - delay),
                            delay: Some(delay),
                            peer_delay: None,
                            raw_sync_offset: Some(offset_b),
                            raw_delay_offset: Some(-offset_a),
                        };
                        filter.measurement(m, &mut system_clock)
                    },
                };

                if let Some(timeout) = update.next_update {
                    update_timer.as_mut().reset(timeout);
                }

                measurement_timer.as_mut().reset(std::time::Duration::from_millis(250));
            }
            () = &mut update_timer => {
                let update = match current_mode {
                    ClockSyncMode::FromSystem => filter.update(&mut clock),
                    ClockSyncMode::ToSystem => filter.update(&mut system_clock),
                };
                if let Some(timeout) = update.next_update {
                    update_timer.as_mut().reset(timeout);
                }
            }
            _ = mode_receiver.changed() => {
                let new_mode = *mode_receiver.borrow_and_update();
                if new_mode != current_mode {
                    let mut new_filter = KalmanFilter::new(KalmanConfiguration::default());
                    std::mem::swap(&mut filter, &mut new_filter);
                    match current_mode {
                        ClockSyncMode::FromSystem => new_filter.demobilize(&mut clock),
                        ClockSyncMode::ToSystem => new_filter.demobilize(&mut system_clock)
                    };
                    current_mode = new_mode;
                }
            }
        }
    }
}

use super::BmcaPort;
use crate::{
    clock::{
        system::BoxedClock,
        timer::{Timer, Timers},
    },
    socket::PtpTargetAddress,
    tlvforwarder::TlvForwarder,
};
use statime::{
    port::{PortAction, PortActionIterator, TimestampContext, MAX_DATA_LEN},
    time::Time,
};
use std::pin::pin;
use timestamped_socket::{
    networkaddress::EthernetAddress,
    socket::{Open, Socket},
};
use tokio::sync::mpsc::{Receiver, Sender};

async fn handle_actions_ethernet(
    actions: PortActionIterator<'_>,
    interface: libc::c_int,
    socket: &mut Socket<EthernetAddress, Open>,
    timers: &mut Timers<'_>,
    tlv_forwarder: &TlvForwarder,
    clock: &BoxedClock,
) -> Option<(TimestampContext, Time)> {
    let mut pending_timestamp = None;

    for action in actions {
        match action {
            PortAction::SendEvent {
                context,
                data,
                link_local,
            } => {
                // send timestamp of the send
                let time = socket
                    .send_to(
                        data,
                        EthernetAddress::new(
                            if link_local {
                                EthernetAddress::PDELAY_EVENT.protocol()
                            } else {
                                EthernetAddress::PRIMARY_EVENT.protocol()
                            },
                            if link_local {
                                EthernetAddress::PDELAY_EVENT.mac()
                            } else {
                                EthernetAddress::PRIMARY_EVENT.mac()
                            },
                            interface,
                        ),
                    )
                    .await
                    .expect("Failed to send event message");

                // anything we send later will have a later pending (send) timestamp
                if let Some(time) = time {
                    log::trace!("Send timestamp {:?}", time);
                    pending_timestamp = Some((context, clock.port_timestamp_to_time(time)));
                } else {
                    log::error!("Missing send timestamp");
                }
            }
            PortAction::SendGeneral { data, link_local } => {
                socket
                    .send_to(
                        data,
                        EthernetAddress::new(
                            if link_local {
                                EthernetAddress::PDELAY_GENERAL.protocol()
                            } else {
                                EthernetAddress::PRIMARY_GENERAL.protocol()
                            },
                            if link_local {
                                EthernetAddress::PDELAY_GENERAL.mac()
                            } else {
                                EthernetAddress::PRIMARY_GENERAL.mac()
                            },
                            interface,
                        ),
                    )
                    .await
                    .expect("Failed to send general message");
            }
            PortAction::ResetAnnounceTimer { duration } => {
                timers.port_announce_timer.as_mut().reset(duration);
            }
            PortAction::ResetSyncTimer { duration } => {
                timers.port_sync_timer.as_mut().reset(duration);
            }
            PortAction::ResetDelayRequestTimer { duration } => {
                timers.delay_request_timer.as_mut().reset(duration);
            }
            PortAction::ResetAnnounceReceiptTimer { duration } => {
                timers.port_announce_timeout_timer.as_mut().reset(duration);
            }
            PortAction::ResetFilterUpdateTimer { duration } => {
                timers.filter_update_timer.as_mut().reset(duration);
            }
            PortAction::ForwardTLV { tlv } => tlv_forwarder.forward(tlv.into_owned()),
        }
    }

    pending_timestamp
}

// the Port task for ethernet transport
//
// This task waits for a new port (in the bmca state) to arrive on its Receiver.
// It will then move the port into the running state, and process actions. When
// the task is notified of a BMCA, it will stop running, move the port into the
// bmca state, and send it on its Sender
pub async fn ethernet_port_task(
    mut port_task_receiver: Receiver<BmcaPort>,
    port_task_sender: Sender<BmcaPort>,
    interface: libc::c_int,
    mut socket: Socket<EthernetAddress, Open>,
    mut bmca_notify: tokio::sync::watch::Receiver<bool>,
    mut tlv_forwarder: TlvForwarder,
    clock: BoxedClock,
) {
    let mut timers = Timers {
        port_sync_timer: pin!(Timer::new()),
        port_announce_timer: pin!(Timer::new()),
        port_announce_timeout_timer: pin!(Timer::new()),
        delay_request_timer: pin!(Timer::new()),
        filter_update_timer: pin!(Timer::new()),
    };

    loop {
        let port_in_bmca = port_task_receiver.recv().await.unwrap();

        // Clear out old tlvs if we are not in the master state, so we don't keep em too
        // long.
        if port_in_bmca.is_master() {
            tlv_forwarder.empty()
        }

        // handle post-bmca actions
        let (mut port, actions) = port_in_bmca.end_bmca();

        let mut pending_timestamp = handle_actions_ethernet(
            actions,
            interface,
            &mut socket,
            &mut timers,
            &tlv_forwarder,
            &clock,
        )
        .await;

        while let Some((context, timestamp)) = pending_timestamp {
            pending_timestamp = handle_actions_ethernet(
                port.handle_send_timestamp(context, timestamp),
                interface,
                &mut socket,
                &mut timers,
                &tlv_forwarder,
                &clock,
            )
            .await;
        }

        let mut event_buffer = [0; MAX_DATA_LEN];

        loop {
            let mut actions = tokio::select! {
                result = socket.recv(&mut event_buffer) => match result {
                    Ok(packet) => {
                        if let Some(timestamp) = packet.timestamp {
                            log::trace!("Recv timestamp: {:?}", packet.timestamp);
                            port.handle_event_receive(&event_buffer[..packet.bytes_read], clock.port_timestamp_to_time(timestamp))
                        } else {
                            port.handle_general_receive(&event_buffer[..packet.bytes_read])
                        }
                    }
                    Err(error) => panic!("Error receiving: {error:?}"),
                },
                () = &mut timers.port_announce_timer => {
                    port.handle_announce_timer(&mut tlv_forwarder)
                },
                () = &mut timers.port_sync_timer => {
                    port.handle_sync_timer()
                },
                () = &mut timers.port_announce_timeout_timer => {
                    port.handle_announce_receipt_timer()
                },
                () = &mut timers.delay_request_timer => {
                    port.handle_delay_request_timer()
                },
                () = &mut timers.filter_update_timer => {
                    port.handle_filter_update_timer()
                },
                result = bmca_notify.wait_for(|v| *v) => match result {
                    Ok(_) => break,
                    Err(error) => panic!("Error on bmca notify: {error:?}"),
                }
            };

            loop {
                let pending_timestamp = handle_actions_ethernet(
                    actions,
                    interface,
                    &mut socket,
                    &mut timers,
                    &tlv_forwarder,
                    &clock,
                )
                .await;

                // there might be more actions to handle based on the current action
                actions = match pending_timestamp {
                    Some((context, timestamp)) => port.handle_send_timestamp(context, timestamp),
                    None => break,
                };
            }
        }

        let port_in_bmca = port.start_bmca();
        port_task_sender.send(port_in_bmca).await.unwrap();
    }
}

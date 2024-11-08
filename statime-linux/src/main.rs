use clap::Parser;
use rand::{rngs::StdRng, SeedableRng};
use statime::{
    config::{ClockIdentity, InstanceConfig, SdoId, TimePropertiesDS, TimeSource},
    filters::{KalmanConfiguration, KalmanFilter},
    OverlayClock, PtpInstance, PtpInstanceState, SharedClock,
};
use statime_linux::{
    clock::{
        system::{BoxedClock, ClockSyncMode, SystemClock},
        task::start_clock_task,
        timer::Timer,
        LinuxClock,
    },
    config::Config,
    initialize_logging_parse_config,
    observer::{json::JsonObserver, ObservableInstanceState, Observer},
    port::{eth::ethernet_port_task, ip::port_task, BmcaPort},
    socket::{
        open_ethernet_socket, open_ipv4_event_socket, open_ipv4_general_socket,
        open_ipv6_event_socket, open_ipv6_general_socket,
    },
    tlvforwarder::TlvForwarder,
};
use std::{collections::HashMap, path::PathBuf, pin::pin, sync::RwLock};
use timestamped_socket::{interface::interfaces, socket::InterfaceTimestampMode};
use tokio::sync::mpsc::{Receiver, Sender};

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    /// Configuration file to use
    #[clap(
        long = "config",
        short = 'c',
        default_value = "/etc/statime/statime.toml"
    )]
    config_file: Option<PathBuf>,
}

async fn start_sync_ports(
    instance: &'static PtpInstance<KalmanFilter, RwLock<PtpInstanceState>>,
    bmca_notify_sender: tokio::sync::watch::Sender<bool>,
    instance_state_sender: tokio::sync::watch::Sender<ObservableInstanceState>,
    mut main_task_receivers: Vec<Receiver<BmcaPort>>,
    main_task_senders: Vec<Sender<BmcaPort>>,
    internal_sync_senders: Vec<tokio::sync::watch::Sender<ClockSyncMode>>,
    clock_port_map: Vec<Option<usize>>,
) -> ! {
    // run bmca over all of the ports at the same time. The ports don't perform
    // their normal actions at this time: bmca is stop-the-world!
    let mut bmca_timer = pin!(Timer::new());

    loop {
        // reset bmca timer
        bmca_timer.as_mut().reset(instance.bmca_interval());

        // wait until the next BMCA
        bmca_timer.as_mut().await;

        // notify all the ports that they need to stop what they're doing
        bmca_notify_sender
            .send(true)
            .expect("Bmca notification failed");

        let mut bmca_ports = Vec::with_capacity(main_task_receivers.len());
        let mut mut_bmca_ports = Vec::with_capacity(main_task_receivers.len());

        for receiver in main_task_receivers.iter_mut() {
            bmca_ports.push(receiver.recv().await.unwrap());
        }

        // have all ports so deassert stop
        bmca_notify_sender
            .send(false)
            .expect("Bmca notification failed");

        for mut_bmca_port in bmca_ports.iter_mut() {
            mut_bmca_ports.push(mut_bmca_port);
        }

        instance.bmca(&mut mut_bmca_ports);

        // Update instance state for observability
        // We don't care if isn't anybody on the other side
        let _ = instance_state_sender.send(ObservableInstanceState {
            default_ds: instance.default_ds(),
            current_ds: instance.current_ds(),
            parent_ds: instance.parent_ds(),
            time_properties_ds: instance.time_properties_ds(),
            path_trace_ds: instance.path_trace_ds(),
            offset_s: mut_bmca_ports.iter().map(|p| p.get_offset_s()).collect(),
        });

        let mut clock_states = vec![ClockSyncMode::FromSystem; internal_sync_senders.len()];
        for (idx, port) in mut_bmca_ports.iter().enumerate() {
            if port.is_steering() {
                if let Some(id) = clock_port_map[idx] {
                    clock_states[id] = ClockSyncMode::ToSystem;
                }
            }
        }
        for (mode, sender) in clock_states.into_iter().zip(internal_sync_senders.iter()) {
            sender.send(mode).expect("Clock mode change failed");
        }

        drop(mut_bmca_ports);

        for (port, sender) in bmca_ports.into_iter().zip(main_task_senders.iter()) {
            sender.send(port).await.unwrap();
        }
    }
}

fn get_clock_id() -> Option<[u8; 8]> {
    let candidates = interfaces()
        .unwrap()
        .into_iter()
        .filter_map(|(_, data)| data.mac());

    for mac in candidates {
        // Ignore multicast and locally administered mac addresses
        if mac[0] & 0x3 == 0 && mac.iter().any(|x| *x != 0) {
            let f = |i| mac.get(i).copied().unwrap_or_default();
            return Some(std::array::from_fn(f));
        }
    }

    None
}

async fn start_sync(config: Config) {
    let clock_identity = config.identity.unwrap_or(ClockIdentity(
        get_clock_id().expect("could not get clock identity"),
    ));

    log::info!("Clock identity: {}", hex::encode(clock_identity.0));

    let instance_config = InstanceConfig {
        clock_identity,
        priority_1: config.priority1,
        priority_2: config.priority2,
        domain_number: config.domain,
        slave_only: false,
        sdo_id: SdoId::try_from(config.sdo_id).expect("sdo-id should be between 0 and 4095"),
        path_trace: config.path_trace,
    };

    let time_properties_ds =
        TimePropertiesDS::new_arbitrary_time(false, false, TimeSource::InternalOscillator);

    let system_clock = if config.virtual_system_clock {
        SystemClock::Overlay(SharedClock::new(OverlayClock::new(LinuxClock::CLOCK_TAI)))
    } else {
        SystemClock::Linux(LinuxClock::CLOCK_TAI)
    };

    // Leak to get a static reference, the ptp instance will be around for the rest
    // of the program anyway
    let instance = Box::leak(Box::new(PtpInstance::new(
        instance_config,
        time_properties_ds,
    )));

    // The observer for the metrics exporter
    let (instance_state_sender, instance_state_receiver) =
        tokio::sync::watch::channel(ObservableInstanceState {
            default_ds: instance.default_ds(),
            current_ds: instance.current_ds(),
            parent_ds: instance.parent_ds(),
            time_properties_ds: instance.time_properties_ds(),
            path_trace_ds: instance.path_trace_ds(),
            offset_s: vec![],
        });

    let config_clone = config.clone();
    let observer = JsonObserver {};
    let _ = tokio::spawn(async move {
        observer
            .observe(&config_clone, instance_state_receiver)
            .await
    })
    .await
    .inspect_err(|err| tracing::error!(err=?err, "Abnormal termination of the state observer"));

    let (bmca_notify_sender, bmca_notify_receiver) = tokio::sync::watch::channel(false);

    let mut main_task_senders = Vec::with_capacity(config.ports.len());
    let mut main_task_receivers = Vec::with_capacity(config.ports.len());

    let mut internal_sync_senders = vec![];

    let mut clock_name_map = HashMap::new();
    let mut clock_port_map = Vec::with_capacity(config.ports.len());

    let mut ports = Vec::with_capacity(config.ports.len());

    let tlv_forwarder = TlvForwarder::new();

    for port_config in config.ports {
        let interface = port_config.interface;
        let network_mode = port_config.network_mode;
        let (port_clock, timestamping) = match port_config.hardware_clock {
            Some(idx) => {
                let mut clock = LinuxClock::open_idx(idx).expect("Unable to open clock");
                if let Some(id) = clock_name_map.get(&idx) {
                    clock_port_map.push(Some(*id));
                } else {
                    clock.init().expect("Unable to initialize clock");
                    let id = internal_sync_senders.len();
                    clock_port_map.push(Some(id));
                    clock_name_map.insert(idx, id);
                    internal_sync_senders
                        .push(start_clock_task(clock.clone(), system_clock.clone()));
                }
                (
                    Box::new(clock) as BoxedClock,
                    InterfaceTimestampMode::HardwarePTPAll,
                )
            }
            None => {
                clock_port_map.push(None);
                (
                    system_clock.clone_boxed(),
                    InterfaceTimestampMode::SoftwareAll,
                )
            }
        };

        let rng = StdRng::from_entropy();
        let bind_phc = port_config.hardware_clock;
        let port = instance.add_port(
            port_config.into(),
            KalmanConfiguration::default(),
            port_clock.clone_box(),
            rng,
        );

        let (main_task_sender, port_task_receiver) = tokio::sync::mpsc::channel(1);
        let (port_task_sender, main_task_receiver) = tokio::sync::mpsc::channel(1);

        // We can't send the port yet, since that may start running on the port,
        // inhibiting write access to the instance and making it impossible to
        // create more ports.
        ports.push(port);
        main_task_senders.push(main_task_sender);
        main_task_receivers.push(main_task_receiver);

        match network_mode {
            statime_linux::config::NetworkMode::Ipv4 => {
                let event_socket = open_ipv4_event_socket(interface, timestamping, bind_phc)
                    .expect("Could not open event socket");
                let general_socket =
                    open_ipv4_general_socket(interface).expect("Could not open general socket");

                tokio::spawn(port_task(
                    port_task_receiver,
                    port_task_sender,
                    event_socket,
                    general_socket,
                    bmca_notify_receiver.clone(),
                    tlv_forwarder.duplicate(),
                    port_clock,
                ));
            }
            statime_linux::config::NetworkMode::Ipv6 => {
                let event_socket = open_ipv6_event_socket(interface, timestamping, bind_phc)
                    .expect("Could not open event socket");
                let general_socket =
                    open_ipv6_general_socket(interface).expect("Could not open general socket");

                tokio::spawn(port_task(
                    port_task_receiver,
                    port_task_sender,
                    event_socket,
                    general_socket,
                    bmca_notify_receiver.clone(),
                    tlv_forwarder.duplicate(),
                    port_clock,
                ));
            }
            statime_linux::config::NetworkMode::Ethernet => {
                let socket = open_ethernet_socket(interface, timestamping, bind_phc)
                    .expect("Could not open socket");

                tokio::spawn(ethernet_port_task(
                    port_task_receiver,
                    port_task_sender,
                    interface
                        .get_index()
                        .expect("Unable to get network interface index") as _,
                    socket,
                    bmca_notify_receiver.clone(),
                    tlv_forwarder.duplicate(),
                    port_clock,
                ));
            }
        }
    }

    // Drop the forwarder so we don't keep an unneeded subscriber.
    drop(tlv_forwarder);

    // All ports created, so we can start running them.
    for (i, port) in ports.into_iter().enumerate() {
        main_task_senders[i]
            .send(port)
            .await
            .expect("space in channel buffer");
    }

    start_sync_ports(
        instance,
        bmca_notify_sender,
        instance_state_sender,
        main_task_receivers,
        main_task_senders,
        internal_sync_senders,
        clock_port_map,
    )
    .await
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let config = initialize_logging_parse_config(
        &args
            .config_file
            .expect("could not determine config file path"),
    );

    start_sync(config).await;
}

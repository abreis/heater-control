use alloc::boxed::Box;
use embassy_net as net;
use embassy_sync::{
    blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex},
    pubsub::{PubSubBehavior, PubSubChannel},
    watch,
};
use embassy_time::{Duration, Timer};
use esp_println::println;
use esp_wifi::wifi;

/// How often to check for changes in the network status.
const NET_MONITOR_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkStatus {
    link_up: bool,
    ip_config: Option<embassy_net::StaticConfigV4>,
}

pub type NetStatusWatch<const W: usize> = &'static watch::Watch<NoopRawMutex, NetworkStatus, W>;
pub type NetStatusDynSender = watch::DynSender<'static, NetworkStatus>;
pub type NetStatusDynReceiver = watch::DynReceiver<'static, NetworkStatus>;

/// Takes a const that sets the maximum number of watchers.
pub fn init<const WATCHERS: usize>() -> NetStatusWatch<WATCHERS> {
    Box::leak(Box::new(watch::Watch::new()))
}

// Monitors the network interface and signals changes.
#[embassy_executor::task]
pub async fn net_monitor(stack: net::Stack<'static>, netstatus_sender: NetStatusDynSender) {
    let mut status = NetworkStatus {
        link_up: false,
        ip_config: None,
    };

    loop {
        Timer::after(NET_MONITOR_INTERVAL).await;

        let new_status = NetworkStatus {
            link_up: stack.is_link_up(),
            ip_config: stack.config_v4(),
        };

        // Notify if changed.
        if status != new_status {
            netstatus_sender.send(new_status.clone());
            status = new_status;
        }
    }
}

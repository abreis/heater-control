use alloc::boxed::Box;
use embassy_executor::Spawner;
use embassy_net as net;
use esp_hal::rng::Rng;
use esp_wifi::wifi;

/// Maximum number of sockets to allocate memory for.
const NET_SOCKETS: usize = 3;

pub async fn init(
    driver: wifi::WifiDevice<'static>,
    mut rng: Rng,
) -> (
    net::Stack<'static>,
    net::Runner<'static, wifi::WifiDevice<'static>>,
) {
    // IPv4 + DHCP
    let net_config = net::Config::dhcpv4(net::DhcpConfig::default());

    // Memory resources for the network stack.
    let net_resources = Box::leak::<'static>(Box::new(net::StackResources::<NET_SOCKETS>::new()));

    let seed_64b = (rng.random() as u64) << 32 | rng.random() as u64;
    let (net_stack, net_runner) = net::new(driver, net_config, net_resources, seed_64b);

    (net_stack, net_runner)
}

/// Drives the network stack.
#[embassy_executor::task]
pub async fn stack_runner(mut runner: net::Runner<'static, wifi::WifiDevice<'static>>) {
    runner.run().await
}

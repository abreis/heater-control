use crate::memlog::SharedLogger;
use alloc::{boxed::Box, format};
use embassy_time::{Duration, Timer};
use esp_hal::{peripherals, rng::Rng};
use esp_wifi::{
    EspWifiTimerSource,
    config::PowerSaveMode,
    wifi::{self, WifiState},
};

const WIFI_SSID: &str = "definitely_not_a_honeypot";
const WIFI_PASS: &str = "nice_try_but";
// How long to wait before attempting to reconnect to WiFi.
const WIFI_RECONNECT_PAUSE: Duration = Duration::from_secs(5);

/// Initializes the WiFi in client mode.
///
/// Returns a WiFi controller and WiFi interfaces.
///
/// Sets a hardcoded SSID and passphrase, and disables power save for performance.
pub async fn init(
    timer: impl EspWifiTimerSource + 'static,
    radio_clocks: peripherals::RADIO_CLK<'static>,
    wifi: peripherals::WIFI<'static>,
    rng: Rng,
) -> Result<(wifi::WifiController<'static>, wifi::Interfaces<'static>), wifi::WifiError> {
    // Allow some time before initializing the (power-hungry) WiFi.
    Timer::after(Duration::from_millis(250)).await;

    let wifi_init =
        Box::leak::<'static>(Box::new(esp_wifi::init(timer, rng, radio_clocks).unwrap()));
    let (mut wifi_controller, wifi_interfaces) = esp_wifi::wifi::new(wifi_init, wifi).unwrap();

    // Set the wifi client configuration.
    let wifi_client_config = wifi::ClientConfiguration {
        ssid: WIFI_SSID.into(),
        password: WIFI_PASS.into(),
        ..Default::default()
    };
    wifi_controller.set_configuration(&wifi::Configuration::Client(wifi_client_config))?;

    // Disable power saving, can cause random packet delay and loss (#3014).
    wifi_controller.set_power_saving(PowerSaveMode::None)?;

    Ok((wifi_controller, wifi_interfaces))
}

#[embassy_executor::task]
pub async fn wifi_permanent_connection(
    mut controller: wifi::WifiController<'static>,
    memlog: SharedLogger,
) {
    memlog.debug(format!("wifi: state: {:?}", wifi::wifi_state()));

    loop {
        // If we're still connected, wait until we disconnect.
        if wifi::wifi_state() == WifiState::StaConnected {
            controller
                .wait_for_event(wifi::WifiEvent::StaDisconnected)
                .await;
        }

        // Pause before attempting to reconnect.
        Timer::after(WIFI_RECONNECT_PAUSE).await;

        // Start the WiFi controller if necessary.
        if !matches!(controller.is_started(), Ok(true)) {
            // TODO: do we need to set_configuration and set_power_saving here in the loop?
            memlog.debug("wifi: starting controller");
            controller.start_async().await.unwrap();
        }

        match controller.connect_async().await {
            Ok(()) => memlog.debug("wifi: connected"),
            Err(error) => memlog.debug(format!("wifi: connect error: {:?}", error)),
        }
    }
}

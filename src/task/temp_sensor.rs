use crate::task::ssr_control::{SsrCommand, SsrCommandChannelSender};
use alloc::boxed::Box;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, watch};
use embassy_time::{Duration, Timer};
use esp_ds18b20::{Ds18b20, Ds18b20Error, Resolution, SensorData};
use esp_hal::gpio;
use esp_onewire::OneWireBus;

pub type TempSensorWatch<const W: usize> =
    &'static watch::Watch<NoopRawMutex, TempSensorReading, W>;
pub type TempSensorDynSender = watch::DynSender<'static, TempSensorReading>;
pub type TempSensorDynReceiver = watch::DynReceiver<'static, TempSensorReading>;

pub type TempSensorReading = Result<SensorData, Ds18b20Error>;

pub fn init<const WATCHERS: usize>() -> TempSensorWatch<WATCHERS> {
    Box::leak(Box::new(watch::Watch::new()))
}

const TEMP_SENSOR_ADDRESS: u64 = 0x545A7B480B646128;
const TEMP_MEASUREMENT_INTERVAL: Duration = Duration::from_secs(10);

// Hysteresis temperature ranges for locking and unlocking the SSR control.
const TEMP_LIMIT_HIGH: f32 = 70.0;
const TEMP_LIMIT_LOW: f32 = 30.0;

#[embassy_executor::task]
pub async fn temp_sensor(
    onewire_pin: gpio::AnyPin<'static>,
    tempsensor_sender: TempSensorDynSender,
    ssrcontrol_command_sender: SsrCommandChannelSender,
) {
    let onewire_bus = OneWireBus::new(onewire_pin);
    let mut sensor = Ds18b20::new(TEMP_SENSOR_ADDRESS, onewire_bus).unwrap();

    let mut temperature_exceeded = false;

    loop {
        Timer::after(TEMP_MEASUREMENT_INTERVAL).await;

        // Attempt to catch errors from 1Wire.
        let sensor_reading: Result<SensorData, Ds18b20Error> = async {
            // Begin a measurement and wait for it to complete.
            sensor.start_temp_measurement()?;

            // 12bit resolution is the default, expects a 750ms wait time.
            let wait_time_ms = Resolution::Bits12.measurement_time_ms();
            let wait_time = Duration::from_millis(wait_time_ms as u64);
            Timer::after(wait_time).await;

            let data = sensor.read_sensor_data()?;

            Ok(data)
        }
        .await;

        // Lock the SSR if the temperature reading exceeds a limit.
        // Unlock with hysteresis.
        if let Ok(SensorData { temperature, .. }) = &sensor_reading {
            if temperature_exceeded && *temperature < TEMP_LIMIT_LOW {
                temperature_exceeded = false;
                ssrcontrol_command_sender.send(SsrCommand::Unlock).await;
            } else if !temperature_exceeded && *temperature >= TEMP_LIMIT_HIGH {
                temperature_exceeded = true;
                ssrcontrol_command_sender.send(SsrCommand::Lock).await;
            }
        }

        tempsensor_sender.send(sensor_reading);
    }
}

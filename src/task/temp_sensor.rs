use crate::{
    ds18b20::{DS18B20Error, Ds18b20, Resolution, SensorData},
    onewire::OneWireBus,
};
use alloc::{boxed::Box, format, string::String};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, pubsub, signal, watch};
use embassy_time::{Duration, Timer};
use embedded_hal::digital::{InputPin, OutputPin};
use esp_hal::gpio;

pub type TempSensorWatch<const W: usize> =
    &'static watch::Watch<NoopRawMutex, TempSensorReading, W>;
pub type TempSensorDynSender = watch::DynSender<'static, TempSensorReading>;
pub type TempSensorDynReceiver = watch::DynReceiver<'static, TempSensorReading>;

pub type TempSensorReading = Result<SensorData, DS18B20Error>;

pub fn init<const WATCHERS: usize>() -> TempSensorWatch<WATCHERS> {
    Box::leak(Box::new(watch::Watch::new()))
}

const TEMP_SENSOR_ADDRESS: u64 = 0x545A7B480B646128;
const TEMP_MEASUREMENT_INTERVAL: Duration = Duration::from_secs(10);

#[embassy_executor::task]
pub async fn temp_sensor(onewire_pin: gpio::AnyPin, tempsensor_sender: TempSensorDynSender) {
    let onewire_bus = OneWireBus::new(onewire_pin);
    let mut sensor = Ds18b20::new(TEMP_SENSOR_ADDRESS, onewire_bus).unwrap();

    loop {
        Timer::after(TEMP_MEASUREMENT_INTERVAL).await;

        // Attempt to catch errors from 1Wire.
        let sensor_reading: Result<SensorData, DS18B20Error> = async {
            // Begin a measurement and wait for it to complete.
            sensor.start_temp_measurement()?;

            // 12bit resolution is the default, expects a 750ms wait time.
            let wait_time = Resolution::Bits12.max_measurement_time();
            Timer::after(wait_time).await;

            let data = sensor.read_sensor_data()?;

            Ok(data)
        }
        .await;

        tempsensor_sender.send(sensor_reading);
    }
}

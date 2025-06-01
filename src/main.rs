#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]

use embassy_executor::{SpawnError, Spawner};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio;
use esp_hal::timer::systimer::SystemTimer;
use esp_hal::timer::timg::TimerGroup;

extern crate alloc;

mod ds18b20;
mod memlog;
mod onewire;
mod task;

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    // let config = esp_hal::Config::default().with_cpu_clock(CpuClock::_240MHz);
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::_160MHz);
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(size: 72 * 1024);
    let timer0 = SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(timer0.alarm0);
    let rng = esp_hal::rng::Rng::new(peripherals.RNG);
    let timer1 = TimerGroup::new(peripherals.TIMG0);

    //
    // M5Stamp-S3 pinout
    //
    // Unused pins, taken here so they aren't used accidentally.
    let _pin8_unused = peripherals.GPIO0;
    let _pin8_unused = peripherals.GPIO3;
    let _pin9_unused = peripherals.GPIO13;
    // G1 controls the solid state relay (SSR) through a MOSFET.
    let output_5ma = gpio::OutputConfig::default()
        .with_drive_strength(gpio::DriveStrength::_5mA)
        .with_drive_mode(gpio::DriveMode::PushPull)
        .with_pull(gpio::Pull::None);
    let pin_control_ssr = gpio::Output::new(peripherals.GPIO1, gpio::Level::Low, output_5ma);
    // G5 reads the case button, which pulls the line to GND when pressed.
    let _pin_button = peripherals.GPIO5;
    // G7 is the 1Wire bus commanding the DS18B20 temperature sensors, which are phantom-powered.
    let pin_sensor_temp = peripherals.GPIO7;
    // G9 goes to the nMOS gate that switches 12VDC power on to the case fan.
    let _pin_power_fan = peripherals.GPIO9;
    // G15 powers the case button LED.
    let _pin_button_led = peripherals.GPIO15;
    // UART pins.
    let pin_uart_tx = peripherals.GPIO43;
    let pin_uart_rx = peripherals.GPIO44;

    // Initialize an in-memory logger with space for 480 characters.
    let memlog = memlog::init(480);
    memlog.info("heater control initialized");

    // Set up the WiFi.
    let (wifi_controller, wifi_interfaces) =
        task::wifi::init(timer1.timer0, peripherals.RADIO_CLK, peripherals.WIFI, rng)
            .await
            .unwrap();

    // Set up the network stack.
    let (net_stack, net_runner) = task::net::init(wifi_interfaces.sta, rng).await;

    //
    // Watcher count: 1 for serial console, 2 for httpd workers

    // Get a watcher to await changes in temperature sensor readings.
    let tempsensor_watch = task::temp_sensor::init::<4>();

    // Get a watcher to monitor the network interface.
    let netstatus_watch = task::net_monitor::init::<4>();

    // Get a watcher to notify the SSR controller of a new duty cycle.
    let ssrcontrol_watch = task::ssr_control::init::<4>();

    //
    // Spawn tasks.
    || -> Result<(), SpawnError> {
        // Keep the wifi connected.
        spawner.spawn(task::wifi::wifi_permanent_connection(
            wifi_controller,
            memlog,
        ))?;

        // Run the network stack.
        spawner.spawn(task::net::stack_runner(net_runner))?;

        // Monitor the network stack for changes.
        spawner.spawn(task::net_monitor(net_stack, netstatus_watch.dyn_sender()))?;

        // Control the SSR duty cycle.
        spawner.spawn(task::ssr_control::ssr_control(
            pin_control_ssr,
            ssrcontrol_watch.dyn_receiver().unwrap(),
        ))?;

        // Take a temperature measurement periodically.
        spawner.spawn(task::temp_sensor(
            pin_sensor_temp.into(),
            tempsensor_watch.dyn_sender(),
        ))?;

        // Launch a control interface on UART0.
        spawner.spawn(task::serial_console(
            peripherals.UART0.into(),
            pin_uart_rx.into(),
            pin_uart_tx.into(),
            ssrcontrol_watch.dyn_sender(),
            ssrcontrol_watch.dyn_receiver().unwrap(),
            netstatus_watch.dyn_receiver().unwrap(),
            tempsensor_watch.dyn_receiver().unwrap(),
            memlog,
        ))?;

        // Launch httpd workers.
        task::httpd::launch_workers(
            spawner,
            net_stack,
            ssrcontrol_watch.dyn_sender(),
            ssrcontrol_watch.dyn_receiver().unwrap(),
            netstatus_watch.dyn_receiver().unwrap(),
            tempsensor_watch.dyn_receiver().unwrap(),
            memlog,
        )?;

        Ok(())
    }()
    .unwrap();
}

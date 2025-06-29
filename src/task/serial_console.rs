#![allow(clippy::too_many_arguments)]
use super::{net_monitor::NetStatusDynReceiver, temp_sensor::TempSensorDynReceiver};
use crate::{
    ESP_APP_DESC,
    memlog::SharedLogger,
    state::SharedState,
    task::ssr_control::{SsrCommand, SsrCommandPublisher, SsrDutyDynReceiver, SsrDutyDynSender},
};
use alloc::{format, string::String};
use core::cell::LazyCell;
use embassy_futures::select;
use embassy_time::{Duration, Timer};
use esp_hal::{Async, gpio, uart};

// Number of bytes to allocate to keep a history of commands.
const COMMAND_HISTORY_BUFFER_SIZE: usize = 1000; // in bytes
const SERIAL_MOTD: LazyCell<String> = LazyCell::new(|| {
    format!(
        "\r\n{} {}\r\nbuilt on {} {}\r\n",
        ESP_APP_DESC.project_name(),
        ESP_APP_DESC.version(),
        ESP_APP_DESC.date(),
        ESP_APP_DESC.time()
    )
});

// Uart::write_async doesn't guarantee it will send everything.
trait UartWriteAllAsync {
    async fn write_all_async(&mut self, data: &[u8]) -> Result<(), uart::TxError>;
}
impl UartWriteAllAsync for uart::Uart<'_, Async> {
    async fn write_all_async(&mut self, mut data: &[u8]) -> Result<(), uart::TxError> {
        while !data.is_empty() {
            let bytes_written = self.write_async(data).await?;
            data = &data[bytes_written..];
        }
        Ok(())
    }
}

/// Triggers actions controlled by output pins.
#[embassy_executor::task]
pub async fn serial_console(
    peripheral_uart: uart::AnyUart<'static>,
    pin_uart_rx: gpio::AnyPin<'static>,
    pin_uart_tx: gpio::AnyPin<'static>,
    mut ssrcontrol_duty_sender: SsrDutyDynSender,
    mut ssrcontrol_duty_receiver: SsrDutyDynReceiver,
    mut ssrcontrol_command_publisher: SsrCommandPublisher,
    mut netstatus_receiver: NetStatusDynReceiver,
    mut tempsensor_receiver: TempSensorDynReceiver,
    memlog: SharedLogger,
    state: SharedState,
) {
    // UART setup. When in loopback mode, ensure TX is configured first (#2914).
    let mut uart = uart::Uart::new(peripheral_uart, uart::Config::default())
        .unwrap()
        .with_tx(pin_uart_tx)
        .with_rx(pin_uart_rx)
        .into_async();

    // Line editor setup.
    let mut input_buffer = [0u8; 100]; // Commands are short, could be smaller
    let mut history_buffer = [0u8; COMMAND_HISTORY_BUFFER_SIZE];
    // let mut editor = noline::builder::EditorBuilder::new_unbounded()
    let mut editor = noline::builder::EditorBuilder::from_slice(&mut input_buffer)
        .with_slice_history(&mut history_buffer)
        .build_async(&mut uart)
        .await
        .unwrap(); // always returns Ok()

    loop {
        // Try block to catch UART errors.
        let catch: Result<(), uart::TxError> = async {
            // Write the MOTD out.
            uart.write_all_async(SERIAL_MOTD.as_bytes()).await?;

            let prompt = "> ";
            // Note: Ctrl-C and Ctrl-D break the readline while loop.
            while let Ok(line) = editor.readline(prompt, &mut uart).await {
                cli_parser(
                    line,
                    &mut uart,
                    &mut ssrcontrol_duty_sender,
                    &mut ssrcontrol_duty_receiver,
                    &mut ssrcontrol_command_publisher,
                    &mut netstatus_receiver,
                    &mut tempsensor_receiver,
                    memlog,
                    state,
                )
                .await?;
            }

            Ok(())
        }
        .await;

        if let Err(tx_error) = catch {
            // Push the UART error to the memlog.
            memlog.warn(format!("uart error: {}", tx_error));
        }

        // Pause before trying the UART again after an error.
        Timer::after(Duration::from_secs(1)).await;
    } // loop
}

async fn cli_parser(
    line: &str,
    uart: &mut uart::Uart<'static, Async>,
    ssrcontrol_duty_sender: &mut SsrDutyDynSender,
    ssrcontrol_duty_receiver: &mut SsrDutyDynReceiver,
    ssrcontrol_command_publisher: &mut SsrCommandPublisher,
    netstatus_receiver: &mut NetStatusDynReceiver,
    tempsensor_receiver: &mut TempSensorDynReceiver,
    memlog: SharedLogger,
    state: SharedState,
) -> Result<(), uart::TxError> {
    // Get the command from the first argument.
    let mut chunks = line.split_whitespace();
    let response = match (chunks.next(), chunks.next()) {
        //
        // Help message.
        (Some("help"), None) => {
            "ssr\r\n\
             · pwm <duty>\r\n\
             · command/{lock,unlock}\r\n\
             temp\r\n\
             · read\r\n\
             · watch\r\n\
             net\r\n\
             · read\r\n\
             · watch\r\n\
             log\r\n\
             · read\r\n\
             · clear\r\n\
             help"
        }

        //
        // SSR control.
        (Some("ssr"), Some("pwm")) => match chunks.next() {
            Some(duty_str) => match duty_str.parse::<u8>() {
                Ok(duty) => {
                    if (0..=100).contains(&duty) {
                        state.lock().await.transition_to_manual(duty);
                        ssrcontrol_duty_sender.send(duty);
                        "Relay duty set"
                    } else {
                        "Relay duty value must be between 0 and 100"
                    }
                }
                Err(_parse_error) => "Failed to parse relay duty value.",
            },
            None => {
                let duty = ssrcontrol_duty_receiver.try_get();
                &format!("{:?}", duty)
            }
        },
        (Some("ssr"), Some("command")) => match chunks.next() {
            Some("lock") => {
                ssrcontrol_command_publisher.publish(SsrCommand::Lock).await;
                "SSR lock command sent"
            }
            Some("unlock") => {
                ssrcontrol_command_publisher
                    .publish(SsrCommand::Unlock)
                    .await;
                "SSR unlock command sent"
            }
            _ => "Relay command required",
        },
        (Some("ssr"), Some(_)) => "Invalid subcommand for 'ssr'",
        (Some("ssr"), None) => "Subcommand required for 'ssr'",

        //
        // Temp sensor.
        (Some("temp"), Some("read")) => {
            let sensor_result = tempsensor_receiver.try_get();
            &format!("{:?}", sensor_result)
        }
        (Some("temp"), Some("watch")) => {
            let mut buf = [0u8; 1];
            'watch_loop: loop {
                // Watch for changes in the temperature sensor until the user interrupts.
                let wait_for_sensor = tempsensor_receiver.changed();
                let wait_for_input = uart.read_async(&mut buf);
                match select::select(wait_for_sensor, wait_for_input).await {
                    select::Either::First(sensor_result) => {
                        let formatted = format!("{:?}\r\n", sensor_result);
                        uart.write_all_async(formatted.as_bytes()).await?;
                    }
                    select::Either::Second(bytes_read) => {
                        // Accept a Ctrl-C or Ctrl-D to interrupt (ASCII End of Text, End of Transmission)
                        if let Ok(1) = bytes_read {
                            if (buf[0] == 0x03) | (buf[0] == 0x04) {
                                break 'watch_loop;
                            }
                        }
                    }
                };
            }
            ""
        }
        (Some("temp"), Some(_)) => "Invalid subcommand for 'temp'",
        (Some("temp"), None) => "Subcommand required for 'temp'",

        //
        // Network status.
        (Some("net"), Some("read")) => {
            let net_status = netstatus_receiver.try_get();
            &format!("{:?}", net_status)
        }
        (Some("net"), Some("watch")) => {
            let mut buf = [0u8; 1];
            'watch_loop: loop {
                let wait_for_status = netstatus_receiver.changed();
                let wait_for_input = uart.read_async(&mut buf);
                match select::select(wait_for_status, wait_for_input).await {
                    select::Either::First(status_result) => {
                        let formatted = format!("{:?}\r\n", status_result);
                        uart.write_all_async(formatted.as_bytes()).await?;
                    }
                    select::Either::Second(bytes_read) => {
                        // Accept a Ctrl-C or Ctrl-D to interrupt (ASCII End of Text, End of Transmission)
                        if let Ok(1) = bytes_read {
                            if (buf[0] == 0x03) | (buf[0] == 0x04) {
                                break 'watch_loop;
                            }
                        }
                    }
                };
            }
            ""
        }
        (Some("net"), Some(_)) => "Invalid subcommand for 'net'",
        (Some("net"), None) => "Subcommand required for 'net'",

        //
        // Log control.
        (Some("log"), Some("read")) => {
            //
            &memlog
                .records()
                .iter()
                .rev()
                .map(|record| format!("{}\r\n", record))
                .collect::<String>()
        }
        (Some("log"), Some("clear")) => {
            memlog.clear();
            "Logs cleared"
        }
        (Some("log"), Some(_)) => "Invalid subcommand for 'log'",
        (Some("log"), None) => "Subcommand required for 'log'",

        //
        //
        (None, None) => "Please enter a command",
        _ => "Unrecognized command",
    };

    if !response.is_empty() {
        uart.write_all_async(response.as_bytes()).await?;
        uart.write_all_async(b"\r\n").await?;
    }

    Ok(())
}

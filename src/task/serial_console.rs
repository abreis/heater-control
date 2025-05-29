use super::{
    net_monitor::NetStatusDynReceiver,
    ssr_control::{SsrControlDynReceiver, SsrControlDynSender},
    temp_sensor::TempSensorDynReceiver,
};
use crate::memlog::{self, SharedLogger};
use alloc::{format, string::String};
use embassy_futures::select;
use embassy_time::{Duration, Timer};
use esp_hal::{Async, gpio, uart};

// Number of bytes to allocate to keep a history of commands.
const COMMAND_HISTORY_BUFFER_SIZE: usize = 1000; // in bytes
const MOTD: &'static str = const_format::formatcp!(
    "\r\n{} {}\r\n",
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_VERSION")
);

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
    peripheral_uart: uart::AnyUart,
    pin_uart_rx: gpio::AnyPin,
    pin_uart_tx: gpio::AnyPin,
    mut ssrcontrol_sender: SsrControlDynSender,
    mut ssrcontrol_receiver: SsrControlDynReceiver,
    mut netstatus_receiver: NetStatusDynReceiver,
    mut tempsensor_receiver: TempSensorDynReceiver,
    memlog: SharedLogger,
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
        let catch = async || -> Result<(), uart::TxError> {
            // Write the MOTD out.
            uart.write_all_async(MOTD.as_bytes()).await?;

            let prompt = "> ";
            // Note: Ctrl-C and Ctrl-D break the readline while loop.
            while let Ok(line) = editor.readline(prompt, &mut uart).await {
                cli_parser(
                    line,
                    &mut uart,
                    &mut ssrcontrol_sender,
                    &mut ssrcontrol_receiver,
                    &mut netstatus_receiver,
                    &mut tempsensor_receiver,
                    memlog,
                )
                .await?;
            }

            Ok(())
        }()
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
    ssrcontrol_sender: &mut SsrControlDynSender,
    ssrcontrol_receiver: &mut SsrControlDynReceiver,
    netstatus_receiver: &mut NetStatusDynReceiver,
    tempsensor_receiver: &mut TempSensorDynReceiver,
    memlog: SharedLogger,
) -> Result<(), uart::TxError> {
    // Get the command from the first argument.
    let mut chunks = line.split_whitespace();
    let response = match (chunks.next(), chunks.next()) {
        //
        // Help message.
        (Some("help"), None) => {
            "ssr\r\n\
             · pwm <duty>\r\n\
             · pwm read\r\n\
             temp\r\n\
             · read\r\n\
             · watch\r\n\
             net\r\n\
             · read\r\n\
             · watch\r\n\
             log\r\n\
             · read\r\n\
             · clear"
        }

        //
        // SSR control.
        (Some("ssr"), Some("pwm")) => match chunks.next() {
            Some("read") => {
                let pwm_value = ssrcontrol_receiver.try_get();
                &format!("{:?}", pwm_value)
            }
            Some(duty_str) => match duty_str.parse::<u8>() {
                Ok(duty_value) => {
                    if (0..=100).contains(&duty_value) {
                        ssrcontrol_sender.send(duty_value);
                        "Relay duty set"
                    } else {
                        "Relay duty value must be between 0 and 100"
                    }
                }
                Err(_parse_error) => "Failed to parse relay duty value.",
            },
            None => "Relay duty value required for 'ssr pwm'",
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
            // Note: this locks the entire memlog while it is being printed.
            for record in memlog.records().iter().rev() {
                let timestamp = format_milliseconds_to_hms(record.instant.as_millis());
                let formatted = format!("[{}] {}: {}\r\n", timestamp, record.level, record.text);
                uart.write_all_async(formatted.as_bytes()).await?;
            }
            ""
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

/// Formats a u64 millisecond value into "HHHHH:MM:SS.xxx" string.
#[inline]
fn format_milliseconds_to_hms(total_ms: u64) -> String {
    let millis_part = total_ms % 1000;
    let total_seconds = total_ms / 1000;

    let seconds_part = total_seconds % 60;
    let total_minutes = total_seconds / 60;

    let minutes_part = total_minutes % 60;
    let hours_part = total_minutes / 60;

    format!(
        "{:05}:{:02}:{:02}.{:03}",
        hours_part, minutes_part, seconds_part, millis_part
    )
}

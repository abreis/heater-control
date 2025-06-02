use super::{net_monitor::NetStatusDynReceiver, temp_sensor::TempSensorDynReceiver};
use crate::{
    memlog::{self, SharedLogger},
    task::ssr_control::{SsrCommand, SsrCommandChannelSender, SsrDutySignal},
};
use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
};
use core::cell::RefCell;
use embassy_executor::{SpawnError, Spawner};
use embassy_time::Duration;
use picoserve::{
    AppBuilder, AppRouter, Config, Router, Timeouts,
    routing::{PathRouter, get, parse_path_segment, post},
};

const HTTPD_MOTD: &str =
    const_format::formatcp!("{} {}\n", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

/// Number of workers to spawn.
pub const HTTPD_WORKERS: usize = 2;

/// Server port.
pub const HTTPD_PORT: u16 = 80;

/// Server timeouts, chosen for operation over embedded WiFi.
pub const HTTPD_TIMEOUTS: Timeouts<Duration> = Timeouts {
    // Timeout for the initial request on a new connection, accommodating potential WiFi latency.
    start_read_request: Some(Duration::from_secs(5)),
    // Shorter timeout for subsequent requests on an existing persistent (keep-alive) connection.
    persistent_start_read_request: Some(Duration::from_secs(2)),
    // Timeout if the server has started reading a request but stalls (e.g., client sends partial data).
    read_request: Some(Duration::from_secs(3)),
    // Timeout if the server is writing a response but the client is not reading it promptly.
    write: Some(Duration::from_secs(3)),
};

pub const HTTPD_CONFIG: Config<Duration> =
    Config::new(HTTPD_TIMEOUTS).close_connection_after_response(); // .keep_connection_alive();

pub fn launch_workers(
    spawner: Spawner,
    stack: embassy_net::Stack<'static>,
    ssrcontrol_duty_signal: SsrDutySignal,
    ssrcontrol_command_sender: SsrCommandChannelSender,
    netstatus_receiver: NetStatusDynReceiver,
    tempsensor_receiver: TempSensorDynReceiver,
    memlog: SharedLogger,
) -> Result<(), SpawnError> {
    let app = AppProps {
        ssrcontrol_duty_signal,
        ssrcontrol_command_sender,
        netstatus_receiver,
        tempsensor_receiver,
        memlog,
    }
    .build_app();
    let app: &'static AppRouter<AppProps> = Box::leak(Box::new(app));

    for worker_id in 0..HTTPD_WORKERS {
        spawner.spawn(worker(worker_id, stack, app))?;
    }

    Ok(())
}

#[embassy_executor::task(pool_size = HTTPD_WORKERS)]
pub async fn worker(
    worker_id: usize,
    stack: embassy_net::Stack<'static>,
    app: &'static AppRouter<AppProps>,
) {
    let mut tcp_rx_buffer = [0; 1024];
    let mut tcp_tx_buffer = [0; 1024];
    let mut http_buffer = [0; 2048];

    picoserve::listen_and_serve(
        worker_id,
        app,
        &HTTPD_CONFIG,
        stack,
        HTTPD_PORT,
        &mut tcp_rx_buffer,
        &mut tcp_tx_buffer,
        &mut http_buffer,
    )
    .await
}

//
// HTTP routing.

struct AppProps {
    ssrcontrol_duty_signal: SsrDutySignal,
    ssrcontrol_command_sender: SsrCommandChannelSender,
    netstatus_receiver: NetStatusDynReceiver,
    tempsensor_receiver: TempSensorDynReceiver,
    memlog: SharedLogger,
}
impl AppBuilder for AppProps {
    type PathRouter = impl picoserve::routing::PathRouter;

    fn build_app(self) -> picoserve::Router<Self::PathRouter> {
        let app: &'static RefCell<AppProps> = Box::leak(Box::new(RefCell::new(self)));

        picoserve::Router::new()
            .route("/", get(|| async { HTTPD_MOTD }))
            .route(
                "/help",
                get(|| async {
                    "GET /ssr/pwm/<duty>\n\
                     GET /ssr/command/{lock,unlock}\n\
                     GET /temp\n\
                     GET /net\n\
                     GET /log\n\
                     GET /log/clear\n\
                     GET /help\n"
                }),
            )
            .route(
                "/temp",
                get(|| async {
                    let value = app.borrow_mut().tempsensor_receiver.try_get();
                    format!("{:#?}\n", value)
                }),
            )
            .route(
                "/net",
                get(|| async {
                    let value = app.borrow_mut().netstatus_receiver.try_get();
                    format!("{:#?}\n", value)
                }),
            )
            .route(
                ("/ssr/pwm", parse_path_segment()),
                get(move |duty: u8| async move {
                    if (0u8..=100).contains(&duty) {
                        app.borrow_mut().ssrcontrol_duty_signal.signal(duty);
                        format!("SSR duty set to {duty}\n")
                    } else {
                        "SSR duty must be in the [0,100] range\n".to_string()
                    }
                }),
            )
            .route(
                ("/ssr/command", parse_path_segment()),
                get(move |command: String| async move {
                    match command.as_str() {
                        "lock" => {
                            app.borrow()
                                .ssrcontrol_command_sender
                                .send(SsrCommand::Lock)
                                .await;
                            "SSR lock command sent\n"
                        }
                        "unlock" => {
                            app.borrow()
                                .ssrcontrol_command_sender
                                .send(SsrCommand::Unlock)
                                .await;
                            "SSR unlock command sent\n"
                        }
                        _ => "Invalid relay command\n",
                    }
                }),
            )
            .route(
                "/log",
                get(|| async {
                    app.borrow()
                        .memlog
                        .records()
                        .iter()
                        .rev()
                        .map(|record| {
                            let timestamp =
                                memlog::format_milliseconds_to_hms(record.instant.as_millis());
                            format!("[{}] {}: {}\n", timestamp, record.level, record.text)
                        })
                        .collect::<String>()
                }),
            )
            .route(
                "/log/clear",
                get(|| async {
                    app.borrow().memlog.clear();
                    "Logs cleared\n"
                }),
            )
    }
}

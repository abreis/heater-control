use core::cell::RefCell;

use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
};
use embassy_executor::{SpawnError, Spawner};
use embassy_time::Duration;
use picoserve::{
    AppBuilder, AppRouter, Config, Router, Timeouts,
    routing::{PathRouter, get, parse_path_segment, post},
};

use crate::memlog::{self, SharedLogger};

use super::{
    net_monitor::NetStatusDynReceiver,
    ssr_control::{SsrControlDynReceiver, SsrControlDynSender},
    temp_sensor::TempSensorDynReceiver,
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
    ssrcontrol_sender: SsrControlDynSender,
    ssrcontrol_receiver: SsrControlDynReceiver,
    netstatus_receiver: NetStatusDynReceiver,
    tempsensor_receiver: TempSensorDynReceiver,
    memlog: SharedLogger,
) -> Result<(), SpawnError> {
    let app = AppProps {
        ssrcontrol_sender,
        ssrcontrol_receiver,
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
    ssrcontrol_sender: SsrControlDynSender,
    ssrcontrol_receiver: SsrControlDynReceiver,
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
                    "GET /ssr/pwm\n\
                     GET /ssr/pwm/<duty>\n\
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
                "/ssr/pwm",
                get(|| async {
                    let value = app.borrow_mut().ssrcontrol_receiver.try_get();
                    format!("{:#?}\n", value)
                }),
            )
            .route(
                ("/ssr/pwm", parse_path_segment::<u8>()),
                get(move |duty| async move {
                    if (0u8..=100).contains(&duty) {
                        app.borrow_mut().ssrcontrol_sender.send(duty);
                        format!("SSR duty set to {duty}\n")
                    } else {
                        "SSR duty must be in the [0,100] range\n".to_string()
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

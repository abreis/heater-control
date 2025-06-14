use alloc::{borrow::Cow, format, string::String};
use core::net::{Ipv4Addr, SocketAddrV4};
use edge_http::io::{
    Error,
    server::{Connection, Handler, Server},
};
use edge_nal::TcpBind;
use edge_nal_embassy::{Tcp, TcpBuffers};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embedded_io_async::{Read, Write};

use crate::{
    memlog::{self, SharedLogger},
    remote::{RemoteControlRequest, RemoteControlResponse},
    state::StateError,
    task::{
        net_monitor::NetStatusDynReceiver,
        ssr_control::{SsrCommand, SsrCommandChannelSender, SsrDutyDynReceiver, SsrDutyDynSender},
        temp_sensor::TempSensorDynReceiver,
    },
};

const HTTPD_LISTEN_ADDR: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), 80);

const HTTPD_MOTD: &str =
    const_format::formatcp!("{} {}\n", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

// How many concurrent connections we can accept.
const HTTPD_HANDLERS: usize = 2;
const HTTPD_BUF_SIZE: usize = 1024; // default was 2048
const HTTPD_MAX_HEADERS_COUNT: usize = 32; // default was 64

#[embassy_executor::task]
pub async fn run(
    stack: embassy_net::Stack<'static>,
    ssrcontrol_duty_sender: SsrDutyDynSender,
    ssrcontrol_duty_receiver: SsrDutyDynReceiver,
    ssrcontrol_command_sender: SsrCommandChannelSender,
    netstatus_receiver: NetStatusDynReceiver,
    tempsensor_receiver: TempSensorDynReceiver,
    memlog: SharedLogger,
) {
    let buffers = TcpBuffers::<HTTPD_HANDLERS, HTTPD_BUF_SIZE, HTTPD_BUF_SIZE>::new();
    let tcp = Tcp::new(stack, &buffers);
    let acceptor = tcp.bind(HTTPD_LISTEN_ADDR.into()).await.unwrap();

    let mut server = Server::<HTTPD_HANDLERS, HTTPD_BUF_SIZE, HTTPD_MAX_HEADERS_COUNT>::new();
    let handler = HttpHandler {
        ssrcontrol_duty_sender: Mutex::new(ssrcontrol_duty_sender),
        ssrcontrol_duty_receiver: Mutex::new(ssrcontrol_duty_receiver),
        ssrcontrol_command_sender: Mutex::new(ssrcontrol_command_sender),
        netstatus_receiver: Mutex::new(netstatus_receiver),
        tempsensor_receiver: Mutex::new(tempsensor_receiver),
        memlog,
    };
    server.run(None, acceptor, handler).await.unwrap()
}

struct HttpHandler {
    ssrcontrol_duty_sender: Mutex<NoopRawMutex, SsrDutyDynSender>,
    ssrcontrol_duty_receiver: Mutex<NoopRawMutex, SsrDutyDynReceiver>,
    ssrcontrol_command_sender: Mutex<NoopRawMutex, SsrCommandChannelSender>,
    netstatus_receiver: Mutex<NoopRawMutex, NetStatusDynReceiver>,
    tempsensor_receiver: Mutex<NoopRawMutex, TempSensorDynReceiver>,
    memlog: SharedLogger,
}

impl Handler for HttpHandler {
    type Error<E>
        = Error<E>
    where
        E: core::fmt::Debug;

    async fn handle<T, const N: usize>(
        &self,
        _task_id: impl core::fmt::Display + Copy,
        connection: &mut Connection<'_, T, N>,
    ) -> Result<(), Self::Error<T::Error>>
    where
        T: embedded_io_async::Read + embedded_io_async::Write,
    {
        let headers = connection.headers()?;

        // Parse path segments.
        let mut segments = headers.path.split('/').skip(1).take(2);

        use edge_http::Method::{Get, Post};
        let response: Result<Cow<'static, str>, (u16, &str, Option<&str>)> =
            match (headers.method, segments.next(), segments.next()) {
                //
                // GET requests.
                //

                // GET /
                (Get, Some(""), None) => Ok(HTTPD_MOTD.into()),

                // GET /help
                (Get, Some("help"), None) => {
                    let content = "\
                         GET /duty\n\
                         GET /duty/<duty>\n\
                         GET /ssr/{lock,unlock}\n\
                         GET /temp\n\
                         GET /net\n\
                         GET /log\n\
                         GET /log/clear\n\
                         GET /help\n\
                         ";
                    Ok(content.into())
                }

                // GET /duty
                (Get, Some("duty"), None) => {
                    let value = self.ssrcontrol_duty_receiver.lock().await.try_get();
                    Ok(format!("{:#?}\n", value).into())
                }

                // GET /duty/<duty>
                (Get, Some("duty"), Some(new_duty)) => match new_duty.parse::<u8>() {
                    Err(_) => Err((400, "Bad Request", None)),
                    Ok(duty) => {
                        if (0u8..=100).contains(&duty) {
                            self.ssrcontrol_duty_sender.lock().await.send(duty);
                            Ok(format!("SSR duty set to {duty}\n").into())
                        } else {
                            Err((
                                400,
                                "Bad Request",
                                Some("SSR duty must be in the [0,100] range\n"),
                            ))
                        }
                    }
                },

                // GET /ssr/{lock,unlock}
                (Get, Some("ssr"), Some(command)) => match command {
                    "lock" => {
                        self.ssrcontrol_command_sender
                            .lock()
                            .await
                            .send(SsrCommand::Lock)
                            .await;
                        Ok("SSR lock command sent\n".into())
                    }
                    "unlock" => {
                        self.ssrcontrol_command_sender
                            .lock()
                            .await
                            .send(SsrCommand::Unlock)
                            .await;
                        Ok("SSR unlock command sent\n".into())
                    }
                    _ => Err((400, "Bad Request", Some("Invalid relay command\n"))),
                },

                // GET /temp
                (Get, Some("temp"), None) => {
                    let value = self.tempsensor_receiver.lock().await.try_get();
                    Ok(format!("{:#?}\n", value).into())
                }

                // GET /net
                (Get, Some("net"), None) => {
                    let value = self.netstatus_receiver.lock().await.try_get();
                    Ok(format!("{:#?}\n", value).into())
                }

                // GET /log
                (Get, Some("log"), None) => {
                    let result = self
                        .memlog
                        .records()
                        .iter()
                        .rev()
                        .map(|record| {
                            let timestamp =
                                memlog::format_milliseconds_to_hms(record.instant.as_millis());
                            format!("[{}] {}: {}\n", timestamp, record.level, record.text)
                        })
                        .collect::<String>();
                    Ok(result.into())
                }

                // GET /log/clear
                (Get, Some("log"), Some("clear")) => {
                    self.memlog.clear();
                    Ok("Logs cleared\n".into())
                }

                // GET not found
                (Get, _, _) => Err((404, "Not Found", None)),

                //
                // POST requests.
                //

                // POST /remote
                (Post, Some("remote"), None) => {
                    return Ok(());
                }

                // POST not found
                (Post, _, _) => Err((404, "Not Found", None)),

                //
                // Unsupported methods.
                //
                _ => Err((405, "Method Not Allowed", None)),
            };

        match response {
            Ok(content) => {
                connection
                    .initiate_response(200, Some("OK"), &[("Content-Type", "text/plain")])
                    .await?;
                connection.write_all(content.as_bytes()).await
            }
            Err((code, message, content)) => {
                // If we have content for the error response:
                // (1) set the content type,
                // (2) write the content message.
                let content_type: &[(&str, &str)] = if content.is_none() {
                    &[]
                } else {
                    &[("Content-Type", "text/plain")]
                };

                connection
                    .initiate_response(code, Some(message), content_type)
                    .await?;

                if let Some(content) = content {
                    connection.write_all(content.as_bytes()).await?;
                }
                Ok(())
            }
        }

        // 200, "OK"
        // 404, "Not Found"
        // 405, "Method Not Allowed"
        // 400, "Bad Request", "cause"
    }
}

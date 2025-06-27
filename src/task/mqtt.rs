use crate::{
    futures::{Either7, select7},
    memlog::SharedLogger,
    state::SharedState,
    task::{
        net_monitor::NetStatusDynReceiver,
        ssr_control::{SsrCommandSubscriber, SsrDutyDynReceiver, SsrDutyDynSender},
        temp_sensor::TempSensorDynReceiver,
    },
};
use alloc::{
    format,
    string::{String, ToString},
};
use const_format::concatcp;
use embassy_net::{IpAddress, IpEndpoint, dns::DnsQueryType, tcp::TcpSocket};
use embassy_sync::pubsub::WaitResult;
use embassy_time::Timer;
use mountain_mqtt::{
    client::{
        Client, ClientError, ClientNoQueue, ClientReceivedEvent, ConnectionSettings, EventHandler,
        EventHandlerError,
    },
    data::{
        property::{Property, PublishProperty},
        quality_of_service::QualityOfService,
        string_pair::StringPair,
    },
    embedded_io_async::ConnectionEmbedded,
    packets::connect::Will,
};

const MQTT_SERVER_ADDR: &str = "broker.abu";
const MQTT_PORT: u16 = 1883;
const MQTT_TIMEOUT_MS: u32 = 5000;
const MQTT_PROPERTIES: usize = 16;
const MQTT_HEATER_TOPIC_ROOT: &str = "devices/heater";
use crate::config::MQTT_CLIENT_ID;
use crate::config::MQTT_TOPIC_DEVICE_NAME;

macro_rules! topic_heater {
    ($TAIL:expr) => {
        concatcp!(
            MQTT_HEATER_TOPIC_ROOT,
            '/',
            MQTT_TOPIC_DEVICE_NAME,
            '/',
            $TAIL
        )
    };
}

struct MqttDelay;
impl mountain_mqtt::client::Delay for MqttDelay {
    async fn delay_us(&mut self, us: u32) {
        Timer::after_micros(us as u64).await
    }
}

type MqttClient<'a> =
    ClientNoQueue<'a, ConnectionEmbedded<TcpSocket<'a>>, MqttDelay, MqttHandler, MQTT_PROPERTIES>;

async fn connect_to_broker<'a>(
    stack: embassy_net::Stack<'static>,
    broker_addr: IpAddress,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
    mqtt_buffer: &'a mut [u8],
    delay: MqttDelay,
    event_handler: MqttHandler,
) -> Result<MqttClient<'a>, String> {
    // Open a TCP connection to the broker.
    let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
    socket
        .connect(IpEndpoint::new(broker_addr, MQTT_PORT))
        .await
        .map_err(|err| format!("{err:?}"))?;

    // Create an MQTT client.
    let mqtt_conn = ConnectionEmbedded::new(socket);

    let mut mqtt_client = ClientNoQueue::new(
        mqtt_conn,
        mqtt_buffer,
        delay,
        MQTT_TIMEOUT_MS,
        event_handler,
    );

    // // PayloadFormatIndicator '0' -> unspecified byte stream
    // // PayloadFormatIndicator '1' -> UTF-8 encoded payload
    // let mut will_properties: heapless::Vec<_, 1> = heapless::Vec::new();
    // will_properties
    //     .push(WillProperty::PayloadFormatIndicator(
    //         PayloadFormatIndicator::new(1),
    //     ))
    //     .unwrap();

    // Set up a LWT marking the client as offline if it is disconnected.
    let will = Will::new(
        QualityOfService::Qos1,
        true,
        topic_heater!("status"),
        "offline".as_bytes(),
        heapless::Vec::<_, 0>::new(),
    );

    // Open the MQTT connection.
    mqtt_client
        .connect_with_will(
            &ConnectionSettings::unauthenticated(MQTT_CLIENT_ID),
            Some(will),
        )
        .await
        .map_err(|err| format!("{err:?}"))?;

    Ok(mqtt_client)
}

// TODO
// - send autodiscovery JSON configuration payloads
//   - https://aistudio.google.com/prompts/1xXkhLHZ8cvOB_IL_hcf-Coo9JO3_lfM1
// - send memlogs to mqtt
// + setup last will and testament
// + publish "online" to the /status topic
// + subscribe to the duty cycle command topic: /duty/set
// + send received values to duty control channel
// + confirm values to /duty
//   - TODO working?
// + send case temperature to mqtt

#[embassy_executor::task]
pub async fn run(
    stack: embassy_net::Stack<'static>,
    ssrcontrol_duty_sender: SsrDutyDynSender,
    mut ssrcontrol_duty_receiver: SsrDutyDynReceiver,
    mut netstatus_receiver: NetStatusDynReceiver,
    mut tempsensor_receiver: TempSensorDynReceiver,
    mut ssrcontrol_command_subscriber: SsrCommandSubscriber,
    memlog: SharedLogger,
    state: SharedState,
) {
    let broker_addr = 'dns: loop {
        match stack.dns_query(MQTT_SERVER_ADDR, DnsQueryType::A).await {
            Ok(mut dns_result) => match dns_result.pop() {
                Some(addr) => break 'dns addr,
                None => memlog.warn("empty dns response to broker address query"),
            },
            Err(_) => memlog.warn("failed to resolve broker address from dns"),
        };

        // Retry DNS request every 10 seconds.
        Timer::after_secs(10).await;
    };

    let mut rx_buffer = [0u8; 1024];
    let mut tx_buffer = [0u8; 1024];
    let mut mqtt_buffer = [0u8; 2048];

    // Enable log watching and get a receiver.
    memlog.enable_watch();
    let mut logwatch_receiver = memlog.watch().unwrap();

    // We continue this loop if the mqtt client is disconnected.
    'connect: loop {
        // Loop, attempting to reconnect
        let mut mqtt_client = 'client_connect: loop {
            let delay = MqttDelay;
            let event_handler = MqttHandler {
                ssrcontrol_duty_sender: ssrcontrol_duty_sender.clone(),
                memlog,
                state,
            };

            match connect_to_broker(
                stack,
                broker_addr,
                &mut rx_buffer,
                &mut tx_buffer,
                &mut mqtt_buffer,
                delay,
                event_handler,
            )
            .await
            {
                Ok(client) => break 'client_connect client,
                Err(error) => {
                    memlog.warn(format!("failed to connect to mqtt broker: {error}"));
                    Timer::after_secs(10).await;
                    continue 'client_connect;
                }
            }
        };

        // Publish an 'online' status.
        if mqtt_client
            .publish(
                topic_heater!("status"),
                "online".as_bytes(),
                QualityOfService::Qos1,
                true,
            )
            .await
            .is_err()
        {
            // Something went wrong, retry the connection.
            Timer::after_secs(10).await;
            continue 'connect;
        }

        // Subscribe to duty cycle updates.
        if mqtt_client
            .subscribe(topic_heater!("duty/set"), QualityOfService::Qos1)
            .await
            .is_err()
        {
            // Something went wrong, retry the connection.
            Timer::after_secs(10).await;
            continue 'connect;
        }

        // We continue this loop if the mqtt client throws an error but did not disconnect.
        'main: loop {
            let catch: Result<(), ClientError> = async {
                let mut ping_fut = Timer::after_secs(10);
                // Poor API design of mountain-mqtt forces us to poll periodically.
                let mut poll_fut = Timer::after_secs(1);

                '_select: loop {
                    let duty_fut = ssrcontrol_duty_receiver.changed();
                    let temp_fut = tempsensor_receiver.changed();
                    let net_fut = netstatus_receiver.changed();
                    let log_fut = logwatch_receiver.changed();
                    let ssrcmd_fut = ssrcontrol_command_subscriber.next_message();

                    match select7(
                        duty_fut,
                        temp_fut,
                        net_fut,
                        log_fut,
                        ssrcmd_fut,
                        &mut ping_fut,
                        &mut poll_fut,
                    )
                    .await
                    {
                        Either7::First(duty) => {
                            mqtt_client
                                .publish(
                                    topic_heater!("duty"),
                                    duty.to_string().as_bytes(),
                                    QualityOfService::Qos0,
                                    false,
                                )
                                .await?;
                        }

                        // Publish case temperature sensor readings.
                        Either7::Second(temp) => {
                            if let Ok(data) = temp {
                                mqtt_client
                                    .publish(
                                        topic_heater!("temp/case"),
                                        data.temperature.to_string().as_bytes(),
                                        QualityOfService::Qos0,
                                        false,
                                    )
                                    .await?;
                            }
                        }

                        // Publish network status updates.
                        Either7::Third(net) => {
                            mqtt_client
                                .publish(
                                    topic_heater!("net"),
                                    format!("{net:?}").as_bytes(),
                                    QualityOfService::Qos0,
                                    false,
                                )
                                .await?;
                        }

                        // Publish logs.
                        Either7::Fourth(log) => {
                            mqtt_client
                                .publish(
                                    topic_heater!("log"),
                                    format!("{log}").as_bytes(),
                                    QualityOfService::Qos0,
                                    false,
                                )
                                .await?;
                        }

                        // Publish SSR commands.
                        Either7::Fifth(ssr_cmd) => {
                            if let WaitResult::Message(cmd) = ssr_cmd {
                                mqtt_client
                                    .publish(
                                        topic_heater!("ssr"),
                                        format!("{cmd:?}").as_bytes(),
                                        QualityOfService::Qos0,
                                        false,
                                    )
                                    .await?;
                            }
                        }

                        // Periodically send a ping to the server.
                        Either7::Sixth(_ping) => {
                            mqtt_client.send_ping().await?;
                            ping_fut = Timer::after_secs(10);
                        }

                        // Periodic poll for MQTT messages.
                        Either7::Seventh(_timeout) => {
                            mqtt_client.poll(false).await?;
                            poll_fut = Timer::after_secs(1);
                        }
                    }
                } // 'select loop
            }
            .await; // async catch

            match catch {
                Err(ClientError::Disconnected(reason)) => {
                    memlog.info(format!("mqtt client disconnected: {reason}"));
                    continue 'connect;
                }
                Err(error) => {
                    memlog.info(format!("mqtt client error: {error}"));
                    continue 'main;
                }
                Ok(()) => (),
            }
        } // 'main loop
    } // 'connect loop
}

struct MqttHandler {
    ssrcontrol_duty_sender: SsrDutyDynSender,
    memlog: SharedLogger,
    state: SharedState,
}

impl<const P: usize> EventHandler<P> for MqttHandler {
    async fn handle_event(
        &mut self,
        event: ClientReceivedEvent<'_, P>,
    ) -> Result<(), EventHandlerError> {
        let ClientReceivedEvent::ApplicationMessage(message) = event else {
            return Ok(());
        };

        // Receive SSR duty updates and set the heater duty cycle.
        if message.topic_name.eq(topic_heater!("duty/set")) {
            let duty_str = core::str::from_utf8(message.payload)?;

            let duty: u8 = duty_str
                .parse()
                .map_err(|_| EventHandlerError::InvalidApplicationMessage)?;

            if !((0..=100).contains(&duty)) {
                return Err(EventHandlerError::UnexpectedApplicationMessage);
            }

            // Is there a UserProperty "remote:<id>" indicating that the duty setter is a remote?
            let control_remote = find_user_property(&message.properties, "remote", None)
                .map(|property| property.value());

            if let Some(remote_id) = control_remote {
                // The duty sender is a remote.
                let state_result = self.state.lock().await.remote_update_duty(remote_id, duty);

                if let Err(error) = state_result {
                    self.memlog.warn(format!("state error: {error}"));
                    return Err(EventHandlerError::UnexpectedApplicationMessage);
                }
            } else {
                // No remote indicator means the duty setting is "manual".
                self.state.lock().await.transition_to_manual(duty);
            }

            self.ssrcontrol_duty_sender.send(duty);
            return Ok(());
        }

        // Unrecognized topics.
        self.memlog
            .warn(format!("unexpected topic: {}", message.topic_name));

        // Note: we deliberately do not error on an unexpected topic.

        Ok(())
    }
}

fn find_user_property<'a, 'p, const N: usize>(
    properties: &'a heapless::Vec<PublishProperty<'p>, N>,
    name: &str,
    value: Option<&str>,
) -> Option<StringPair<'p>> {
    properties.iter().find_map(|property| {
        let PublishProperty::UserProperty(user_property) = property else {
            return None;
        };

        if user_property.value().name() != name {
            return None;
        }

        if let Some(value) = value {
            if user_property.value().value() != value {
                return None;
            }
        }

        Some(user_property.value())
    })
}

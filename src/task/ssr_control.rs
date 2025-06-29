use alloc::boxed::Box;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, pubsub, watch};
use embassy_time::{Duration, Timer};
use esp_hal::gpio;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SsrCommand {
    /// Sets the SSR duty to zero and locks it from being updated.
    Lock,
    /// Unlocks the SSR duty, allowing it to be updated.
    /// Remains set to zero until an update.
    Unlock,
}
const COMMAND_CHANNEL_CAP: usize = 2;
pub type SsrDutyWatch<const W: usize> = &'static watch::Watch<NoopRawMutex, u8, W>;
pub type SsrDutyDynSender = watch::DynSender<'static, u8>;
pub type SsrDutyDynReceiver = watch::DynReceiver<'static, u8>;
pub type SsrCommandPubSub<const S: usize, const P: usize> =
    &'static pubsub::PubSubChannel<NoopRawMutex, SsrCommand, COMMAND_CHANNEL_CAP, S, P>;
pub type SsrCommandPublisher = pubsub::DynPublisher<'static, SsrCommand>;
pub type SsrCommandSubscriber = pubsub::DynSubscriber<'static, SsrCommand>;

// The duration of each duty step.
// Smallest interval is one 50Hz mains power cycle (20ms).
// Note: SSR operate time is max. 1/2 cycle of voltage sine wave +1 ms.
// 200ms: 100 steps over 20 seconds (1000 cycles), 10 cycles per step.
const PATTERN_STEP_DURATION: Duration = Duration::from_millis(200);

/// Takes a const that sets the maximum number of watchers.
pub fn init<const DUTY_WATCHERS: usize, const CMD_SUBS: usize, const CMD_PUBS: usize>() -> (
    SsrDutyWatch<DUTY_WATCHERS>,
    SsrCommandPubSub<CMD_SUBS, CMD_PUBS>,
) {
    (
        Box::leak(Box::new(watch::Watch::new())),
        Box::leak(Box::new(pubsub::PubSubChannel::new())),
    )
}

#[embassy_executor::task]
pub async fn ssr_control(
    mut ssrcontrol_pin: gpio::Output<'static>,
    mut ssrcontrol_duty_receiver: SsrDutyDynReceiver,
    mut ssrcontrol_command_subscriber: SsrCommandSubscriber,
) {
    // Generate an initial pattern for 100% duty cycle.
    let mut pattern = generate_evenly_distributed_steps(100);

    // Locking the SSR sets its duty to zero and ignores any commands until an unlock.
    let mut is_locked = false;

    loop {
        for step in 0..100 {
            Timer::after(PATTERN_STEP_DURATION).await;

            if pattern[step] {
                ssrcontrol_pin.set_high();
            } else {
                ssrcontrol_pin.set_low();
            }

            // See if we have a lock/unlock message.
            if let Some(pubsub::WaitResult::Message(command)) =
                ssrcontrol_command_subscriber.try_next_message()
            {
                match command {
                    SsrCommand::Lock => {
                        pattern = [false; 100];
                        is_locked = true;
                    }
                    SsrCommand::Unlock => is_locked = false,
                }
            }

            // See if we have a new duty cycle.
            // We simply replace the pattern and continue from the same step position.
            // Since the pattern is evenly distributed, this puts us right into the
            // new duty cycle.
            if !is_locked {
                if let Some(new_duty_cycle) = ssrcontrol_duty_receiver.try_changed() {
                    pattern = generate_evenly_distributed_steps(new_duty_cycle);
                }
            }
        }
    }
}

/// Turns a duty cycle percentage into a pattern of on/off steps of equal duration.
///
/// These steps are evenly distributed, maximizing the number of transitions.
///
/// Example output:
///   0%: ····································································································
///   1%: ·················································o··················································
///   2%: ························o·················································o·························
///   3%: ················o································o·································o················
///   4%: ············o························o························o························o············
///   5%: ·········o···················o···················o···················o···················o··········
///   ..
///  50%: o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·o·
///   ..
///  96%: oooooooooooo·oooooooooooooooooooooooo·oooooooooooooooooooooooo·oooooooooooooooooooooooo·oooooooooooo
///  97%: oooooooooooooooo·ooooooooooooooooooooooooooooooooo·oooooooooooooooooooooooooooooooo·oooooooooooooooo
///  98%: ooooooooooooooooooooooooo·ooooooooooooooooooooooooooooooooooooooooooooooooo·oooooooooooooooooooooooo
///  99%: oooooooooooooooooooooooooooooooooooooooooooooooooo·ooooooooooooooooooooooooooooooooooooooooooooooooo
/// 100%: oooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooo
fn generate_evenly_distributed_steps(duty_percent: u8) -> [bool; 100] {
    const TOTAL_STEPS: usize = 100;
    const TOTAL_STEPS_I32: i32 = TOTAL_STEPS as i32;

    if duty_percent > 100 {
        panic!("duty cycle outside 0.100 range");
    }

    // The target number of ON steps.
    let num_on_steps_target = duty_percent as i32;

    // Initialize the output array with all steps OFF (false).
    let mut steps_array: [bool; TOTAL_STEPS] = [false; TOTAL_STEPS];

    // Initialize the accumulator.
    // Starting at `TOTAL_STEPS/2` centers the distribution of ON pulses.
    let mut accumulator: i32 = TOTAL_STEPS_I32 / 2;

    // Loop through each of the 100 steps to decide if it's ON or OFF.
    for step in steps_array.iter_mut() {
        // Add the "target density" of ON states to the accumulator.
        accumulator += num_on_steps_target;

        // Check if the accumulator has reached the threshold.
        if accumulator >= TOTAL_STEPS_I32 {
            *step = true; // This step is ON.
            // "Spend" the credit for one ON pulse by subtracting TOTAL_STEPS
            // from the accumulator.
            accumulator -= TOTAL_STEPS_I32;
        }
        // Else, the step remains OFF (false), which is its initialized state.
    }

    steps_array
}

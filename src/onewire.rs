#![allow(dead_code)]

use esp_hal::{
    delay::Delay,
    gpio,
    time::{Duration, Instant},
};

pub struct OneWireBus {
    pin: gpio::Flex<'static>,
    delay: Delay,
}

mod command {
    pub const SEARCH_NORMAL: u8 = 0xF0;
    pub const MATCH_ROM: u8 = 0x55;
    pub const SKIP_ROM: u8 = 0xCC;
    pub const SEARCH_ALARM: u8 = 0xEC;
    pub const READ_POWER_SUPPLY: u8 = 0xB4;
}

mod timing {
    // 1-Wire devices samples the 1-Wire bus during a window that lasts from
    // 15μs to 60μs after the master initiates the write time slot. If the
    // bus is high during the sampling window, a 1 is written to the device.
    // If the line is low, a 0 is written to the device.

    // All write time slots must be a minimum of 60μs in duration with a
    // minimum of a 1μs recovery time between individual write slots.
    // Both types of write time slots are initiated by the master pulling
    // the 1-Wire bus low.
    pub const RECOVERY_TIME: u32 = 1 + 1;

    // Between 60µs and 120µs.
    pub const TIME_SLOT: u32 = 60 + 20;

    // To generate a Write 1 time slot, after pulling the 1-Wire bus low,
    // the bus master must release the 1-Wire bus within 15μs. When the
    // bus is released, the 5kΩ pullup resistor will pull the bus high.

    // Between 1µs and 15µs.
    pub const WRITE_1_LOW_TIME: u32 = 5;

    // To generate a Write 0 time slot, after pulling the 1-Wire bus low,
    // the bus master must continue to hold the bus low for the duration
    // of the time slot (at least 60μs).

    // Between 60µs and 120µs.
    pub const WRITE_0_LOW_TIME: u32 = TIME_SLOT;
}

impl OneWireBus {
    pub fn new(onewire_pin: gpio::AnyPin<'static>) -> Self {
        let mut pin = gpio::Flex::new(onewire_pin);
        pin.apply_output_config(
            &gpio::OutputConfig::default()
                .with_drive_mode(gpio::DriveMode::OpenDrain)
                .with_pull(gpio::Pull::None)
                .with_drive_strength(gpio::DriveStrength::_40mA),
        );
        pin.set_output_enable(true);
        pin.set_input_enable(true);

        Self {
            pin,
            delay: Delay::new(),
        }
    }

    pub fn send_command(&mut self, command: u8, address: u64) -> Result<(), OneWireBusError> {
        self.reset()?;
        self.match_address(address);
        self.write_byte(command);

        Ok(())
    }

    /// Returns the address of the first device found on the bus.
    //
    // TODO: expand this code to loop and find all devices.
    pub fn find_first_device(&mut self) -> Result<u64, OneWireBusError> {
        // Begin the 1-Wire search algorithm to discover device addresses.
        // Each 1-Wire device has a unique 64-bit address that we'll discover bit by bit.
        self.reset()?;
        self.write_byte(command::SEARCH_NORMAL);

        // The 64-bit address we're building.
        let mut address = 0;
        // Starting bit position (0 for first search).
        let continue_start_bit = 0;
        // // Track where conflicts occurred for future searches.
        // let mut last_discrepancy_index: u8 = 0;
        // // Bitmask of positions where conflicts occurred.
        // let mut discrepancies = 0;

        // Process all 64 bits of the 1-Wire address, from LSB to MSB.
        for bit_index in continue_start_bit..64 {
            // The 1-Wire search algorithm works by having all participating devices
            // send their address bit and its complement for each bit position.
            // We read both to determine if there are conflicts (discrepancies).
            let false_bit = !self.read_bit(); // Read normal bit (inverted because bus is active-low)
            let true_bit = !self.read_bit(); // Read complement bit

            // Analyze the two bits to determine the state of this bit position.
            let chosen_bit = match (false_bit, true_bit) {
                (false, false) => {
                    // Both bits are 0: this means no devices responded to the search request.
                    // This shouldn't happen if devices are present and responding.
                    return Err(OneWireBusError::NoResponseToSearch);
                }
                (false, true) => {
                    // All remaining devices have bit=1 at this position.
                    // No conflict, all devices agree on bit value 1.
                    true
                }
                (true, false) => {
                    // All remaining devices have bit=0 at this position.
                    // No conflict, all devices agree on bit value 0.
                    false
                }
                (true, true) => {
                    // Both bits are 1: This indicates a discrepancy.
                    // Some devices have 0 and others have 1 at this bit position
                    // We need to choose a path to follow. Choosing 0 (false) will follow
                    // devices with 0 at this position, effectively discovering the device
                    // with the lowest address first.

                    // // To turn this into an iterator over every device, track discrepancies:
                    // discrepancies |= 1_u64 << (bit_index as u64);
                    // last_discrepancy_index = bit_index;

                    // Choose the lower path (0) to find lowest address first.
                    false
                }
            };

            // Build the address by setting or clearing the bit at this position.
            let address_mask = 1_u64 << (bit_index as u64);
            if chosen_bit {
                address |= address_mask;
            } else {
                address &= !address_mask;
            }

            // Send our choice back to the bus so only matching devices continue.
            // This eliminates devices that don't match our chosen path.
            self.write_bit(chosen_bit);
        }

        // Validate the discovered address.
        Self::check_crc8(&address.to_le_bytes())?;

        Ok(address)
    }

    pub fn check_crc8(data: &[u8]) -> Result<(), OneWireBusError> {
        const CRC_MAXIM: crc::Crc<u8> = crc::Crc::<u8>::new(&crc::CRC_8_MAXIM_DOW);
        if CRC_MAXIM.checksum(data) != 0 {
            return Err(OneWireBusError::ChecksumFailed);
        }
        Ok(())
    }

    /// Returns an error if no device responds to the reset pulse.
    #[inline]
    pub fn reset(&mut self) -> Result<(), OneWireBusError> {
        // During the initialization sequence the bus master transmits (TX)
        // the reset pulse by pulling the 1-Wire bus low for a minimum of 480μs.
        // The bus master then releases the bus and goes into receive mode (RX).
        // When the bus is released, the 5kΩ pullup resistor pulls the 1-Wire
        // bus high. When the DS18B20 detects this rising edge, it waits 15μs to
        // 60μs and then transmits a presence pulse by pulling the 1-Wire bus
        // low for 60μs to 240μs.

        // These are minimum times: we add a 5% margin.
        const RESET_TIME_HIGH: u32 = 480 + 24;
        const RESET_TIME_LOW: u64 = 480 + 24;

        self.wait_for_high()?;
        // Master pulls the line LOW for 480µs, then releases it.
        self.pin.set_low();
        self.delay.delay_micros(RESET_TIME_HIGH);
        self.pin.set_high();
        // Master listens for 480µs.
        // Slaves pull the line low for 60-240µs if they are present.
        let mut device_present = false;
        let start = Instant::now();
        while start.elapsed() < Duration::from_micros(RESET_TIME_LOW) {
            // Keep checking the line until we hear a slave pulling it low.
            if !device_present {
                device_present = self.pin.is_low();
            }
            self.delay.delay_micros(20);
        }

        if device_present {
            Ok(())
        } else {
            Err(OneWireBusError::DeviceNotPresent)
        }
    }

    #[inline]
    fn wait_for_high(&self) -> Result<(), OneWireBusError> {
        let instant = Instant::now();

        while instant.elapsed() < Duration::from_micros(250) {
            if self.pin.is_high() {
                return Ok(());
            }
            self.delay.delay_micros(10);
        }

        Err(OneWireBusError::BusNotHighTimeout)
    }

    #[inline]
    pub fn write_byte(&mut self, byte: u8) {
        use timing::*;

        for i in 0..8 {
            self.delay.delay_micros(RECOVERY_TIME);
            let bit_value = (byte >> i) & 1 == 1;
            self.write_bit(bit_value);
        }
    }

    #[inline]
    fn write_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.write_byte(*byte);
        }
    }

    #[inline(always)]
    fn write_bit(&mut self, value: bool) {
        use timing::*;
        if value {
            self.pin.set_low();
            self.delay.delay_micros(WRITE_1_LOW_TIME);
            self.pin.set_high();
            self.delay.delay_micros(TIME_SLOT - WRITE_1_LOW_TIME);
        } else {
            self.pin.set_low();
            self.delay.delay_micros(WRITE_0_LOW_TIME);
            self.pin.set_high();
            self.delay.delay_micros(TIME_SLOT - WRITE_0_LOW_TIME); // no-op
        }
    }

    #[inline]
    pub fn match_address(&mut self, address: u64) {
        self.write_byte(command::MATCH_ROM);
        self.write_bytes(&address.to_le_bytes());
    }

    #[inline]
    fn read_bit(&mut self) -> bool {
        self.pin.set_low();
        self.delay.delay_micros(6);
        self.pin.set_high();
        self.delay.delay_micros(9);
        let read_bit = self.pin.is_high();
        self.delay.delay_micros(55);
        read_bit
    }

    #[inline]
    fn read_byte(&mut self) -> u8 {
        // This approach reads bits LSB-first and constructs the byte accordingly.
        // (0..8) iterates from 0 to 7, representing the bit positions.
        // `fold` starts with an accumulator `0u8`.
        // For each bit_index (0 through 7):
        //   - `self.read_bit()` reads the physical bit (LSB first for 1-Wire).
        //   - If the bit is 1, it's placed at `(1 << bit_index)` in the accumulator.
        //   - If 0, the accumulator remains unchanged for that bit position.
        (0..8).fold(0u8, |byte_accumulator, bit_index| {
            if self.read_bit() {
                byte_accumulator | (1 << bit_index)
            } else {
                byte_accumulator
            }
        })
    }

    #[inline]
    pub fn read_bytes<const N: usize>(&mut self, buffer: &mut [u8; N]) {
        for entry in buffer.iter_mut() {
            *entry = self.read_byte();
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum OneWireBusError {
    /// Expected the bus to become high but timed out.
    BusNotHighTimeout,
    /// No device pulled the bus low during reset.
    DeviceNotPresent,
    /// No devices responded to the search request.
    NoResponseToSearch,
    /// CRC-8 checksum failed.
    ChecksumFailed,
}

// pub mod crc {
//     use super::OneWireBusError;

//     pub fn check_crc8(data: &[u8]) -> Result<(), OneWireBusError> {
//         let mut crc = 0;
//         for byte_val in data {
//             let mut current_byte = *byte_val;
//             for _ in 0..8 {
//                 // Extract LSB of current_byte and LSB of crc. XOR them.
//                 // `current_byte & 0x01` is the LSB of the data byte.
//                 // `crc & 0x01` is the LSB of the current CRC value.
//                 let xor_lsbs = (current_byte ^ crc) & 0x01;

//                 // Shift CRC register right by 1.
//                 crc >>= 1;

//                 // If the XOR of LSBs was 1, XOR crc with the polynomial.
//                 if xor_lsbs != 0 {
//                     crc ^= 0x8C; // Using the bit-reversed polynomial.
//                 }

//                 // Shift current_byte right to get the next bit in the next iteration.
//                 // This effectively processes the byte LSB-first.
//                 current_byte >>= 1;
//             }
//         }

//         if crc != 0 {
//             Err(OneWireBusError::ChecksumFailed)
//         } else {
//             Ok(())
//         }
//     }
// }

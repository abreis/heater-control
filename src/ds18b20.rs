#![allow(dead_code)]

use crate::onewire::{OneWireBus, OneWireBusError};
use embassy_time::Duration;

const FAMILY_CODE: u8 = 0x28;

pub struct Ds18b20 {
    address: u64,
    bus: OneWireBus,
}

impl Ds18b20 {
    pub fn new(address: u64, bus: OneWireBus) -> Result<Self, DS18B20Error> {
        if address.to_le_bytes()[0] == FAMILY_CODE {
            Ok(Self { address, bus })
        } else {
            Err(DS18B20Error::FamilyCodeMismatch)
        }
    }

    pub fn start_temp_measurement(&mut self) -> Result<(), DS18B20Error> {
        self.bus.send_command(command::CONVERT_TEMP, self.address)?;
        Ok(())
    }

    pub fn read_scratchpad(&mut self) -> Result<[u8; 9], DS18B20Error> {
        let mut scratchpad = [0; 9];

        self.bus.reset()?;
        self.bus.match_address(self.address);
        self.bus.write_byte(command::READ_SCRATCHPAD);
        self.bus.read_bytes(&mut scratchpad);
        OneWireBus::check_crc8(&scratchpad)?;
        Ok(scratchpad)
    }

    pub fn read_sensor_data(&mut self) -> Result<SensorData, DS18B20Error> {
        let scratchpad = self.read_scratchpad()?;

        let resolution = Resolution::try_from(scratchpad[4])?;

        let raw_temp = i16::from_le_bytes([scratchpad[0], scratchpad[1]]);
        let temperature = match resolution {
            Resolution::Bits12 => (raw_temp as f32) / 16.0,
            Resolution::Bits11 => (raw_temp as f32) / 8.0,
            Resolution::Bits10 => (raw_temp as f32) / 4.0,
            Resolution::Bits9 => (raw_temp as f32) / 2.0,
        };

        Ok(SensorData {
            temperature,
            resolution,
            alarm_temp_high: i8::from_le_bytes([scratchpad[2]]),
            alarm_temp_low: i8::from_le_bytes([scratchpad[3]]),
        })
    }
}

mod command {
    pub const CONVERT_TEMP: u8 = 0x44;
    pub const WRITE_SCRATCHPAD: u8 = 0x4E;
    pub const READ_SCRATCHPAD: u8 = 0xBE;
    pub const COPY_SCRATCHPAD: u8 = 0x48;
    pub const RECALL_EEPROM: u8 = 0xB8;
}

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
pub enum Resolution {
    Bits9 = 0b00011111,
    Bits10 = 0b00111111,
    Bits11 = 0b01011111,
    Bits12 = 0b01111111,
}

impl Resolution {
    pub fn max_measurement_time(&self) -> Duration {
        match self {
            Resolution::Bits9 => Duration::from_millis(94),
            Resolution::Bits10 => Duration::from_millis(188),
            Resolution::Bits11 => Duration::from_millis(375),
            Resolution::Bits12 => Duration::from_millis(750),
        }
    }
}
impl TryFrom<u8> for Resolution {
    type Error = DS18B20Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0b0001_1111 => Ok(Resolution::Bits9),
            0b0011_1111 => Ok(Resolution::Bits10),
            0b0101_1111 => Ok(Resolution::Bits11),
            0b0111_1111 => Ok(Resolution::Bits12),
            _ => Err(DS18B20Error::InvalidResolution),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DS18B20Error {
    OneWireError(OneWireBusError),
    InvalidResolution,
    FamilyCodeMismatch,
}

impl From<OneWireBusError> for DS18B20Error {
    fn from(error: OneWireBusError) -> Self {
        DS18B20Error::OneWireError(error)
    }
}

/// All of the data that can be read from the sensor.
#[derive(Clone, Copy, Debug)]
pub struct SensorData {
    /// Temperature in degrees Celsius. Defaults to 85 on startup
    pub temperature: f32,

    /// The current resolution configuration
    pub resolution: Resolution,

    /// If the last recorded temperature is lower than this, the sensor is put in an alarm state
    pub alarm_temp_low: i8,

    /// If the last recorded temperature is higher than this, the sensor is put in an alarm state
    pub alarm_temp_high: i8,
}

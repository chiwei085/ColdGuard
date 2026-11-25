//! Virtual devices. Convert physical state to bus signals.

use sim_core::Node;

pub const TMP117: u64 = 0x48;
pub const GAUGE: u64 = 0x36;

const REG_TEMP: u8 = 0x00;
const REG_CONFIG: u8 = 0x01;
const REG_ID: u8 = 0x0f;
const DEVICE_ID: u16 = 0x0117;
const CONFIG_RESET: u16 = 0x0220;
/// Bits the host may write.
const CONFIG_WRITABLE: u16 = 0x0ffc;
const DATA_READY: u16 = 0x2000;
const NO_DATA: u16 = 0x8000;
const LSB_C: f64 = 0.0078125;

/// Continuous conversion only.
pub struct Tmp117 {
    pointer: u8,
    config: u16,
    since_ms: u64,
    read: u64,
    present: bool,
}

impl Default for Tmp117 {
    fn default() -> Self {
        Self {
            pointer: 0,
            config: CONFIG_RESET,
            since_ms: 0,
            read: 0,
            present: true,
        }
    }
}

impl Tmp117 {
    /// None means no acknowledge.
    pub fn transfer(
        &mut self,
        reading: Option<f64>,
        now_ms: u64,
        write: &[u8],
        read_len: usize,
    ) -> Option<Vec<u8>> {
        let Some(celsius) = reading else {
            self.present = false;
            return None;
        };
        if !self.present {
            // A reconnected sensor starts over.
            *self = Self {
                since_ms: now_ms,
                ..Self::default()
            };
        }
        if let Some((&register, data)) = write.split_first() {
            self.pointer = register;
            if let (REG_CONFIG, [high, low]) = (register, data) {
                self.config = u16::from_be_bytes([*high, *low]) & CONFIG_WRITABLE;
                self.since_ms = now_ms;
                self.read = 0;
            }
        }
        if read_len == 0 {
            return Some(vec![]);
        }
        let done = (now_ms - self.since_ms) / self.cycle_ms();
        let value = match self.pointer {
            REG_TEMP if done == 0 => NO_DATA,
            REG_TEMP => {
                self.read = done;
                (celsius / LSB_C).round().clamp(-32767.0, 32767.0) as i16 as u16
            }
            REG_CONFIG => {
                let ready = if done > self.read { DATA_READY } else { 0 };
                self.read = done;
                self.config | ready
            }
            REG_ID => DEVICE_ID,
            _ => 0,
        };
        Some(take(value, read_len))
    }

    /// Datasheet table 7-6, CONV and AVG fields.
    fn cycle_ms(&self) -> u64 {
        let conv = usize::from(self.config >> 7 & 7);
        let floor = [16, 125, 500, 1000][usize::from(self.config >> 5 & 3)];
        [16, 125, 250, 500, 1000, 4000, 8000, 16000][conv].max(floor)
    }
}

#[derive(Default)]
pub struct Gauge {
    pointer: u8,
}

impl Gauge {
    pub fn transfer(&mut self, level: f64, write: &[u8], read_len: usize) -> Vec<u8> {
        if let Some(&register) = write.first() {
            self.pointer = register;
        }
        let value = match self.pointer {
            0x00 => (level * 10_000.0).round() as u16,
            0x02 => (3300.0 + 900.0 * level).round() as u16,
            _ => 0,
        };
        take(value, read_len)
    }
}

fn take(value: u16, len: usize) -> Vec<u8> {
    value.to_be_bytes().into_iter().take(len).collect()
}

/// STX, UID digits, XOR digits, ETX.
pub fn tag_frame(uid: &str) -> Vec<u8> {
    let sum = (0..uid.len() / 2)
        .filter_map(|i| u8::from_str_radix(&uid[2 * i..2 * i + 2], 16).ok())
        .fold(0, |a, b| a ^ b);
    let mut frame = vec![0x02];
    frame.extend(uid.bytes());
    frame.extend(format!("{sum:02X}").bytes());
    frame.push(0x03);
    frame
}

pub const PIN_RELAY: u64 = 4;
pub const PIN_DOOR: u64 = 5;
pub const PIN_MAINS: u64 = 6;
pub const PIN_BUZZER: u64 = 7;
pub const PIN_SDA: u64 = 8;
pub const PIN_SCL: u64 = 9;
pub const PIN_RFID_TX: u64 = 17;
pub const PIN_RFID_RX: u64 = 18;

/// Levels on the input pins.
pub fn input_levels(node: &Node) -> [(u64, bool); 2] {
    let open = node.thermal.as_ref().is_some_and(|t| t.door_open);
    [(PIN_DOOR, open), (PIN_MAINS, node.mains)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_register() {
        let mut t = Tmp117::default();
        let read = |t: &mut Tmp117, c, ms| t.transfer(Some(c), ms, &[REG_TEMP], 2).unwrap();
        assert_eq!(read(&mut t, 5.0, 0), [0x80, 0x00]);
        assert_eq!(read(&mut t, 5.0, 1000), [0x02, 0x80]);
        assert_eq!(read(&mut t, -1.0, 2000), [0xff, 0x80]);
    }

    #[test]
    fn data_ready_clears_on_read_and_follows_the_cycle() {
        let mut t = Tmp117::default();
        t.transfer(Some(5.0), 0, &[REG_CONFIG, 0x00, 0xa0], 0);
        let mut ready = |ms| t.transfer(Some(5.0), ms, &[REG_CONFIG], 2).unwrap()[0] & 0x20 != 0;
        assert!(!ready(124));
        assert!(ready(125));
        assert!(!ready(126));
        assert!(ready(250));
    }

    #[test]
    fn disconnected_sensor_nacks_then_resets() {
        let mut t = Tmp117::default();
        t.transfer(Some(5.0), 0, &[REG_CONFIG, 0x00, 0xa0], 0);
        assert_eq!(t.transfer(None, 500, &[REG_TEMP], 2), None);
        let config = t.transfer(Some(5.0), 600, &[REG_CONFIG], 2).unwrap();
        assert_eq!(config, [0x02, 0x20]);
    }

    #[test]
    fn gauge_and_tag_formats() {
        let mut g = Gauge::default();
        assert_eq!(g.transfer(0.5, &[0x00], 2), 5000u16.to_be_bytes());
        assert_eq!(g.transfer(1.0, &[0x02], 2), 4200u16.to_be_bytes());
        // 04 ^ A1 ^ B2 ^ C3 = D4.
        assert_eq!(tag_frame("04A1B2C3"), *b"\x0204A1B2C3D4\x03");
    }
}

//! Hand-coded esp_cosim.v1 subset. Claims the clock and the board's devices.

use crate::devices::{
    CLOCK, GAUGE, PIN_BUZZER, PIN_DOOR, PIN_MAINS, PIN_RELAY, PIN_RFID_RX, PIN_RFID_TX, PIN_SCL,
    PIN_SDA, TMP117,
};
use sim_core::STEP_MS;
use std::{
    io::{BufReader, Error, ErrorKind, Read, Result, Write},
    net::{Ipv4Addr, TcpStream},
    time::{Duration, Instant},
};

const GPIO_LEVEL: u64 = 3;
const REGISTER: u64 = 4;
const REGISTER_ACK: u64 = 5;
const GPIO_DRIVE: u64 = 6;
const BUS_REQUEST: u64 = 7;
const BUS_REPLY: u64 = 8;
const CHIP_RESET: u64 = 9;
const UART_RX: u64 = 11;
const EN_DRIVE: u64 = 34;
const TIME_GRANT: u64 = 40;
const TIME_WAITING: u64 = 41;

const CLAIM_GPIO: u64 = 1;
const CLAIM_I2C: u64 = 2;
const CLAIM_UART: u64 = 4;
const CLAIM_EN: u64 = 19;
const CLAIM_TIME: u64 = 23;

const BUS_OP_I2C: u64 = 1;

/// One lockstep quantum.
pub const QUANTUM_NS: u64 = STEP_MS * 1_000_000;

const MAX_FRAME: usize = 256 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

/// One connection carries everything. Devices answer inside the lockstep loop,
/// so inputs and replies keep a fixed order.
pub struct Cosim {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
}

pub enum Event {
    /// Chip time in nanoseconds.
    Waiting(u64),
    Reset,
    Level {
        pin: u64,
        level: bool,
    },
    Bus {
        id: u64,
        address: u64,
        write: Vec<u8>,
        read_len: usize,
    },
}

impl Cosim {
    /// Claim the clock and every board device. The chip stops until a grant.
    pub fn attach(port: u16, deadline: Duration) -> Result<Self> {
        let gpio = [
            uint(1, 1 << PIN_DOOR | 1 << PIN_MAINS),
            uint(2, 1 << PIN_RELAY | 1 << PIN_BUZZER),
        ]
        .concat();
        let i2c = |address| [uint(1, address), uint(2, PIN_SCL), uint(3, PIN_SDA)].concat();
        let uart = [uint(1, PIN_RFID_TX), uint(2, PIN_RFID_RX)].concat();
        let mut cosim = Self::register(
            port,
            deadline,
            &[
                (CLAIM_TIME, vec![]),
                (CLAIM_GPIO, gpio),
                (CLAIM_I2C, i2c(TMP117)),
                (CLAIM_I2C, i2c(GAUGE)),
                (CLAIM_I2C, i2c(CLOCK)),
                (CLAIM_UART, uart),
                (CLAIM_EN, vec![]),
            ],
        )?;
        cosim.wait()?;
        Ok(cosim)
    }

    fn register(port: u16, deadline: Duration, claims: &[(u64, Vec<u8>)]) -> Result<Self> {
        let end = Instant::now() + deadline;
        let stream = loop {
            match TcpStream::connect((Ipv4Addr::LOCALHOST, port)) {
                Ok(s) => break s,
                Err(e) if Instant::now() > end => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(TIMEOUT))?;
        let reader = BufReader::new(stream.try_clone()?);
        let mut cosim = Cosim { stream, reader };
        let mut register = [uint(1, 1), bytes(2, b"coldguard")].concat();
        for (field, claim) in claims {
            register.extend(bytes(3, &bytes(*field, claim)));
        }
        cosim.send(&bytes(REGISTER, &register))?;
        let raw = loop {
            let raw = cosim.recv()?;
            if get(&decode(&raw)?, REGISTER_ACK).is_some() {
                break raw;
            }
        };
        let message = decode(&raw)?;
        let ack = decode(blob(&message, REGISTER_ACK))?;
        if int(&ack, 5) != 1 {
            let reason = String::from_utf8_lossy(blob(&ack, 6)).into_owned();
            return Err(invalid(&format!("esp-emu rejected registration: {reason}")));
        }
        Ok(cosim)
    }

    pub fn power_cycle(&mut self) -> Result<()> {
        for level in [false, true] {
            self.enable(level)?;
            self.grant(QUANTUM_NS)?;
            self.wait()?;
        }
        Ok(())
    }

    pub fn grant(&mut self, ns: u64) -> Result<()> {
        self.send(&bytes(TIME_GRANT, &uint(1, ns)))
    }

    pub fn drive(&mut self, pin: u64, level: bool) -> Result<()> {
        let body = [uint(1, pin), uint(2, level.into())].concat();
        self.send(&bytes(GPIO_DRIVE, &body))
    }

    /// Hold the chip in reset while false.
    pub fn enable(&mut self, level: bool) -> Result<()> {
        self.send(&bytes(EN_DRIVE, &uint(1, level.into())))
    }

    pub fn uart(&mut self, data: &[u8]) -> Result<()> {
        self.send(&bytes(UART_RX, &bytes(1, data)))
    }

    /// None is a NACK.
    pub fn reply(&mut self, id: u64, data: Option<&[u8]>) -> Result<()> {
        let body = [
            uint(1, id),
            uint(2, data.is_some().into()),
            bytes(3, data.unwrap_or_default()),
            uint(4, BUS_OP_I2C),
        ]
        .concat();
        self.send(&bytes(BUS_REPLY, &body))
    }

    /// Wait until the chip stops. Return its time in nanoseconds.
    fn wait(&mut self) -> Result<u64> {
        loop {
            if let Event::Waiting(ns) = self.event()? {
                return Ok(ns);
            }
        }
    }

    /// Wait for the next message that matters to a board.
    pub fn event(&mut self) -> Result<Event> {
        loop {
            let raw = self.recv()?;
            let message = decode(&raw)?;
            if get(&message, TIME_WAITING).is_some() {
                return Ok(Event::Waiting(int(&message, 1)));
            }
            if get(&message, CHIP_RESET).is_some() {
                return Ok(Event::Reset);
            }
            if get(&message, GPIO_LEVEL).is_some() {
                let body = decode(blob(&message, GPIO_LEVEL))?;
                return Ok(Event::Level {
                    pin: int(&body, 1),
                    level: int(&body, 2) == 1,
                });
            }
            if get(&message, BUS_REQUEST).is_some() {
                let body = decode(blob(&message, BUS_REQUEST))?;
                let i2c = decode(blob(&body, 2))?;
                return Ok(Event::Bus {
                    id: int(&body, 1),
                    address: int(&i2c, 2),
                    write: blob(&i2c, 3).to_vec(),
                    read_len: int(&i2c, 4) as usize,
                });
            }
        }
    }

    fn send(&mut self, message: &[u8]) -> Result<()> {
        let mut frame = (message.len() as u32).to_le_bytes().to_vec();
        frame.extend(message);
        self.stream.write_all(&frame)
    }

    /// Return one message without its topic.
    fn recv(&mut self) -> Result<Vec<u8>> {
        let mut len = [0; 4];
        self.reader.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as usize;
        if !(2..=MAX_FRAME).contains(&len) {
            return Err(invalid("Invalid cosim frame length"));
        }
        let mut frame = vec![0; len];
        self.reader.read_exact(&mut frame)?;
        let topic = u16::from_le_bytes([frame[0], frame[1]]) as usize;
        if 2 + topic > len {
            return Err(invalid("Invalid cosim topic length"));
        }
        frame.drain(..2 + topic);
        Ok(frame)
    }
}

/// Minimal clock holder for the environment checks.
pub struct Clock(Cosim);

impl Clock {
    /// Return the stopped time in nanoseconds.
    pub fn claim(port: u16, deadline: Duration) -> Result<(Self, u64)> {
        let mut clock = Clock(Cosim::register(port, deadline, &[(CLAIM_TIME, vec![])])?);
        let now = clock.0.wait()?;
        Ok((clock, now))
    }

    /// Advance by one grant. Return the stopped time in nanoseconds.
    pub fn advance(&mut self, ns: u64) -> Result<u64> {
        self.0.grant(ns)?;
        self.0.wait()
    }
}

#[derive(Debug, PartialEq)]
enum Field<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = vec![];
    while n > 0x7f {
        out.push(n as u8 | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
    out
}

fn uint(field: u64, value: u64) -> Vec<u8> {
    [varint(field << 3), varint(value)].concat()
}

fn bytes(field: u64, value: &[u8]) -> Vec<u8> {
    [
        varint(field << 3 | 2),
        varint(value.len() as u64),
        value.to_vec(),
    ]
    .concat()
}

fn read_varint(buf: &mut &[u8]) -> Result<u64> {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let (&b, rest) = buf
            .split_first()
            .ok_or_else(|| invalid("Truncated protobuf message"))?;
        *buf = rest;
        value |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(invalid("Protobuf varint is too long"))
}

fn decode(mut buf: &[u8]) -> Result<Vec<(u64, Field<'_>)>> {
    let mut out = vec![];
    while !buf.is_empty() {
        let tag = read_varint(&mut buf)?;
        let len = match tag & 7 {
            0 => {
                out.push((tag >> 3, Field::Int(read_varint(&mut buf)?)));
                continue;
            }
            1 => 8,
            2 => read_varint(&mut buf)? as usize,
            5 => 4,
            _ => return Err(invalid("Invalid protobuf wire type")),
        };
        let (head, rest) = buf
            .split_at_checked(len)
            .ok_or_else(|| invalid("Truncated protobuf message"))?;
        out.push((tag >> 3, Field::Bytes(head)));
        buf = rest;
    }
    Ok(out)
}

fn get<'a, 'b>(message: &'b [(u64, Field<'a>)], field: u64) -> Option<&'b Field<'a>> {
    message.iter().find(|(n, _)| *n == field).map(|(_, v)| v)
}

fn int(message: &[(u64, Field)], field: u64) -> u64 {
    match get(message, field) {
        Some(Field::Int(n)) => *n,
        _ => 0,
    }
}

fn blob<'a>(message: &[(u64, Field<'a>)], field: u64) -> &'a [u8] {
    match get(message, field) {
        Some(Field::Bytes(b)) => b,
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_wire_format() {
        // Field 40 and length 4. Inner field 1 is 1000000.
        let grant = bytes(TIME_GRANT, &uint(1, 1_000_000));
        assert_eq!(grant, [0xc2, 0x02, 0x04, 0x08, 0xc0, 0x84, 0x3d]);
        let message = decode(&grant).unwrap();
        let inner = blob(&message, TIME_GRANT);
        assert_eq!(decode(inner).unwrap(), [(1, Field::Int(1_000_000))]);
    }

    #[test]
    fn malformed_input_is_an_error() {
        assert!(decode(&[0x0a, 0x05, 0x00]).is_err());
        assert!(decode(&[0xff; 11]).is_err());
    }
}

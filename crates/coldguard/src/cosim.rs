//! Minimal esp_cosim.v1 clock holder.
// ponytail: hand-coded messages, use prost when the coordinator needs devices.

use std::{
    io::{Error, ErrorKind, Read, Result, Write},
    net::{Ipv4Addr, TcpStream},
    time::{Duration, Instant},
};

const REGISTER: u64 = 4;
const REGISTER_ACK: u64 = 5;
const TIME_GRANT: u64 = 40;
const TIME_WAITING: u64 = 41;
const CLAIM_TIME: u64 = 23;

const MAX_FRAME: usize = 256 * 1024;

pub struct Clock {
    stream: TcpStream,
}

impl Clock {
    /// Register as a clock holder. The chip stops until the first grant.
    /// Return the stopped time in nanoseconds.
    pub fn claim(port: u16, deadline: Duration) -> Result<(Self, u64)> {
        let end = Instant::now() + deadline;
        let stream = loop {
            match TcpStream::connect((Ipv4Addr::LOCALHOST, port)) {
                Ok(s) => break s,
                Err(e) if Instant::now() > end => return Err(e),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut clock = Clock { stream };
        let claim = bytes(CLAIM_TIME, &[]);
        let register = [uint(1, 1), bytes(2, b"coldguard-doctor"), bytes(3, &claim)].concat();
        clock.send(&bytes(REGISTER, &register))?;
        let ack = loop {
            if let Some(ack) = find(&clock.recv()?, REGISTER_ACK)? {
                break decode(&ack)?;
            }
        };
        if !matches!(get(&ack, 5), Some(Field::Int(1))) {
            let reason = match get(&ack, 6) {
                Some(Field::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
                _ => "unspecified".into(),
            };
            return Err(invalid(&format!(
                "esp-emu rejected clock registration: {reason}"
            )));
        }
        let now = clock.wait()?;
        Ok((clock, now))
    }

    /// Advance by one grant. Return the stopped time in nanoseconds.
    pub fn advance(&mut self, ns: u64) -> Result<u64> {
        self.send(&bytes(TIME_GRANT, &uint(1, ns)))?;
        self.wait()
    }

    fn wait(&mut self) -> Result<u64> {
        loop {
            let message = decode(&self.recv()?)?;
            if get(&message, TIME_WAITING).is_some() {
                return Ok(match get(&message, 1) {
                    Some(Field::Int(ns)) => *ns,
                    _ => 0,
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
        self.stream.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as usize;
        if !(2..=MAX_FRAME).contains(&len) {
            return Err(invalid("Invalid cosim frame length"));
        }
        let mut frame = vec![0; len];
        self.stream.read_exact(&mut frame)?;
        let topic = u16::from_le_bytes([frame[0], frame[1]]) as usize;
        frame
            .get(2 + topic..)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| invalid("Invalid cosim topic length"))
    }
}

#[derive(Debug, PartialEq)]
enum Field {
    Int(u64),
    Bytes(Vec<u8>),
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

fn decode(mut buf: &[u8]) -> Result<Vec<(u64, Field)>> {
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
        out.push((tag >> 3, Field::Bytes(head.to_vec())));
        buf = rest;
    }
    Ok(out)
}

fn get(message: &[(u64, Field)], field: u64) -> Option<&Field> {
    message.iter().find(|(n, _)| *n == field).map(|(_, v)| v)
}

fn find(message: &[u8], field: u64) -> Result<Option<Vec<u8>>> {
    Ok(decode(message)?.into_iter().find_map(|(n, v)| match v {
        Field::Bytes(b) if n == field => Some(b),
        _ => None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_wire_format() {
        // Field 40 and length 4. Inner field 1 is 1000000.
        let grant = bytes(TIME_GRANT, &uint(1, 1_000_000));
        assert_eq!(grant, [0xc2, 0x02, 0x04, 0x08, 0xc0, 0x84, 0x3d]);
        let inner = find(&grant, TIME_GRANT).unwrap().unwrap();
        assert_eq!(decode(&inner).unwrap(), [(1, Field::Int(1_000_000))]);
    }

    #[test]
    fn malformed_input_is_an_error() {
        assert!(decode(&[0x0a, 0x05, 0x00]).is_err());
        assert!(decode(&[0xff; 11]).is_err());
    }
}

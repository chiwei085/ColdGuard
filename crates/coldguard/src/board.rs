//! One emulated unit in lockstep with the world.

use crate::{
    cosim::{Cosim, Event},
    devices::{self, GAUGE, Gauge, PIN_BUZZER, PIN_RELAY, TMP117, Tmp117, tag_frame},
    smoke::{Emulator, NET, free_port},
    toolchain::Toolchain,
};
use sim_core::{Node, NodeId, STEP_MS};
use std::{fs, path::PathBuf, time::Duration};

type Result<T = ()> = std::result::Result<T, String>;

const CFG_OFFSET: usize = 0x190000;
const CFG_SIZE: usize = 0x1000;
const LOG_OFFSET: usize = 0x191000;
const LOG_SIZE: usize = 0x110000;
const QUANTUM_NS: u64 = STEP_MS * 1_000_000;

pub struct Board {
    pub id: NodeId,
    pub emulator: Emulator,
    cosim: Cosim,
    thermometer: Tmp117,
    gauge: Gauge,
    levels: [(u64, Option<bool>); 2],
    powered: bool,
    scans: u64,
    /// Undriven outputs read high. A board pulls them low.
    driven: [Option<bool>; 2],
    /// World time, pin and level of each output change.
    pub trace: Vec<(u64, u64, bool)>,
    /// Chip time at the last stop.
    pub stopped_ns: u64,
    control: u16,
    image: PathBuf,
}

impl Board {
    /// Broker port 0 keeps the unit offline.
    pub fn start(t: &Toolchain, id: NodeId, broker: u16) -> Result<Self> {
        let source = t.node_image();
        let mut flash =
            fs::read(&source).map_err(|e| format!("{}: {e}. Run setup", source.display()))?;
        if flash.len() < LOG_OFFSET + LOG_SIZE {
            flash.resize(LOG_OFFSET + LOG_SIZE, 0xff);
        }
        let name = id.name();
        let text = format!("{name} {broker}\0");
        flash[CFG_OFFSET..CFG_OFFSET + CFG_SIZE].fill(0xff);
        flash[CFG_OFFSET..CFG_OFFSET + text.len()].copy_from_slice(text.as_bytes());
        // Ports are unique, so concurrent labs do not share images.
        let (port, control) = (free_port()?, free_port()?);
        let image = t.tools().join(format!("unit-{port}.bin"));
        fs::write(&image, flash).map_err(|e| e.to_string())?;

        let mut command = t.plain(t.esp_emu());
        command
            .args(["--chip", "esp32s3", "--net", NET, "--firmware"])
            .arg(&image)
            .arg("--cosim-tcp")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--control-tcp")
            .arg(format!("127.0.0.1:{control}"));
        let emulator = Emulator::spawn(command)?;
        let mut cosim = Cosim::attach(port, Duration::from_secs(10)).map_err(|e| e.to_string())?;
        // The emulator keeps its flash in memory.
        let _ = fs::remove_file(&image);
        // The chip ran freely until it was claimed. Power cycle it so that
        // every run starts from the same instant.
        cosim.power_cycle().map_err(|e| e.to_string())?;
        Ok(Self {
            id,
            emulator,
            cosim,
            thermometer: Tmp117::default(),
            gauge: Gauge::default(),
            levels: [(devices::PIN_DOOR, None), (devices::PIN_MAINS, None)],
            powered: true,
            scans: 0,
            driven: [None; 2],
            trace: vec![],
            stopped_ns: 0,
            control,
            image,
        })
    }

    pub fn relay(&self) -> bool {
        self.powered && self.driven[0] == Some(true)
    }

    pub fn buzzer(&self) -> bool {
        self.powered && self.driven[1] == Some(true)
    }

    /// Apply the world to the pins, then let the chip run one quantum.
    pub fn begin(&mut self, node: &Node) -> Result {
        let io = |e: std::io::Error| e.to_string();
        for ((pin, last), (_, level)) in self.levels.iter_mut().zip(devices::input_levels(node)) {
            if *last != Some(level) {
                *last = Some(level);
                self.cosim.drive(*pin, level).map_err(io)?;
            }
        }
        if self.powered != node.powered {
            self.powered = node.powered;
            self.driven = [None; 2];
            self.cosim.enable(node.powered).map_err(io)?;
        }
        if node.scan_count > self.scans
            && let Some(uid) = &node.last_scan
        {
            let mut frame = tag_frame(uid);
            if node.scan_garbled {
                frame[4] ^= 1;
            }
            self.cosim.uart(&frame).map_err(io)?;
        }
        self.scans = node.scan_count;
        self.cosim.grant(QUANTUM_NS).map_err(io)
    }

    /// Serve the devices until the chip has used the quantum.
    pub fn finish(&mut self, node: &Node, now_ms: u64) -> Result {
        loop {
            match self.cosim.event().map_err(|e| self.lost(e))? {
                Event::Waiting(ns) => {
                    self.stopped_ns = ns;
                    return Ok(());
                }
                Event::Reset => self.driven = [None; 2],
                Event::Level { pin, level } => {
                    let index = match pin {
                        PIN_RELAY => 0,
                        PIN_BUZZER => 1,
                        _ => continue,
                    };
                    let driven = &mut self.driven[index];
                    if *driven != Some(level) && (driven.is_some() || !level) {
                        *driven = Some(level);
                        self.trace.push((now_ms, pin, level));
                    }
                }
                Event::Bus {
                    id,
                    address,
                    write,
                    read_len,
                } => {
                    let data = match address {
                        TMP117 => {
                            node.thermal.as_ref().map(|t| t.reading_c()).and_then(|r| {
                                self.thermometer.transfer(r, now_ms, &write, read_len)
                            })
                        }
                        GAUGE => Some(self.gauge.transfer(node.battery_level, &write, read_len)),
                        _ => None,
                    };
                    self.cosim
                        .reply(id, data.as_deref())
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }

    /// Flash log as stored by the firmware.
    pub fn log(&self) -> Result<Vec<u8>> {
        use std::io::{BufRead, BufReader, Write};
        let out = self.image.with_extension("log");
        let mut s =
            std::net::TcpStream::connect(("127.0.0.1", self.control)).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(10)))
            .and_then(|()| writeln!(s, "read-region {LOG_OFFSET} {LOG_SIZE} {}", out.display()))
            .map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(s)
            .read_line(&mut reply)
            .map_err(|e| e.to_string())?;
        let data = fs::read(&out).map_err(|e| format!("{}: {e}", reply.trim()));
        let _ = fs::remove_file(out);
        data
    }

    /// Reset as a brown-out would. Flash survives.
    pub fn reset(&self) -> Result {
        use std::io::Write;
        std::net::TcpStream::connect(("127.0.0.1", self.control))
            .and_then(|mut s| writeln!(s, "reset"))
            .map_err(|e| e.to_string())
    }

    fn lost(&self, e: std::io::Error) -> String {
        format!("{:?}: {e}. {}", self.id, self.emulator.tail())
    }
}

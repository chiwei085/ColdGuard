//! Connection to the simulation server.

use bevy::prelude::Resource;
use sim_core::{Action, Control, Frame, LIVE_PORT};
use std::{
    io::{BufRead, BufReader, Write},
    net::{Ipv4Addr, TcpStream},
    sync::{Arc, Mutex},
    thread::{sleep, spawn},
    time::Duration,
};

#[derive(Default)]
struct Shared {
    frame: Option<Frame>,
    stream: Option<TcpStream>,
}

#[derive(Resource, Clone, Default)]
pub struct Link(Arc<Mutex<Shared>>);

impl Link {
    /// Connect in the background and reconnect after failures.
    pub fn start() -> Self {
        let link = Self::default();
        let shared = link.0.clone();
        spawn(move || {
            loop {
                if let Ok(stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, LIVE_PORT)) {
                    shared.lock().unwrap().stream = stream.try_clone().ok();
                    for line in BufReader::new(stream).lines().map_while(Result::ok) {
                        match serde_json::from_str(&line) {
                            Ok(frame) => shared.lock().unwrap().frame = Some(frame),
                            Err(_) => break,
                        }
                    }
                    *shared.lock().unwrap() = Shared::default();
                }
                sleep(Duration::from_millis(200));
            }
        });
        link
    }

    /// Latest state, or none when disconnected.
    pub fn frame(&self) -> Option<Frame> {
        self.0.lock().unwrap().frame.clone()
    }

    pub fn send(&self, control: &Control) {
        let mut shared = self.0.lock().unwrap();
        if let (Some(stream), Ok(mut line)) = (&mut shared.stream, serde_json::to_vec(control)) {
            line.push(b'\n');
            // The reader thread handles a lost connection.
            let _ = stream.write_all(&line);
        }
    }

    pub fn act(&self, input: Action) {
        self.send(&Control::Act { input });
    }
}

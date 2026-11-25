//! Network cable between one unit and the broker.

use std::{
    io::{ErrorKind, Read, Write, copy},
    net::{Ipv4Addr, Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{sleep, spawn},
    time::Duration,
};

pub struct Proxy {
    pub port: u16,
    up: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    open: Arc<Mutex<Vec<TcpStream>>>,
}

impl Proxy {
    pub fn start(broker: u16) -> Result<Self, String> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let proxy = Self {
            port: listener.local_addr().map_err(|e| e.to_string())?.port(),
            up: Arc::new(AtomicBool::new(true)),
            stop: Arc::default(),
            open: Arc::default(),
        };
        let (up, stop, open) = (proxy.up.clone(), proxy.stop.clone(), proxy.open.clone());
        spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((unit, _)) => {
                        // Holding the lock keeps a cut from missing this link.
                        let mut open = open.lock().unwrap();
                        if up.load(Ordering::Relaxed)
                            && let Ok(links) = join(unit, broker)
                        {
                            open.retain(|s| s.peer_addr().is_ok());
                            open.extend(links);
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => sleep(Duration::from_millis(5)),
                    Err(_) => return,
                }
            }
        });
        Ok(proxy)
    }

    pub fn set(&self, up: bool) {
        if self.up.load(Ordering::Relaxed) != up {
            let mut open = self.open.lock().unwrap();
            self.up.store(up, Ordering::Relaxed);
            if !up {
                for s in open.drain(..) {
                    let _ = s.shutdown(Shutdown::Both);
                }
            }
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.set(false);
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn join(unit: TcpStream, broker: u16) -> std::io::Result<[TcpStream; 2]> {
    unit.set_nonblocking(false)?;
    let far = TcpStream::connect((Ipv4Addr::LOCALHOST, broker))?;
    unit.set_nodelay(true)?;
    far.set_nodelay(true)?;
    for (from, to) in [(&unit, &far), (&far, &unit)] {
        pump(from.try_clone()?, to.try_clone()?);
    }
    Ok([unit, far])
}

fn pump(mut from: impl Read + Send + 'static, to: TcpStream) {
    spawn(move || {
        let mut out = &to;
        let _ = copy(&mut from, &mut out);
        let _ = out.flush();
        let _ = to.shutdown(Shutdown::Both);
    });
}

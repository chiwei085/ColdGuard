//! MQTT envelopes, durable records and confirmations.

use rumqttc::{Client, Event, MqttOptions, Packet, QoS};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sim_core::{Action, Batch, Entry, Fleet, NodeId};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{sleep, spawn},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Result<T = ()> = std::result::Result<T, String>;

/// Three missed heartbeats.
const OFFLINE_MS: u64 = 15_000;
const SHOWN_EVENTS: usize = 200;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    node_id: String,
    boot_id: u32,
    seq: u32,
    sim_time_ms: u64,
    r#type: String,
    body: String,
    checksum: u32,
}

#[derive(Default)]
struct Shared {
    fleet: Fleet,
    heartbeats: [Option<u64>; 3],
    telemetry: [u64; 3],
    counts: [(u64, u64); 3],
}

pub struct Hub {
    client: Client,
    shared: Arc<Mutex<Shared>>,
    time_ms: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    /// Emulates a host that cannot store records.
    pub reject: Arc<AtomicBool>,
    session: u32,
    seq: u32,
}

/// CRC-32/ISO-HDLC.
fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0, |crc: u32, b| {
        (0..8).fold(crc ^ u32::from(*b), |c, _| {
            (c >> 1) ^ (0xedb8_8320 & (c & 1).wrapping_neg())
        })
    })
}

fn open(text: &[u8], node: NodeId) -> Option<Envelope> {
    let e: Envelope = serde_json::from_slice(text).ok()?;
    (e.version == 1 && e.node_id == node.name() && e.checksum == crc32(e.body.as_bytes()))
        .then_some(e)
}

fn publish(client: &Client, node: NodeId, kind: &str, boot: u32, seq: u32, body: Value) -> Result {
    let body = body.to_string();
    let envelope = Envelope {
        version: 1,
        node_id: "host".into(),
        boot_id: boot,
        seq,
        sim_time_ms: 0,
        r#type: kind.into(),
        checksum: crc32(body.as_bytes()),
        body,
    };
    let text = serde_json::to_vec(&envelope).map_err(|e| e.to_string())?;
    let topic = format!("coldguard/v1/{}/{kind}", node.name());
    client
        .try_publish(topic, QoS::AtLeastOnce, false, text)
        .map_err(|e| e.to_string())
}

pub fn database(path: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         CREATE TABLE IF NOT EXISTS records (
             session INTEGER NOT NULL,
             node TEXT NOT NULL,
             boot INTEGER NOT NULL,
             seq INTEGER NOT NULL,
             time_ms INTEGER NOT NULL,
             type TEXT NOT NULL,
             body TEXT NOT NULL,
             PRIMARY KEY (session, node, boot, seq)
         ) WITHOUT ROWID;",
    )?;
    Ok(db)
}

/// Write the records as JSON lines, oldest first.
pub fn export(
    path: &Path,
    latest: bool,
    out: &mut impl std::io::Write,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    if !path.exists() {
        return Err(format!("{}: no records", path.display()).into());
    }
    let db = database(path)?;
    let mut query = db.prepare(
        "SELECT session, node, boot, seq, time_ms, type, body FROM records
         WHERE NOT ?1 OR session = (SELECT max(session) FROM records)
         ORDER BY session, time_ms, node, boot, seq",
    )?;
    let mut rows = query.query([latest])?;
    while let Some(r) = rows.next()? {
        let body: String = r.get(6)?;
        let body: Value = serde_json::from_str(&body).unwrap_or(Value::String(body));
        let line = json!({
            "session": r.get::<_, i64>(0)?, "node": r.get::<_, String>(1)?,
            "boot": r.get::<_, i64>(2)?, "seq": r.get::<_, i64>(3)?,
            "time_ms": r.get::<_, i64>(4)?, "type": r.get::<_, String>(5)?, "body": body,
        });
        writeln!(out, "{line}")?;
    }
    Ok(())
}

impl Hub {
    pub fn start(port: u16, db: &Path) -> Result<Self> {
        // Flash starts blank in each session, so boot numbers repeat.
        let session = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32);
        let db = database(db).map_err(|e| e.to_string())?;
        let mut options = MqttOptions::new("host", "127.0.0.1", port);
        options.set_keep_alive(Duration::from_secs(30));
        options.set_max_packet_size(65536, 65536);
        let (client, mut connection) = Client::new(options, 256);
        let hub = Self {
            client: client.clone(),
            shared: Arc::default(),
            time_ms: Arc::default(),
            stop: Arc::default(),
            reject: Arc::default(),
            session,
            seq: 0,
        };
        let (shared, time, stop) = (hub.shared.clone(), hub.time_ms.clone(), hub.stop.clone());
        let reject = hub.reject.clone();
        spawn(move || {
            for event in connection.iter() {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                match event {
                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        for kind in ["telemetry", "heartbeat", "event"] {
                            let topic = format!("coldguard/v1/+/{kind}");
                            let _ = client.try_subscribe(topic, QoS::AtLeastOnce);
                        }
                    }
                    Ok(Event::Incoming(Packet::Publish(p)))
                        if reject.load(Ordering::Relaxed) && p.topic.ends_with("/event") => {}
                    Ok(Event::Incoming(Packet::Publish(p))) => {
                        let now = time.load(Ordering::Relaxed);
                        if let Err(e) =
                            receive(&p.topic, &p.payload, now, session, &db, &client, &shared)
                        {
                            eprintln!("MQTT {}: {e}", p.topic);
                        }
                    }
                    Ok(_) => {}
                    Err(_) => sleep(Duration::from_millis(200)),
                }
            }
        });
        Ok(hub)
    }

    pub fn tick(&self, time_ms: u64) {
        self.time_ms.store(time_ms, Ordering::Relaxed);
    }

    pub fn fleet(&self) -> Fleet {
        let shared = self.shared.lock().unwrap();
        let mut fleet = shared.fleet.clone();
        let now = self.time_ms.load(Ordering::Relaxed);
        for (id, heard) in NodeId::ALL.into_iter().zip(shared.heartbeats) {
            fleet.report_mut(id).online =
                heard.is_some_and(|t| now.saturating_sub(t) <= OFFLINE_MS);
            fleet.report_mut(id).age_ms = now.saturating_sub(shared.telemetry[id as usize]);
        }
        fleet
    }

    /// Last reported transit state of the carrier.
    pub fn transit(&self) -> u64 {
        self.shared.lock().unwrap().fleet.carrier.transit
    }

    /// Telemetry and heartbeat messages received from a node.
    pub fn counts(&self, id: NodeId) -> (u64, u64) {
        self.shared.lock().unwrap().counts[id as usize]
    }

    pub fn command(&mut self, action: &Action) -> Result {
        let Action::Operate {
            node,
            cmd,
            batch,
            to,
            note,
        } = action
        else {
            return Ok(());
        };
        self.seq += 1;
        let body = json!({"cmd": cmd, "batch": batch, "to": to, "note": note});
        publish(&self.client, *node, "command", self.session, self.seq, body)
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.client.try_disconnect();
    }
}

fn receive(
    topic: &str,
    payload: &[u8],
    now: u64,
    session: u32,
    db: &Connection,
    client: &Client,
    shared: &Mutex<Shared>,
) -> Result {
    let mut parts = topic.split('/').skip(2);
    let (Some(name), Some(kind)) = (parts.next(), parts.next()) else {
        return Err("bad topic".into());
    };
    let id = NodeId::from_name(name).ok_or("unknown node")?;
    let e = open(payload, id).ok_or("bad envelope")?;
    match kind {
        "heartbeat" => {
            let mut s = shared.lock().unwrap();
            s.heartbeats[id as usize] = Some(now);
            s.counts[id as usize].1 += 1;
        }
        "telemetry" if e.r#type == "telemetry" => {
            let body: Value = serde_json::from_str(&e.body).map_err(|e| e.to_string())?;
            let mut s = shared.lock().unwrap();
            s.counts[id as usize].0 += 1;
            s.telemetry[id as usize] = now;
            telemetry(&mut s.fleet, id, e.boot_id, &body);
        }
        "event" => {
            // Confirm only after the record is durable.
            let new = db
                .prepare_cached("INSERT OR IGNORE INTO records VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")
                .and_then(|mut s| {
                    s.execute(params![
                        session,
                        name,
                        e.boot_id,
                        e.seq,
                        e.sim_time_ms as i64,
                        e.r#type,
                        e.body
                    ])
                })
                .map_err(|e| e.to_string())?;
            let ack = json!({"boot_id": e.boot_id, "seq": e.seq});
            publish(client, id, "ack", e.boot_id, e.seq, ack)?;
            if new == 1 {
                let mut s = shared.lock().unwrap();
                let events = &mut s.fleet.events;
                if events.len() == SHOWN_EVENTS {
                    events.remove(0);
                }
                events.push(Entry {
                    time_ms: now,
                    node: id,
                    kind: e.r#type,
                    body: e.body,
                });
            }
        }
        _ => {}
    }
    Ok(())
}

fn telemetry(fleet: &mut Fleet, id: NodeId, boot: u32, body: &Value) {
    let flag = |key: &str| body[key].as_u64().unwrap_or(0) == 1;
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let report = fleet.report_mut(id);
    report.boot_id = boot;
    report.temp_c = body["temp_c"].as_f64().filter(|_| flag("valid"));
    report.battery = body["battery"].as_f64().unwrap_or(0.0);
    report.relay = flag("relay");
    report.alarms = body["alarms"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    report.safe_s = body["safe_s"].as_u64();
    report.transit = body["transit"].as_u64().unwrap_or(0);
    report.receiver = flag("receiver");
    report.person = text(&body["person"]);
    report.backlog = body["backlog"].as_u64().unwrap_or(0);
    if let Some(list) = body["batches"].as_array() {
        fleet.batches = list
            .iter()
            .map(|b| Batch {
                name: text(&b[0]),
                place: text(&b[1]),
                phase: text(&b[2]),
                status: text(&b[3]),
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn rejects_damaged_envelopes() {
        let text = |sum: u32| {
            json!({"version":1,"node_id":"fridge","boot_id":1,"seq":1,"sim_time_ms":0,
                   "type":"alarm","body":"{}","checksum":sum})
            .to_string()
        };
        let good = text(crc32(b"{}"));
        assert!(open(good.as_bytes(), NodeId::Fridge).is_some());
        assert!(open(good.as_bytes(), NodeId::Carrier).is_none());
        assert!(open(text(1).as_bytes(), NodeId::Fridge).is_none());
    }
}

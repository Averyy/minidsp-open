//! Sanitized Device Console reports, replayed through the real protocol and queue.
use minidsp::client::MiniDspClient;
use minidsp::queue::{CommandQueue, Priority};
use minidsp::transport::{Transport, TransportError};
use minidsp_protocol::{commands, dialect::Dialect, eeprom, packet};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

#[derive(Deserialize)]
struct Fixture {
    format_version: u8,
    exchanges: Vec<Exchange>,
}

#[derive(Deserialize)]
struct Exchange {
    id: String,
    label: String,
    support: String,
    encoder: String,
    arguments: Value,
    request: Frame,
    replies: Vec<Reply>,
}

#[derive(Deserialize)]
struct Frame {
    direction: String,
    hex: String,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(flatten)]
    frame: Frame,
    expected: Expected,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Expected {
    Ack,
    HardwareId {
        fw_major: u8,
        fw_minor: u8,
        hw_id: u8,
    },
    Flash {
        data_hex: String,
    },
    Floats {
        values: Vec<f32>,
        words: Vec<u32>,
    },
    PresetComplete,
    Unknown {
        data_hex: String,
    },
    Notification {
        address: u16,
        source: u8,
    },
}

fn fixture() -> Fixture {
    let fixture: Fixture =
        serde_json::from_str(include_str!("fixtures/device-console-hid.json")).unwrap();
    assert_eq!(fixture.format_version, 1);
    fixture
}

fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

fn request(exchange: &Exchange) -> Vec<u8> {
    let report = hex(&exchange.request.hex);
    assert_eq!(report.len(), packet::HID_REPORT_SIZE);
    assert_eq!(report[0], 0);
    packet::decode_frame(&report[1..]).unwrap()
}

fn reply_payload(reply: &Reply) -> Vec<u8> {
    let report = hex(&reply.frame.hex);
    assert_eq!(report.len(), 64);
    packet::decode_hid_response(&report).unwrap()
}

fn encoded_request(exchange: &Exchange) -> Option<Vec<u8>> {
    let a = &exchange.arguments;
    let number = |name: &str| a[name].as_u64().unwrap();
    let address = || number("address") as u16;
    let data = || hex(a["data_hex"].as_str().unwrap());
    Some(match exchange.encoder.as_str() {
        "read_hardware_id" if exchange.support == "passthrough" => return None,
        "read_hardware_id" => commands::read_hardware_id(),
        "read_dsp_version" => commands::read_dsp_version(),
        "read_flash" => commands::read_flash(
            (address() >> 8) as u8,
            address() as u8,
            number("count") as u8,
        ),
        "write_flash" => commands::write_flash((address() >> 8) as u8, address() as u8, &data()),
        "read_floats" => commands::read_floats(address(), number("count") as u8, &Dialect::NEW),
        "set_volume" => commands::set_volume(number("value") as u8),
        "set_source" => commands::set_source(number("source") as u8),
        "write_dsp" => commands::write_dsp(
            address(),
            a["value"].as_f64().unwrap() as f32,
            a["save"].as_bool().unwrap(),
            &Dialect::NEW,
        ),
        "write_dsp_int" => commands::write_dsp_int(address(), number("value") as u16),
        "write_dsp_delay" => commands::write_dsp_delay(
            address(),
            number("samples") as u32,
            a["save"].as_bool().unwrap(),
            &Dialect::NEW,
        ),
        "write_biquad" => {
            let coeffs = serde_json::from_value::<[f32; 5]>(a["coefficients"].clone()).unwrap();
            commands::write_biquad(address(), &coeffs, &Dialect::NEW)
        }
        "bypass_filter" => {
            commands::bypass_filter(address(), a["bypass"].as_bool().unwrap(), &Dialect::NEW)
        }
        "set_config" => commands::set_config(number("preset") as u8, number("mode") as u8),
        "write_flash_block" => commands::write_flash_block(
            number("block") as u8,
            &data(),
            a["begin"].as_bool().unwrap(),
        ),
        "finish_flash_block" => commands::finish_flash_block(number("block") as u8),
        other => panic!("unhandled fixture encoder {other}"),
    })
}

fn assert_response(exchange: &Exchange, reply: &Reply, parsed: commands::Response) {
    use commands::Response;
    match &reply.expected {
        Expected::Ack => assert_eq!(parsed, Response::Ack, "{}", exchange.id),
        Expected::HardwareId {
            fw_major,
            fw_minor,
            hw_id,
        } => assert_eq!(
            parsed,
            Response::HardwareId(commands::HardwareId {
                fw_major: *fw_major,
                fw_minor: *fw_minor,
                hw_id: *hw_id,
            }),
            "{}",
            exchange.id
        ),
        Expected::Flash { data_hex } => {
            assert_eq!(
                parsed,
                Response::FlashData(hex(data_hex)),
                "{}",
                exchange.id
            );
        }
        Expected::Floats { values, words } => {
            let Response::Floats(actual) = parsed else {
                panic!("{}: {parsed:?}", exchange.id);
            };
            assert_eq!(
                actual.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "{}",
                exchange.id
            );
            assert_eq!(
                commands::parse_param_words_response_for_request(
                    &request(exchange),
                    &reply_payload(reply)
                ),
                Some(words.clone()),
                "{}",
                exchange.id
            );
        }
        Expected::PresetComplete => assert_eq!(parsed, Response::PresetComplete, "{}", exchange.id),
        Expected::Unknown { data_hex } => {
            assert_eq!(parsed, Response::Unknown(hex(data_hex)), "{}", exchange.id);
        }
        Expected::Notification { address, source } => {
            let payload = reply_payload(reply);
            assert_eq!(
                payload,
                [
                    commands::CMD_READ_FLASH,
                    (*address >> 8) as u8,
                    *address as u8,
                    *source
                ]
            );
            assert_eq!(*address, 0xffa9);
            assert!(commands::is_source_changed_notification_for_request(
                &request(exchange),
                &payload
            ));
            assert_eq!(parsed, Response::Ack);
        }
    }
}

#[test]
fn captured_requests_match_our_encoders_and_hid_framing() {
    let fixture = fixture();
    assert_eq!(fixture.exchanges.len(), 54);
    let mut opcodes = BTreeSet::new();
    let mut passthrough = 0;
    for exchange in &fixture.exchanges {
        assert!(!exchange.label.is_empty());
        assert_eq!(exchange.request.direction, "tx");
        let captured = request(exchange);
        opcodes.insert(captured[0]);
        if let Some(encoded) = encoded_request(exchange) {
            assert_eq!(encoded, captured, "{}", exchange.id);
            assert_eq!(
                packet::encode_hid(&encoded).as_slice(),
                hex(&exchange.request.hex),
                "{}",
                exchange.id
            );
        } else {
            passthrough += 1;
            assert_eq!(
                captured,
                [0x31, exchange.arguments["selector"].as_u64().unwrap() as u8]
            );
            assert_eq!(
                packet::encode_hid(&captured).as_slice(),
                hex(&exchange.request.hex)
            );
        }
    }
    assert_eq!(passthrough, 2);
    assert_eq!(
        opcodes,
        BTreeSet::from([
            0x04, 0x05, 0x06, 0x07, 0x13, 0x14, 0x19, 0x25, 0x30, 0x31, 0x34, 0x40, 0x42
        ])
    );
}

#[test]
fn every_captured_reply_decodes_to_labelled_fields() {
    for exchange in &fixture().exchanges {
        for reply in &exchange.replies {
            assert_eq!(reply.frame.direction, "rx");
            let parsed = commands::parse_response_for_request(
                &request(exchange),
                &reply_payload(reply),
                &Dialect::NEW,
            );
            assert_response(exchange, reply, parsed);
        }
    }
    let fixture = fixture();
    let serial = find(&fixture, "capture-8");
    let Expected::Flash { data_hex } = &serial.replies[0].expected else {
        panic!("serial data")
    };
    assert_eq!(eeprom::parse_serial(&hex(data_hex)), Some(912345));
    let dsp_id = find(&fixture, "capture-5");
    let Expected::Flash { data_hex } = &dsp_id.replies[0].expected else {
        panic!("DSP ID")
    };
    assert_eq!(eeprom::parse_dsp_version(&hex(data_hex)), Some(100));
}

struct Step {
    request: Vec<u8>,
    reports: Vec<Vec<u8>>,
}

impl Step {
    fn captured(exchange: &Exchange) -> Self {
        Self {
            request: request(exchange),
            reports: exchange
                .replies
                .iter()
                .map(|reply| hex(&reply.frame.hex))
                .collect(),
        }
    }
}

#[derive(Default)]
struct Replay {
    steps: VecDeque<Step>,
    incoming: VecDeque<Vec<u8>>,
}

struct CaptureTransport(Arc<Mutex<Replay>>);

impl Transport for CaptureTransport {
    fn send_frame(
        &self,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let mut replay = self.0.lock().unwrap();
        let step = replay
            .steps
            .pop_front()
            .expect("unexpected client/queue request");
        assert_eq!(payload, step.request);
        replay.incoming.extend(step.reports);
        Box::pin(async { Ok(()) })
    }

    fn recv_frame(
        &self,
        _timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
        let report = self
            .0
            .lock()
            .unwrap()
            .incoming
            .pop_front()
            .expect("queue skipped the expected response");
        let payload = packet::decode_hid_response(&report).expect("captured HID report");
        Box::pin(async { Ok(payload) })
    }

    fn is_connected(&self) -> bool {
        true
    }
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
    fn url(&self) -> String {
        "capture://2x4hd".into()
    }
}

fn transport(steps: impl IntoIterator<Item = Step>) -> (Box<dyn Transport>, Arc<Mutex<Replay>>) {
    let state = Arc::new(Mutex::new(Replay {
        steps: steps.into_iter().collect(),
        ..Replay::default()
    }));
    (Box::new(CaptureTransport(state.clone())), state)
}

fn assert_drained(state: &Mutex<Replay>) {
    let state = state.lock().unwrap();
    assert!(state.steps.is_empty());
    assert!(state.incoming.is_empty());
}

fn find<'a>(fixture: &'a Fixture, id: &str) -> &'a Exchange {
    fixture
        .exchanges
        .iter()
        .find(|exchange| exchange.id == id)
        .unwrap()
}

#[tokio::test]
async fn queue_replays_all_captured_families_in_session_order() {
    let fixture = fixture();
    let (transport, state) = transport(fixture.exchanges.iter().map(Step::captured));
    let queue = CommandQueue::new(transport);
    for exchange in &fixture.exchanges {
        let req = request(exchange);
        if exchange.support == "raw_response" {
            let raw = queue.execute_raw(req, Priority::Normal).await.unwrap();
            assert_response(
                exchange,
                &exchange.replies[0],
                commands::parse_response(&raw, &Dialect::NEW),
            );
        } else {
            let parsed = queue.execute(req, Priority::Normal).await.unwrap();
            assert_response(exchange, &exchange.replies[0], parsed);
        }
        // Source notifications follow the synchronous ACK and remain for the next request.
        assert_eq!(
            state.lock().unwrap().incoming.len(),
            exchange.replies.len() - 1
        );
    }
    assert_drained(&state);
}

#[tokio::test]
async fn queue_skips_source_notifications_and_wrong_address_stale_reads() {
    let fixture = fixture();
    let source = find(&fixture, "capture-242");
    let meter = find(&fixture, "capture-44");
    let serial = find(&fixture, "capture-8");
    let mut meter_step = Step::captured(meter);
    meter_step
        .reports
        .insert(0, hex(&find(&fixture, "capture-41").replies[0].frame.hex));
    let mut serial_step = Step::captured(serial);
    serial_step
        .reports
        .insert(0, hex(&find(&fixture, "capture-26").replies[0].frame.hex));
    let (transport, state) = transport([Step::captured(source), meter_step, serial_step]);
    let queue = CommandQueue::new(transport);
    for exchange in [source, meter, serial] {
        let parsed = queue
            .execute(request(exchange), Priority::Normal)
            .await
            .unwrap();
        assert_response(exchange, &exchange.replies[0], parsed);
    }
    assert_drained(&state);
}

// Device Console reads these status fields separately. The client reads four
// contiguous bytes, so ONLY this status response is reconstructed from captured
// fields; it is not presented as a captured four-byte request/reply exchange.
fn master_step(master: &[u8; 4]) -> Step {
    let mut payload = vec![commands::CMD_READ_FLASH, 0xff, 0xd8];
    payload.extend_from_slice(master);
    Step {
        request: commands::read_flash(0xff, 0xd8, 4),
        reports: vec![packet::encode_tcp_response(&payload)],
    }
}

#[tokio::test]
async fn client_probes_and_writes_using_captured_reports() {
    let fixture = fixture();
    let flash = |id| {
        let Expected::Flash { data_hex } = &find(&fixture, id).replies[0].expected else {
            panic!("flash field")
        };
        hex(data_hex)
    };
    let preset = flash("capture-23");
    let source = flash("capture-32");
    let vol_mute = flash("capture-38");
    let initial = [preset[0], source[0], vol_mute[0], vol_mute[1]];
    let volume = find(&fixture, "capture-59");
    let new_volume = volume.arguments["value"].as_u64().unwrap() as u8;
    let source = find(&fixture, "capture-242");
    let new_source = source.arguments["source"].as_u64().unwrap() as u8;
    assert_ne!(initial[2], new_volume);
    assert_ne!(initial[1], new_source);
    let after_volume = [initial[0], initial[1], new_volume, initial[3]];
    let after_source = [initial[0], new_source, new_volume, initial[3]];
    let peq = find(&fixture, "capture-635");
    let bypass = find(&fixture, "capture-677");
    let (transport, state) = transport([
        Step::captured(find(&fixture, "capture-2")),
        Step::captured(find(&fixture, "capture-5")),
        Step::captured(find(&fixture, "capture-8")),
        master_step(&initial),
        master_step(&initial),
        Step::captured(volume),
        master_step(&after_volume),
        master_step(&after_volume),
        Step::captured(source),
        master_step(&after_source),
        Step::captured(peq),
        Step::captured(bypass),
    ]);
    let client = MiniDspClient::connect(transport).await.unwrap();
    assert_eq!(client.serial(), 912345);
    assert_eq!(
        client.hw_id(),
        &commands::HardwareId {
            fw_major: 1,
            fw_minor: 85,
            hw_id: 10
        }
    );
    assert_eq!(client.dsp_version(), 100);
    client.set_volume(new_volume).await.unwrap();
    client.set_source(new_source).await.unwrap();
    let coefficients: [f32; 5] =
        serde_json::from_value(peq.arguments["coefficients"].clone()).unwrap();
    client
        .write_biquad(
            peq.arguments["address"].as_u64().unwrap() as u16,
            &coefficients,
        )
        .await
        .unwrap();
    client
        .bypass_filter(bypass.arguments["address"].as_u64().unwrap() as u16, false)
        .await
        .unwrap();
    assert_drained(&state);
}

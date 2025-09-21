use helper::clock::LamportClock;
use helper::uap::{Command, UapMessage};
use helper::util::{now_unix_nanos, random_session_id, SeqCounter};
use std::io::{self, BufRead};
use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const DATA_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSING_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State { HelloWait, Ready, ReadyTimer, Closing, Closed }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerKind { Hello, DataWait, Closing }

#[derive(Debug)]
enum Event {
    Net(UapMessage),
    Timeout(TimerKind),
    StdinLine(String),
    StdinEof,
}

type TimerCell = Arc<Mutex<Option<(TimerKind, Instant, bool)>>>;

fn spawn_stdin(tx: mpsc::Sender<Event>) {
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(s) => {
                    let s_trim = s.trim().to_string();
                    if tx.send(Event::StdinLine(s_trim)).is_err() { break; }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(Event::StdinEof);
    });
}

fn spawn_recv(sock: UdpSocket, tx: mpsc::Sender<Event>) {
    thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match sock.recv_from(&mut buf) {
                Ok((n, _addr)) => {
                    if let Ok(msg) = UapMessage::decode(&buf[..n]) {
                        if tx.send(Event::Net(msg)).is_err() { break; }
                    } else {
                        // silently ignore invalid
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn spawn_timer(timers: TimerCell, tx: mpsc::Sender<Event>) {
    // Simple timer thread that polls and sends timeout events
    thread::spawn(move || loop {
        // Sleep granularity
        thread::sleep(Duration::from_millis(10));
        let mut send_kind = None;
        {
            let mut guard = timers.lock().unwrap();
            if let Some((kind, deadline, active)) = guard.as_ref().copied() {
                if active && Instant::now() >= deadline {
                    // mark inactive and trigger
                    *guard = Some((kind, deadline, false));
                    send_kind = Some(kind);
                }
            }
        }
        if let Some(k) = send_kind {
            let _ = tx.send(Event::Timeout(k));
        }
        // In a real app, add a shutdown mechanism; process ends when main exits
    });
}

fn set_timer(timers: &TimerCell, kind: TimerKind, dur: Duration) {
    let mut guard = timers.lock().unwrap();
    *guard = Some((kind, Instant::now() + dur, true));
}

fn cancel_timer(timers: &TimerCell) {
    let mut guard = timers.lock().unwrap();
    if let Some((k, d, _)) = *guard { *guard = Some((k, d, false)); }
}

fn main() {
    // Args: ./client <hostname> <port>
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| {
        eprintln!("Usage: client <hostname> <portnum>");
        std::process::exit(2);
    });
    let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| {
        eprintln!("Usage: client <hostname> <portnum>");
        std::process::exit(2);
    });

    // Resolve server address
    let server_addr = format!("{}:{}", host, port);
    let server_sockaddr = server_addr
        .to_socket_addrs()
        .ok()
        .and_then(|mut it| it.next())
        .unwrap_or_else(|| {
            eprintln!("Failed to resolve {server_addr}");
            std::process::exit(2);
        });

    // Bind UDP socket (ephemeral)
    let sock = UdpSocket::bind("0.0.0.0:0").expect("bind client socket");
    sock.connect(server_sockaddr).expect("connect UDP");
    // Clone for recv thread
    let sock_recv = sock.try_clone().expect("clone socket");

    // State
    let state = Arc::new(Mutex::new(State::HelloWait));
    let session_id = random_session_id();
    let seq = SeqCounter::new();
    let clock = LamportClock::new();

    // Latency tracking
    let mut lat_n_sum: u128 = 0;
    let mut lat_n_count: u64 = 0;

    let (tx, rx) = mpsc::channel::<Event>();
    spawn_stdin(tx.clone());
    spawn_recv(sock_recv, tx.clone());
    let timers = Arc::new(Mutex::new(None));
    spawn_timer(timers.clone(), tx.clone());

    // Handle Ctrl+C -> behave like EOF/quit: move to Closing and send GOODBYE once
    let tx_sig = tx.clone();
    ctrlc::set_handler(move || {
        let _ = tx_sig.send(Event::StdinEof);
    }).ok();

    // Send initial HELLO and start timer
    send_hello(&sock, &clock, &seq, session_id);
    set_timer(&timers, TimerKind::Hello, HELLO_TIMEOUT);

    // Main event loop (single thread for orchestration)
    while let Ok(ev) = rx.recv() {
        // Calculate one-way latency for incoming messages
        match &ev {
            Event::Net(m) => {
                let t1 = now_unix_nanos();
                if m.header.timestamp <= t1 {
                    lat_n_sum += (t1 - m.header.timestamp) as u128;
                    lat_n_count += 1;
                    // println!("one-way latency: {} ns", t1 - m.header.timestamp);
                }
                // Lamport on receive
                let _ = clock.recv_event(m.header.logical_clock);
            }
            _ => {}
        }

        let cur = *state.lock().unwrap();
        match (cur, ev) {
            // Global: GOODBYE -> Closed
            (_, Event::Net(m)) if m.header.command == Command::Goodbye => {
                println!("server sent GOODBYE -> closing");
                *state.lock().unwrap() = State::Closed;
                break;
            }

            // HelloWait
            (State::HelloWait, Event::Net(m)) if m.header.command == Command::Hello => {
                cancel_timer(&timers);
                *state.lock().unwrap() = State::Ready;
            }
            (State::HelloWait, Event::Net(m)) if m.header.command == Command::Alive => {
                // ignore, stay in HelloWait
            }
            (State::HelloWait, Event::Timeout(TimerKind::Hello)) => {
                println!("HELLO timeout -> Closing");
                *state.lock().unwrap() = State::Closing;
                send_goodbye(&sock, &clock, &seq, session_id);
                set_timer(&timers, TimerKind::Closing, CLOSING_TIMEOUT);
            }
            (State::HelloWait, Event::StdinEof) => {
                println!("EOF during HELLO wait -> Closing");
                *state.lock().unwrap() = State::Closing;
                send_goodbye(&sock, &clock, &seq, session_id);
                set_timer(&timers, TimerKind::Closing, CLOSING_TIMEOUT);
            }
            (State::HelloWait, Event::StdinLine(s)) => {
                if s.eq_ignore_ascii_case("q") || s.eq_ignore_ascii_case("eof") { // user requested quit
                    println!("q received -> Closing");
                    *state.lock().unwrap() = State::Closing;
                    send_goodbye(&sock, &clock, &seq, session_id);
                    set_timer(&timers, TimerKind::Closing, CLOSING_TIMEOUT);
                }
            }

            // Ready
            (State::Ready, Event::StdinLine(s)) => {
                if s.eq_ignore_ascii_case("q") || s.eq_ignore_ascii_case("eof") {
                    *state.lock().unwrap() = State::Closing;
                    send_goodbye(&sock, &clock, &seq, session_id);
                    set_timer(&timers, TimerKind::Closing, CLOSING_TIMEOUT);
                } else {
                    send_data(&sock, &clock, &seq, session_id, s.into_bytes());
                    *state.lock().unwrap() = State::ReadyTimer;
                    set_timer(&timers, TimerKind::DataWait, DATA_TIMEOUT);
                }
            }
            (State::Ready, Event::Net(m)) if m.header.command == Command::Alive => {
                // stay in Ready
            }
            (State::Ready, Event::StdinEof) => {
                *state.lock().unwrap() = State::Closing;
                send_goodbye(&sock, &clock, &seq, session_id);
                set_timer(&timers, TimerKind::Closing, CLOSING_TIMEOUT);
            }

            // ReadyTimer
            (State::ReadyTimer, Event::Net(m)) if m.header.command == Command::Alive => {
                cancel_timer(&timers);
                *state.lock().unwrap() = State::Ready;
            }
            (State::ReadyTimer, Event::StdinLine(s)) => {
                if s.eq_ignore_ascii_case("q") || s.eq_ignore_ascii_case("eof") {
                    *state.lock().unwrap() = State::Closing;
                    cancel_timer(&timers);
                    send_goodbye(&sock, &clock, &seq, session_id);
                    set_timer(&timers, TimerKind::Closing, CLOSING_TIMEOUT);
                } else {
                    send_data(&sock, &clock, &seq, session_id, s.into_bytes());
                    set_timer(&timers, TimerKind::DataWait, DATA_TIMEOUT);
                }
            }
            (State::ReadyTimer, Event::Timeout(TimerKind::DataWait)) => {
                println!("DATA wait timeout -> Closing");
                *state.lock().unwrap() = State::Closing;
                send_goodbye(&sock, &clock, &seq, session_id);
                set_timer(&timers, TimerKind::Closing, CLOSING_TIMEOUT);
            }
            (State::ReadyTimer, Event::StdinEof) => {
                *state.lock().unwrap() = State::Closing;
                cancel_timer(&timers);
                send_goodbye(&sock, &clock, &seq, session_id);
                set_timer(&timers, TimerKind::Closing, CLOSING_TIMEOUT);
            }

            // Closing
            (State::Closing, Event::Net(m)) if m.header.command == Command::Alive => {
                // ignore, stay Closing
            }
            (State::Closing, Event::Net(m)) if m.header.command == Command::Goodbye => {
                cancel_timer(&timers);
                *state.lock().unwrap() = State::Closed;
                break;
            }
            (State::Closing, Event::Timeout(TimerKind::Closing)) => {
                *state.lock().unwrap() = State::Closed;
                break;
            }

            // Closed: break
            (State::Closed, _) => { break; }

            // Other combinations: ignore
            _ => {}
        }
    }

    if lat_n_count > 0 {
        let avg = (lat_n_sum / lat_n_count as u128) as u64;
        println!("average one-way latency: {} ns over {} msgs", avg, lat_n_count);
    }
}

fn send_hello(sock: &UdpSocket, clock: &LamportClock, seq: &SeqCounter, sid: u32) {
    let lc = clock.send_event();
    let ts = now_unix_nanos();
    let msg = UapMessage::hello(seq.next(), sid, lc, ts);
    let _ = sock.send(&msg.encode());
}

fn send_data(sock: &UdpSocket, clock: &LamportClock, seq: &SeqCounter, sid: u32, data: Vec<u8>) {
    let lc = clock.send_event();
    let ts = now_unix_nanos();
    let msg = UapMessage::data(seq.next(), sid, lc, ts, data);
    let _ = sock.send(&msg.encode());
}

fn send_goodbye(sock: &UdpSocket, clock: &LamportClock, seq: &SeqCounter, sid: u32) {
    let lc = clock.send_event();
    let ts = now_unix_nanos();
    let msg = UapMessage::goodbye(seq.next(), sid, lc, ts);
    let _ = sock.send(&msg.encode());
}


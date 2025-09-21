use helper::clock::LamportClock;
use helper::uap::{Command, UapMessage};
use helper::util::{now_unix_nanos, SeqCounter};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{self, Instant};

// Session timeout duration (inactivity leads to GOODBYE)
const SESSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
enum Control {
    Quit,                 // stdin EOF or 'q' -> graceful shutdown
    Timeout(u32),         // session timeout by id
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase { Init, Receive, Done }

#[derive(Debug)]
struct SessionState {
    addr: SocketAddr,
    phase: Phase,
    last_seq: Option<u32>, // last client sequence received
}

#[tokio::main]
async fn main() -> io::Result<()> {
    // Parse CLI: ./server <port>
    let mut args = std::env::args().skip(1);
    let port: u16 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            eprintln!("Usage: server <portnum>");
            std::process::exit(2);
        });

    let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
    let socket = UdpSocket::bind(bind_addr).await?;
    println!("Server listening on {}", socket.local_addr()?);

    run_server(socket).await
}

async fn run_server(socket: UdpSocket) -> io::Result<()> {
    // Shared state: sessions by session_id
    let mut sessions: HashMap<u32, SessionState> = HashMap::new();
    let mut timers: HashMap<u32, mpsc::Sender<()>> = HashMap::new(); // cancel handles

    let clock = LamportClock::new();
    let seq = SeqCounter::new(); // server's outgoing sequence

    // Control channel for stdin and timer events
    let (ctrl_tx, mut ctrl_rx) = mpsc::channel::<Control>(32);

    // Spawn stdin task
    let stdin_tx = ctrl_tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let l = line.trim().to_string();
            if l.eq_ignore_ascii_case("q") {
                let _ = stdin_tx.send(Control::Quit).await;
                return;
            }
        }
        // EOF
        let _ = stdin_tx.send(Control::Quit).await;
    });

    let mut buf = vec![0u8; 64 * 1024];

    loop {
        tokio::select! {
            // Network receive
            res = socket.recv_from(&mut buf) => {
                let (n, addr) = match res { Ok(v) => v, Err(e) => { eprintln!("recv error: {e}"); continue; } };
                let pkt = &buf[..n];
                match UapMessage::decode(pkt) {
                    Ok(msg) => {
                        // Lamport on receive
                        let _ = clock.recv_event(msg.header.logical_clock);
                        handle_incoming(&socket, &clock, &seq, &mut sessions, &mut timers, addr, msg, &ctrl_tx).await?;
                    }
                    Err(_) => {
                        // silently discard per spec when magic/version invalid; already validated in decode
                        // For other decode errors (like command), we ignore too.
                        continue;
                    }
                }
            }
            // Control events (stdin quit or session timeouts)
            Some(ctrl) = ctrl_rx.recv() => {
                match ctrl {
                    Control::Quit => {
                        // Send GOODBYE to all active sessions, then exit
                        for (&sid, st) in sessions.iter() {
                            if st.phase != Phase::Done {
                                send_goodbye(&socket, &clock, &seq, sid, st.addr).await?;
                            }
                        }
                        break;
                    }
                    Control::Timeout(sid) => {
                        if let Some(st) = sessions.get_mut(&sid) {
                            if st.phase != Phase::Done {
                                // Timeout: send GOODBYE and close
                                send_goodbye(&socket, &clock, &seq, sid, st.addr).await?;
                                st.phase = Phase::Done;
                                println!("0x{sid:08x} Session closed");
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_incoming(
    socket: &UdpSocket,
    clock: &LamportClock,
    seq: &SeqCounter,
    sessions: &mut HashMap<u32, SessionState>,
    timers: &mut HashMap<u32, mpsc::Sender<()>>,
    addr: SocketAddr,
    msg: UapMessage,
    ctrl_tx: &mpsc::Sender<Control>,
) -> io::Result<()> {
    let sid = msg.header.session_id;
    let command = msg.header.command;
    let now = Instant::now();

    // Ensure session exists
    let entry = sessions.entry(sid).or_insert_with(|| SessionState { addr, phase: Phase::Init, last_seq: None });
    // Update remote endpoint if changed
    entry.addr = addr;

    // Sequence handling using last_seq
    let seq_in = msg.header.sequence_number;
    if let Some(last) = entry.last_seq {
        if seq_in < last {
            eprintln!("session {sid}: sequence went backwards (got {seq_in}, last {last}) -> closing");
            send_goodbye(socket, clock, seq, sid, entry.addr).await?;
            entry.phase = Phase::Done;
            println!("0x{sid:08x} Session closed");
            return Ok(());
        }
        if seq_in == last {
            eprintln!("session {sid}: duplicate packet #{seq_in} -> discard");
            return Ok(());
        }
        if seq_in > last + 1 {
            for missing in (last + 1)..seq_in {
                println!("0x{sid:08x} [{missing}] Lost packet!");
            }
        }
    }
    entry.last_seq = Some(seq_in);

    match command {
        Command::Hello => {
            // Only valid in Init; otherwise protocol error
            if entry.phase != Phase::Init {
                eprintln!("session {sid}: unexpected HELLO -> closing");
                send_goodbye(socket, clock, seq, sid, entry.addr).await?;
                entry.phase = Phase::Done;
                println!("0x{sid:08x} Session closed");
                return Ok(());
            }
            send_hello(socket, clock, seq, sid, entry.addr).await?;
            entry.phase = Phase::Receive;
            println!("0x{sid:08x} [{seq_in}] Session created");
            reset_session_timer(sid, ctrl_tx.clone(), timers, now + SESSION_TIMEOUT);
        }
        Command::Data => {
            // Print payload, send ALIVE, reset timer
            if let Some(payload) = msg.payload {
                if let Ok(s) = String::from_utf8(payload.clone()) {
                    println!("0x{sid:08x} [{seq_in}] {}", s);
                } else {
                    println!("0x{sid:08x} [{seq_in}] <{} bytes>", payload.len());
                }
            }
            send_alive(socket, clock, seq, sid, entry.addr).await?;
            reset_session_timer(sid, ctrl_tx.clone(), timers, now + SESSION_TIMEOUT);
        }
        Command::Alive => {
            // Keep-alive, just reset timer
            reset_session_timer(sid, ctrl_tx.clone(), timers, now + SESSION_TIMEOUT);
        }
        Command::Goodbye => {
            // Reply GOODBYE and mark done
            println!("0x{sid:08x} [{seq_in}] GOODBYE from client.");
            send_goodbye(socket, clock, seq, sid, entry.addr).await?;
            entry.phase = Phase::Done;
            // Cancel timer if any
            if let Some(tx) = timers.remove(&sid) { let _ = tx.try_send(()); }
            println!("0x{sid:08x} Session closed");
        }
    }

    Ok(())
}

fn reset_session_timer(
    sid: u32,
    ctrl_tx: mpsc::Sender<Control>,
    timers: &mut HashMap<u32, mpsc::Sender<()>>,
    deadline: Instant,
) {
    // Cancel previous timer if exists
    if let Some(prev) = timers.remove(&sid) {
        let _ = prev.try_send(()); // best-effort cancel
    }
    // New cancellation channel for this timer
    let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
    timers.insert(sid, cancel_tx);

    tokio::spawn(async move {
        let until = deadline;
        let sleep = time::sleep_until(until);
        tokio::pin!(sleep);
        tokio::select! {
            _ = &mut sleep => { let _ = ctrl_tx.send(Control::Timeout(sid)).await; }
            _ = cancel_rx.recv() => { /* cancelled */ }
        }
    });
}

async fn send_hello(socket: &UdpSocket, clock: &LamportClock, seq: &SeqCounter, sid: u32, addr: SocketAddr) -> io::Result<()> {
    let lc = clock.send_event();
    let ts = now_unix_nanos();
    let msg = UapMessage::hello(seq.next(), sid, lc, ts);
    socket.send_to(&msg.encode(), addr).await?;
    Ok(())
}

async fn send_alive(socket: &UdpSocket, clock: &LamportClock, seq: &SeqCounter, sid: u32, addr: SocketAddr) -> io::Result<()> {
    let lc = clock.send_event();
    let ts = now_unix_nanos();
    let msg = UapMessage::alive(seq.next(), sid, lc, ts);
    socket.send_to(&msg.encode(), addr).await?;
    Ok(())
}

async fn send_goodbye(socket: &UdpSocket, clock: &LamportClock, seq: &SeqCounter, sid: u32, addr: SocketAddr) -> io::Result<()> {
    let lc = clock.send_event();
    let ts = now_unix_nanos();
    let msg = UapMessage::goodbye(seq.next(), sid, lc, ts);
    socket.send_to(&msg.encode(), addr).await?;
    Ok(())
}


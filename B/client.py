import socket
import struct
import sys
import time
from enum import Enum, auto
from typing import Optional, Tuple


# Protocol constants (must match server)
MAGIC_NUMBER = 0xC461
VERSION = 1

COMMANDS = {
    'HELLO': 0,
    'DATA': 1,
    'ALIVE': 2,
    'GOODBYE': 3,
}

# Header: magic(2), version(1), command(1), seq_num(4), session_id(4), clock(8), timestamp_ms(8)
HEADER_FORMAT = '!HBBIIQQ'
HEADER_SIZE = struct.calcsize(HEADER_FORMAT)


class ClientState(Enum):
    INITIAL = auto()
    HELLO = auto()
    READY = auto()
    READY_TIMER = auto()
    CLOSING = auto()
    CLOSED = auto()


class UDPClientFSA:
    MAX_RETRIES = 3
    RETRY_TIMEOUT = 2.0  # seconds

    def __init__(self, server_ip: str, server_port: int, session_id: int = 1000):
        self.server = (server_ip, server_port)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.settimeout(self.RETRY_TIMEOUT)

        self.session_id = session_id
        self.state = ClientState.INITIAL
        self.sequence = 0  # Client DATA sequence (HELLO/GOODBYE use 0)
        self.logical_clock = 0

    # Lamport logical clock
    def tick(self, received_clock: Optional[int] = None) -> int:
        if received_clock is not None:
            self.logical_clock = max(self.logical_clock, received_clock)
        self.logical_clock += 1
        return self.logical_clock

    def make_packet(self, command: int, seq: int, payload: bytes = b'') -> bytes:
        clock_value = self.tick()
        timestamp_ms = int(time.time() * 1000)
        header = struct.pack(
            HEADER_FORMAT,
            MAGIC_NUMBER,
            VERSION,
            command,
            seq,
            self.session_id,
            clock_value,
            timestamp_ms,
        )
        return header + payload

    def send(self, command_name: str, seq: int, payload: bytes = b'') -> None:
        pkt = self.make_packet(COMMANDS[command_name], seq, payload)
        self.sock.sendto(pkt, self.server)

    def recv(self) -> Tuple[Optional[tuple], Optional[bytes]]:
        try:
            packet, _ = self.sock.recvfrom(4096)
        except socket.timeout:
            return None, None
        except Exception:
            return None, None

        if len(packet) < HEADER_SIZE:
            return None, None

        try:
            header = struct.unpack(HEADER_FORMAT, packet[:HEADER_SIZE])
        except struct.error:
            return None, None

        magic, version, command, seq, sess_id, clock, ts = header
        if magic != MAGIC_NUMBER or version != VERSION or sess_id != self.session_id:
            return None, None

        # Merge Lamport clock on any valid packet
        self.tick(clock)
        return header, packet[HEADER_SIZE:]

    # FSM actions
    def perform_handshake(self) -> bool:
        self.state = ClientState.HELLO
        for attempt in range(self.MAX_RETRIES):
            self.send('HELLO', 0)
            header, _ = self.recv()
            if header is None:
                continue
            _, _, command, _, _, _, _ = header
            if command == COMMANDS['HELLO']:
                # Move to READY and prime data sequence to 1
                self.state = ClientState.READY
                self.sequence = 1
                return True
            if command == COMMANDS['GOODBYE']:
                self.state = ClientState.CLOSED
                return False
        self.state = ClientState.CLOSED
        return False

    def send_data(self, data: bytes) -> bool:
        if self.state != ClientState.READY:
            return False

        self.state = ClientState.READY_TIMER
        acked = False
        for _ in range(self.MAX_RETRIES):
            self.send('DATA', self.sequence, data)
            header, _ = self.recv()
            if header is None:
                continue
            _, _, command, _, _, _, _ = header
            if command == COMMANDS['ALIVE']:
                acked = True
                break
            if command == COMMANDS['GOODBYE']:
                self.state = ClientState.CLOSED
                return False

        if not acked:
            self.state = ClientState.CLOSING
            return False

        # Ack ok -> back to READY for next DATA
        self.sequence += 1
        self.state = ClientState.READY
        return True

    def goodbye(self) -> None:
        if self.state == ClientState.CLOSED:
            return
        self.state = ClientState.CLOSING
        for _ in range(self.MAX_RETRIES):
            self.send('GOODBYE', 0)
            header, _ = self.recv()
            if header is None:
                continue
            _, _, command, _, _, _, _ = header
            if command == COMMANDS['GOODBYE']:
                self.state = ClientState.CLOSED
                return
        self.state = ClientState.CLOSED

    # High-level API
    def run_interactive(self) -> None:
        if self.state == ClientState.INITIAL:
            ok = self.perform_handshake()
            if not ok:
                print('Handshake failed; closing.')
                self.goodbye()
                return
            print('Handshake OK. Session ready.')

        print("Type text to send, 'sendfile <path>', or 'quit'.")
        while self.state not in (ClientState.CLOSING, ClientState.CLOSED):
            try:
                line = input('> ')
                # Normalize line endings; keep empty lines if any
                line = line.rstrip('\r\n')
            except (EOFError, KeyboardInterrupt):
                break
            # allow empty lines too
            low = line.lower()
            if low in ('q', 'quit', 'exit'):
                break
            if low.startswith('sendfile '):
                path = line.split(' ', 1)[1].strip()
                self.send_file(path)
                continue

            # Encode with replacement to avoid surrogate failures from console
            data = line.encode('utf-8', errors='replace')
            sent = self.send_data(data)
            if not sent:
                print('Send failed; closing session.')
                break

        self.goodbye()

    def run_from_stdin(self) -> None:
        # Called when stdin is not a TTY (e.g., piped input)
        if self.state == ClientState.INITIAL:
            ok = self.perform_handshake()
            if not ok:
                return
        for raw in sys.stdin.buffer:
            # raw is bytes; strip CRLF without decoding failures
            raw = raw.rstrip(b'\r\n')
            if not raw:
                # Send empty lines as empty payload? Skip to reduce noise
                continue
            # Ensure utf-8 validity by replacing invalid sequences
            try:
                # Attempt to decode and re-encode to normalize; replace errors
                payload = raw.decode('utf-8', errors='replace').encode('utf-8', errors='replace')
            except Exception:
                payload = raw  # fallback: send as-is
            if not self.send_data(payload):
                break
        self.goodbye()

    def send_file(self, path: str) -> None:
        try:
            with open(path, 'r', encoding='utf-8') as f:
                for raw in f:
                    # Preserve per-line semantics; skip empty lines cleanly
                    payload = raw.rstrip('\r\n')
                    if payload:
                        if not self.send_data(payload.encode('utf-8')):
                            print('File transfer aborted.')
                            return
            print('File transfer complete.')
        except FileNotFoundError:
            print(f'File not found: {path}')
        except Exception as e:
            print(f'Error reading file: {e}')


def main() -> None:
    if len(sys.argv) != 3:
        print('Usage: python client_fsa.py <server_ip> <server_port>')
        sys.exit(1)

    server_ip = sys.argv[1]
    try:
        server_port = int(sys.argv[2])
    except ValueError:
        print('Server port must be an integer')
        sys.exit(1)

    client = UDPClientFSA(server_ip, server_port)
    try:
        # If stdin is piped, stream from stdin; otherwise interactive mode
        if sys.stdin.isatty():
            client.run_interactive()
        else:
            client.run_from_stdin()
    except KeyboardInterrupt:
        pass
    finally:
        client.goodbye()


if __name__ == '__main__':
    main()

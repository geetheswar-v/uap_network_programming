#!/usr/bin/env python3

import sys
import socket
import struct
import threading
import time
import queue
import random
import select

# Protocol constants from the lab assignment
MAGIC_NUMBER = 0xC461  # 50273 in decimal
VERSION = 1

COMMANDS = {
    'HELLO': 0,
    'DATA': 1,
    'ALIVE': 2,
    'GOODBYE': 3,
}

# Header format: magic(2), version(1), command(1), seq_num(4), session_id(4), logical_clock(8), timestamp(8)
HEADER_FORMAT = "!HBBIIQQ"
HEADER_LENGTH = struct.calcsize(HEADER_FORMAT)

# Global variables for session management
sessions = {}
sessions_lock = threading.Lock()
server_logical_clock = 0
clock_lock = threading.Lock()
server_running = True
server_socket = None

class Session(threading.Thread):
    def __init__(self, session_id, client_address, server_socket, initial_packet):
        super().__init__()
        self.session_id = session_id
        self.client_address = client_address
        self.server_socket = server_socket
        self.state = 'Initial'  # Start in Initial state, will check HELLO first
        self.timer = None
        self.expected_seq_num = 0
        self.server_seq_num = 0  # Server maintains its own sequence number
        self.logical_clock = 0
        self.inbox = queue.Queue()
        self.inbox.put(initial_packet)
        self.running = True
        self.daemon = True  # Make thread daemon so it closes when main exits
        # Stats
        self.lost_count = 0
        self.dup_count = 0
        self.recv_count = 0
        self.last_seq_seen = 0

    def run(self):
        """Main session thread loop"""
        while self.running and server_running:
            try:
                # Use timeout to periodically check timer and running status
                data = self.inbox.get(timeout=1.0)
                self.process_packet(data)
            except queue.Empty:
                self.check_timer()
            except Exception as e:
                print(f"Session 0x{self.session_id:08x} error: {e}")
                self.send_goodbye_and_terminate()
                break

    def check_timer(self):
        """Check if inactivity timer has expired"""
        if self.timer and time.time() > self.timer:
            print(f"0x{self.session_id:08x} Session timeout - sending GOODBYE")
            self.send_goodbye_and_terminate()

    def process_packet(self, data):
        """Process incoming packet"""
        try:
            # Unpack header
            if len(data) < HEADER_LENGTH:
                return  # Silently discard malformed packets
            
            header_data = struct.unpack(HEADER_FORMAT, data[:HEADER_LENGTH])
            magic, version, command, seq_num, session_id, client_clock, timestamp = header_data
            
            # Verify magic and version
            if magic != MAGIC_NUMBER or version != VERSION:
                return  # Silently discard
            
            # Update logical clock
            self.update_logical_clock(client_clock)
            
            # Calculate one-way latency
            current_time = int(time.time() * 1000)  # milliseconds
            if timestamp > 0:
                latency = current_time - timestamp
                #print(f"One-way latency: {latency}ms")
            
            payload = data[HEADER_LENGTH:] if len(data) > HEADER_LENGTH else b''
            
            # Handle based on current state
            if self.state == 'Initial':
                self.handle_initial_state(command, seq_num, payload)
            elif self.state == 'Receive':
                self.handle_receive_state(command, seq_num, payload)
            else:
                # No valid transition - protocol error
                self.send_goodbye_and_terminate()
                
        except struct.error:
            # Malformed packet - silently discard
            pass
        except Exception as e:
            print(f"Error processing packet: {e}")
            self.send_goodbye_and_terminate()

    def handle_initial_state(self, command, seq_num, payload):
        """Handle packets in initial state - expecting HELLO"""
        if command == COMMANDS['HELLO']:
            if seq_num != 0:
                # First packet should have sequence number 0
                self.send_goodbye_and_terminate()
                return
                
            print(f"0x{self.session_id:08x} [0] Session created")
            self.expected_seq_num = 1  # Next expected sequence number
            self.send_message('HELLO')
            self.set_timer()
            self.state = 'Receive'
        else:
            # First message must be HELLO
            self.send_goodbye_and_terminate()

    def handle_receive_state(self, command, seq_num, payload):
        """Handle packets in receive state"""
        if command == COMMANDS['DATA']:
            # Handle sequence number checking
            if seq_num > self.expected_seq_num:
                # Lost packets
                for missing_seq in range(self.expected_seq_num, seq_num):
                    print(f"0x{self.session_id:08x} [{missing_seq}] Lost packet!")
                self.lost_count += (seq_num - self.expected_seq_num)
                self.expected_seq_num = seq_num + 1
                self.last_seq_seen = seq_num
            elif seq_num == self.expected_seq_num:
                # Expected sequence number
                self.expected_seq_num = seq_num + 1
                self.last_seq_seen = seq_num
            elif seq_num == self.expected_seq_num - 1:
                # Duplicate packet
                print(f"0x{self.session_id:08x} [{seq_num}] Duplicate packet!")
                self.dup_count += 1
                # Re-ACK duplicate to help client recover when ALIVE was lost
                self.send_message('ALIVE')
                self.set_timer()
                return
            else:
                # Out of order (from the past) - protocol error
                print(f"0x{self.session_id:08x} [{seq_num}] Out-of-order packet - terminating session")
                print('a')
                self.send_goodbye_and_terminate()
                return
            
            # Print data payload
            try:
                data_str = payload.decode('utf-8').rstrip('\n\r')
                print(f"0x{self.session_id:08x} [{seq_num}] {data_str}")
                self.recv_count += 1
            except UnicodeDecodeError:
                print(f"0x{self.session_id:08x} [{seq_num}] <binary data>")
                self.recv_count += 1
            
            # Send ALIVE response
            self.send_message('ALIVE')
            self.set_timer()
            
        elif command == COMMANDS['ALIVE']:
            # Keep session alive
            self.set_timer()
            
        elif command == COMMANDS['GOODBYE']:
            print(f"0x{self.session_id:08x} [{seq_num}] GOODBYE from client.")
            self.send_message('GOODBYE')
            self.terminate_session()
            
        else:
            # Invalid command for this state
            self.send_goodbye_and_terminate()

    def update_logical_clock(self, received_clock):
        """Update logical clock according to Lamport's algorithm"""
        with clock_lock:
            global server_logical_clock
            server_logical_clock = max(server_logical_clock, received_clock) + 1
            self.logical_clock = server_logical_clock

    def set_timer(self):
        """Set inactivity timer (30 seconds)"""
        self.timer = time.time() + 30.0

    def send_message(self, command_name, payload=b''):
        """Send UAP message to client"""
        try:
            with clock_lock:
                global server_logical_clock
                server_logical_clock += 1
                current_clock = server_logical_clock
            
            timestamp = int(time.time() * 1000)  # milliseconds
            
            header = struct.pack(HEADER_FORMAT,
                               MAGIC_NUMBER,
                               VERSION, 
                               COMMANDS[command_name],
                               self.server_seq_num,
                               self.session_id,
                               current_clock,
                               timestamp)
            
            message = header + payload
            self.server_socket.sendto(message, self.client_address)
            self.server_seq_num += 1
            
        except Exception as e:
            print(f"Error sending message: {e}")

    def send_goodbye_and_terminate(self):
        """Send GOODBYE and terminate session"""
        self.send_message('GOODBYE')
        self.terminate_session()

    def terminate_session(self):
        """Clean up and terminate session"""
        self.state = 'Done'
        self.running = False
        print(f"0x{self.session_id:08x} Session closed")
        # Print simple stats
        try:
            print(f"0x{self.session_id:08x} Lost packets: {self.lost_count}")
            print(f"0x{self.session_id:08x} Duplicate packets: {self.dup_count}")
            print(f"0x{self.session_id:08x} Received DATA packets: {self.recv_count}")
            print(f"0x{self.session_id:08x} Last sequence received: {self.last_seq_seen}")
        except Exception:
            pass
        
        # Remove from global sessions
        with sessions_lock:
            if self.session_id in sessions:
                del sessions[self.session_id]


def monitor_stdin():
    """Monitor stdin for quit command in separate thread"""
    global server_running
    
    try:
        while server_running:
            # Use select to check if stdin has data available
            if sys.stdin.isatty():
                # Interactive mode - read line by line
                try:
                    line = input().strip()
                    if line.lower() == 'q':
                        break
                except EOFError:
                    break
            else:
                # Non-interactive mode - check for data
                ready, _, _ = select.select([sys.stdin], [], [], 1.0)
                if ready:
                    try:
                        line = sys.stdin.readline()
                        if not line:  # EOF
                            break
                        if line.strip().lower() == 'q':
                            break
                    except:
                        break
                        
    except Exception as e:
        print(f"Stdin monitor error: {e}")
    
    # Shutdown server
    print("Server shutting down...")
    server_running = False
    
    # Send GOODBYE to all active sessions
    with sessions_lock:
        active_sessions = list(sessions.values())
    
    for session in active_sessions:
        try:
            session.send_goodbye_and_terminate()
        except:
            pass
    
    # Wait for sessions to close
    for session in active_sessions:
        try:
            session.join(timeout=2.0)
        except:
            pass
    
    # Close server socket
    if server_socket:
        server_socket.close()


def main():
    global server_socket, server_running
    
    if len(sys.argv) != 2:
        print("Usage: ./server <portnum>")
        sys.exit(1)

    try:
        portnum = int(sys.argv[1])
    except ValueError:
        print("Port number must be an integer")
        sys.exit(1)

    # Create and bind server socket
    try:
        server_socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        server_socket.bind(('', portnum))
        print(f"Waiting on port {portnum}...")
    except Exception as e:
        print(f"Error creating server socket: {e}")
        sys.exit(1)

    # Start stdin monitoring thread
    stdin_thread = threading.Thread(target=monitor_stdin)
    stdin_thread.daemon = True
    stdin_thread.start()

    try:
        while server_running:
            try:
                # Set timeout to check server_running periodically
                server_socket.settimeout(1.0)
                data, client_address = server_socket.recvfrom(4096)
                
                # Basic packet validation
                if len(data) < HEADER_LENGTH:
                    continue
                
                # Parse header to get session ID
                try:
                    header_data = struct.unpack(HEADER_FORMAT, data[:HEADER_LENGTH])
                    magic, version, command, seq_num, session_id, logical_clock, timestamp = header_data
                    
                    # Check magic and version
                    if magic != MAGIC_NUMBER or version != VERSION:
                        continue  # Silently discard
                        
                except struct.error:
                    continue  # Malformed packet
                
                # Route to appropriate session
                with sessions_lock:
                    if session_id in sessions:
                        # Existing session
                        sessions[session_id].inbox.put(data)
                    else:
                        # New session - must start with HELLO
                        if command == COMMANDS['HELLO']:
                            new_session = Session(session_id, client_address, server_socket, data)
                            sessions[session_id] = new_session
                            new_session.start()
                        # Else silently discard non-HELLO packets for new sessions
                        
            except socket.timeout:
                continue  # Check server_running and continue
            except Exception as e:
                if server_running:
                    print(f"Server error: {e}")
                break
                
    except KeyboardInterrupt:
        print("\nServer interrupted")
    finally:
        server_running = False
        if server_socket:
            server_socket.close()


if __name__ == "__main__":
    main()
UAP Network Programming - Assignment 3
UAP Network Programming - Assignment 3

Project Overview
----------------
This repository contains an assignment for UAP Network Programming. It includes two main parts:

- `A/` — a Rust workspace with `client` and `server` crates and a `helper` library used by both.
- `B/` — a simple Python client/server pair (`client.py`, `server.py`) used for demonstration or testing.

Repository Structure
--------------------

- `A/`
	- `client/`: Rust client binary (`A/client/src/main.rs`)
	- `server/`: Rust server binary (`A/server/src/main.rs`)
	- `helper/`: Rust library with shared code (`A/helper/src/*.rs`)
	- `Cargo.toml`, `Cargo.lock`
	- `server` and `client` executables
- `B/`
	- `client.py`: Python client example
	- `server.py`: Python server example

Getting Started
---------------

Rust (A/)

Prerequisites: Rust toolchain (rustc + cargo) installed.

Build and run the server (from source):

```bash
cd A/server
cargo run --release
```

If you already have compiled executables placed in `A/server` or `A/` root, run them directly:

```bash
cd A
./server
```

Build and run the client (in a new terminal):

From source:

```bash
cd A/client
cargo run --release
```

If using prebuilt executables in `A/`:

```bash
cd A
./client
```

Python (B/)

Prerequisites: Python 3.x installed.

Run the server:

The Python server can be run either as a script or as an executable. The executable usage is:

Start the server on a specific port:

```bash
cd B
./server <port>
```

Or run with the interpreter:

```bash
cd B
python3 server.py <port>
```

Run the client (in a new terminal):

Executable usage:

```bash
cd B
./client <host> <port>
```

Or run with the interpreter:

```bash
cd B
python3 client.py <host> <port>
```

Team Members
------------

- V Geetheswar - 142201025
- M Rahul - 142201022

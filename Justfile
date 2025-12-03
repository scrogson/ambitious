# Starlang - Erlang-style Concurrency for Rust
# Justfile for common development tasks

# Default recipe - show available commands
default:
    @just --list

# Build the project
build:
    cargo build

# Build in release mode
build-release:
    cargo build --release

# Run all tests
test:
    cargo test

# Run tests with output
test-verbose:
    cargo test -- --nocapture

# Run only the distribution tests
test-dist:
    cargo test -p starlang-chat --test distribution_test

# Build the chat example
build-chat:
    cargo build --manifest-path examples/chat/Cargo.toml

# ─────────────────────────────────────────────────────────────
# Chat Server Commands
# ─────────────────────────────────────────────────────────────

# Start chat server node1 (primary node)
node1:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-server -- \
        --name node1 --port 9999 --dist-port 9000

# Start chat server node2 (connects to node1)
node2:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-server -- \
        --name node2 --port 9998 --dist-port 9001 --connect 127.0.0.1:9000

# Start chat server node3 (connects to node1)
node3:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-server -- \
        --name node3 --port 9997 --dist-port 9002 --connect 127.0.0.1:9000

# Start a custom node: just node <name> <client-port> <dist-port> [connect-addr]
node name client_port dist_port connect="":
    #!/usr/bin/env bash
    if [ -n "{{connect}}" ]; then
        cargo run --manifest-path examples/chat/Cargo.toml --bin chat-server -- \
            --name {{name}} --port {{client_port}} --dist-port {{dist_port}} --connect {{connect}}
    else
        cargo run --manifest-path examples/chat/Cargo.toml --bin chat-server -- \
            --name {{name}} --port {{client_port}} --dist-port {{dist_port}}
    fi

# ─────────────────────────────────────────────────────────────
# Chat Client Commands
# ─────────────────────────────────────────────────────────────

# Path to the chat client config file
chat_config := "examples/chat/config.toml"

# Connect client to node1 (port 9999) - no auto-login
client:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-client -- --port 9999 --config {{chat_config}}

# Connect as alice to node1, auto-join #general
client-alice:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-client -- --port 9999 --config {{chat_config}} --nick alice --room general

# Connect as bob to node1, auto-join #general
client-bob:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-client -- --port 9999 --config {{chat_config}} --nick bob --room general

# Connect as charlie to node2, auto-join #general
client-charlie:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-client -- --port 9998 --config {{chat_config}} --nick charlie --room general

# Connect as dave to node2, auto-join #general
client-dave:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-client -- --port 9998 --config {{chat_config}} --nick dave --room general

# Connect as eve to node3, auto-join #general
client-eve:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-client -- --port 9997 --config {{chat_config}} --nick eve --room general

# Connect client to a specific port: just connect <port>
connect port:
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-client -- --port {{port}} --config {{chat_config}}

# Connect with custom nick and room: just chat <nick> <room> [port]
chat nick room port="9999":
    cargo run --manifest-path examples/chat/Cargo.toml --bin chat-client -- --port {{port}} --config {{chat_config}} --nick {{nick}} --room {{room}}

# ─────────────────────────────────────────────────────────────
# Development Helpers
# ─────────────────────────────────────────────────────────────

# Watch for changes and rebuild
watch:
    cargo watch -x build

# Watch and run tests
watch-test:
    cargo watch -x test

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt -- --check

# Run clippy lints
clippy:
    cargo clippy --all-targets --all-features -- -D warnings

# Run all CI checks (format, lint, test)
ci: fmt-check clippy test

# Generate documentation
docs:
    cargo doc --open

# Clean build artifacts
clean:
    cargo clean

# ─────────────────────────────────────────────────────────────
# Quick Start Guide
# ─────────────────────────────────────────────────────────────

# Print quick start instructions
quickstart:
    @echo "Starlang Chat - Quick Start Guide"
    @echo "=================================="
    @echo ""
    @echo "1. Start the first server node:"
    @echo "   just node1"
    @echo ""
    @echo "2. In another terminal, start a second server:"
    @echo "   just node2"
    @echo ""
    @echo "3. In another terminal, connect as alice (auto-joins #general):"
    @echo "   just client-alice"
    @echo ""
    @echo "4. In another terminal, connect as bob on node2:"
    @echo "   just client-charlie"
    @echo ""
    @echo "5. Start chatting! Messages are shared across nodes."
    @echo ""
    @echo "Available client shortcuts:"
    @echo "   just client-alice    - alice on node1, auto-join #general"
    @echo "   just client-bob      - bob on node1, auto-join #general"
    @echo "   just client-charlie  - charlie on node2, auto-join #general"
    @echo "   just client-dave     - dave on node2, auto-join #general"
    @echo "   just client-eve      - eve on node3, auto-join #general"
    @echo ""
    @echo "Custom client:"
    @echo "   just chat <nick> <room> [port]"
    @echo "   just chat frank lobby 9999"
    @echo ""
    @echo "In-chat commands:"
    @echo "   /nick <name>  - Change nickname"
    @echo "   /join <room>  - Join a room"
    @echo "   /leave        - Leave current room"
    @echo "   /rooms        - List available rooms"
    @echo "   /quit         - Disconnect"

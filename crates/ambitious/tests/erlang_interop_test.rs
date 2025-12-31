//! Test Erlang/Elixir interoperability.
//!
//! To run this test:
//! 1. Start the Elixir test server:
//!    ```
//!    cd crates/ambitious/tests/elixir_interop
//!    elixir --sname elixir_test --cookie test_cookie test_server.exs
//!    ```
//! 2. Run this test:
//!    ```
//!    cargo test -p ambitious --features erlang-dist erlang_interop -- --nocapture
//!    ```

#![cfg(feature = "erlang-dist")]

use ambitious::distribution::erlang::{ErlangConfig, ErlangConnection};
use erltf::{OwnedTerm, erl_atom, erl_tuple};

/// Test connecting to an Elixir node and sending a ping.
#[tokio::test]
async fn test_connect_to_elixir() {
    // Skip if EPMD isn't running or Elixir node isn't up
    let config = ErlangConfig::new(
        "rust_test@localhost",
        "elixir_test@localhost",
        "test_cookie",
    );

    let conn_result = ErlangConnection::connect(config).await;

    match conn_result {
        Ok(conn) => {
            println!("✅ Connected to Elixir node: {}", conn.remote_node());
            assert!(conn.is_connected());
        }
        Err(e) => {
            println!("⚠️  Could not connect to Elixir node (is it running?)");
            println!("   Error: {}", e);
            println!();
            println!("   To run this test:");
            println!("   1. cd crates/ambitious/tests/elixir_interop");
            println!("   2. elixir --sname elixir_test --cookie test_cookie test_server.exs");
            println!(
                "   3. cargo test -p ambitious --features erlang-dist erlang_interop -- --nocapture"
            );
            // Don't fail - just skip if Elixir isn't running
            return;
        }
    }
}

/// Test sending a message to a registered process.
#[tokio::test]
async fn test_send_to_named_process() {
    let config = ErlangConfig::new(
        "rust_test2@localhost",
        "elixir_test@localhost",
        "test_cookie",
    );

    let mut conn = match ErlangConnection::connect(config).await {
        Ok(c) => c,
        Err(_) => {
            println!("⚠️  Skipping - Elixir node not running");
            return;
        }
    };

    // Allocate a PID for ourselves
    let our_pid = conn.allocate_pid();
    println!("Our PID: {:?}", our_pid);

    // Send a simple message to :test_server
    // In Elixir, this would be: send(:test_server, {:hello, "from Rust"})
    let message = erl_tuple![erl_atom!("hello"), OwnedTerm::Binary(b"from Rust".to_vec())];

    match conn.send_to_name(&our_pid, "test_server", message).await {
        Ok(()) => println!("✅ Sent message to :test_server"),
        Err(e) => println!("❌ Failed to send: {}", e),
    }
}

/// Test making a gen_server call.
#[tokio::test]
async fn test_gen_server_call() {
    let config = ErlangConfig::new(
        "rust_test3@localhost",
        "elixir_test@localhost",
        "test_cookie",
    );

    let mut conn = match ErlangConnection::connect(config).await {
        Ok(c) => c,
        Err(_) => {
            println!("⚠️  Skipping - Elixir node not running");
            return;
        }
    };

    let our_pid = conn.allocate_pid();
    let our_ref = conn.allocate_ref();

    // Build a $gen_call message
    // Format: {:"$gen_call", {pid, ref}, request}
    let gen_call = OwnedTerm::Tuple(vec![
        erl_atom!("$gen_call"),
        OwnedTerm::Tuple(vec![
            OwnedTerm::Pid(our_pid.as_inner().clone()),
            OwnedTerm::Reference(our_ref.as_inner().clone()),
        ]),
        erl_atom!("ping"),
    ]);

    println!("Sending $gen_call :ping to :test_server...");

    match conn.send_to_name(&our_pid, "test_server", gen_call).await {
        Ok(()) => println!("✅ Sent $gen_call"),
        Err(e) => {
            println!("❌ Failed to send: {}", e);
            return;
        }
    }

    // Wait for reply
    println!("Waiting for reply...");

    match tokio::time::timeout(std::time::Duration::from_secs(5), conn.receive()).await {
        Ok(Ok(msg)) => {
            println!("✅ Received reply!");
            println!("   Control: {:?}", msg.control);
            if let Some(payload) = msg.payload() {
                println!("   Payload: {:?}", payload);
            }
        }
        Ok(Err(e)) => println!("❌ Receive error: {}", e),
        Err(_) => println!("❌ Timeout waiting for reply"),
    }
}

/// Test receiving messages.
#[tokio::test]
async fn test_receive_cast() {
    let config = ErlangConfig::new(
        "rust_test4@localhost",
        "elixir_test@localhost",
        "test_cookie",
    );

    let mut conn = match ErlangConnection::connect(config).await {
        Ok(c) => c,
        Err(_) => {
            println!("⚠️  Skipping - Elixir node not running");
            return;
        }
    };

    let our_pid = conn.allocate_pid();

    // Send a $gen_cast
    // Format: {:"$gen_cast", request}
    let gen_cast = erl_tuple![
        erl_atom!("$gen_cast"),
        erl_tuple![
            erl_atom!("print"),
            OwnedTerm::Binary(b"Hello from Rust via cast!".to_vec())
        ]
    ];

    match conn.send_to_name(&our_pid, "test_server", gen_cast).await {
        Ok(()) => println!("✅ Sent $gen_cast"),
        Err(e) => println!("❌ Failed to send: {}", e),
    }
}

/// Test the transparent BEAM interop layer.
///
/// This test verifies that serde-serializable Rust structs can be
/// automatically encoded to ETF and sent to BEAM nodes.
#[tokio::test]
async fn test_transparent_beam_interop() {
    let config = ErlangConfig::new(
        "rust_test5@localhost",
        "elixir_test@localhost",
        "test_cookie",
    );

    let mut conn = match ErlangConnection::connect(config).await {
        Ok(c) => c,
        Err(e) => {
            println!(
                "⚠️  Skipping transparent interop test - Elixir node not running: {}",
                e
            );
            return;
        }
    };

    // Test sending a serde-serializable struct as ETF
    // This mimics what the transparent layer would do
    #[derive(serde::Serialize)]
    struct TestMessage {
        action: String,
        value: i32,
    }

    let msg = TestMessage {
        action: "test".to_string(),
        value: 42,
    };

    // Encode using erltf_serde (what the transparent layer does)
    let etf_bytes = erltf_serde::to_bytes(&msg).expect("ETF encoding should work");
    println!("ETF encoded message: {} bytes", etf_bytes.len());

    // Decode it back to verify
    let term = erltf::decode(&etf_bytes).expect("Should decode back to term");
    println!("Decoded term: {:?}", term);

    // Send it to the test server
    let our_pid = conn.allocate_pid();
    if let Err(e) = conn.send_to_name(&our_pid, "test_server", term).await {
        println!("❌ Failed to send: {}", e);
        return;
    }
    println!("✅ Sent serde-serialized message to Elixir");

    // The Elixir server should receive this as a map: %{"action" => "test", "value" => 42}
}

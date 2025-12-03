//! Integration tests for dream-macros.

use starlang_macros::{starlang_process, GenServerImpl};

#[derive(GenServerImpl)]
struct TestServer {
    counter: i64,
}

#[test]
fn test_gen_server_impl_derive() {
    assert_eq!(TestServer::type_name(), "TestServer");
}

#[starlang_process]
async fn test_process() {
    // This just needs to compile
}

#[test]
fn test_starlang_process_attribute() {
    // The attribute should not change the function signature
    // This test passes if it compiles
}

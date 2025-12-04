//! Integration tests for ambitious-macros.

use ambitious_macros::{GenServerImpl, ambitious_process};

#[derive(GenServerImpl)]
struct TestServer {
    #[allow(dead_code)]
    counter: i64,
}

#[test]
fn test_gen_server_impl_derive() {
    assert_eq!(TestServer::type_name(), "TestServer");
}

#[ambitious_process]
#[allow(dead_code)]
async fn test_process() {
    // This just needs to compile
}

#[test]
fn test_ambitious_process_attribute() {
    // The attribute should not change the function signature
    // This test passes if it compiles
}

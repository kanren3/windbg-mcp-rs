use std::collections::HashMap;

use windbg_mcp_rs::{Catalog, CommandDispatcher, ExecutionMode};

#[tokio::test]
async fn mock_dispatcher_returns_scripted_output() {
    let mut responses = HashMap::new();
    responses.insert(
        "dt _PEB_LDR_DATA".to_string(),
        "ntdll!_PEB_LDR_DATA".to_string(),
    );

    let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock { responses })
        .expect("mock dispatcher should start");

    let output = dispatcher
        .execute("dt _PEB_LDR_DATA")
        .await
        .expect("mock command should succeed");

    assert_eq!(output.output, "ntdll!_PEB_LDR_DATA");
}

#[test]
fn catalog_exposes_resource_uri() {
    let entry = Catalog::global().lookup("dt").expect("dt entry must exist");
    assert_eq!(entry.resource_uri(), "windbg://command/dt_display_type");
}

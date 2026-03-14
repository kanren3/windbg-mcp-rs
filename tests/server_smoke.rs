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

    assert_eq!(output, "ntdll!_PEB_LDR_DATA");
}

#[tokio::test]
async fn mock_dispatcher_interrupt_returns_status() {
    let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock {
        responses: HashMap::new(),
    })
    .expect("mock dispatcher should start");

    let output = dispatcher
        .interrupt()
        .await
        .expect("mock interrupt should succeed");

    assert_eq!(output, "mock-interrupted");
}

#[test]
fn catalog_exposes_resource_tool_and_prompt_names() {
    let entry = Catalog::global().lookup("dt").expect("dt entry must exist");
    assert_eq!(entry.tool_name(), "windbg_cmd_dt__display_type_");
    assert_eq!(entry.prompt_name(), "windbg_prompt_dt__display_type_");
    assert_eq!(entry.resource_uri(), "windbg://command/dt__display_type_");
}

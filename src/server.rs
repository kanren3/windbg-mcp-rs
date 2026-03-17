use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, handler::server::common::schema_for_type,
    model::*, schemars::JsonSchema, service::RequestContext,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    catalog::{Catalog, CatalogEntry, CatalogResourceKind, CatalogSection},
    executor::CommandDispatcher,
    resources::{GUIDE_URI, render_compact_command, render_full_command, render_guide},
};

#[cfg(test)]
use crate::executor::build_command;

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecuteRawArgs {
    command: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct InterruptTargetArgs {}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct GetExecutionStateArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchCatalogArgs {
    query: String,
    section: Option<CatalogSection>,
    limit: Option<u32>,
}

#[derive(Clone)]
pub struct WindbgMcpServer {
    dispatcher: Arc<CommandDispatcher>,
}

impl WindbgMcpServer {
    pub fn new(dispatcher: CommandDispatcher) -> Self {
        Self {
            dispatcher: Arc::new(dispatcher),
        }
    }

    fn catalog(&self) -> &'static Catalog {
        Catalog::global()
    }

    fn parse_arguments<T>(&self, arguments: Option<JsonObject>) -> Result<T, McpError>
    where
        T: for<'de> Deserialize<'de>,
    {
        serde_json::from_value(Value::Object(arguments.unwrap_or_default()))
            .map_err(|error| McpError::invalid_params(error.to_string(), None))
    }

    fn generic_command_tool(&self) -> Tool {
        Tool::new(
            "windbg_execute_command",
            "Execute a WinDbg command string through dbgeng. The debugger must already be ready for commands; query state first and interrupt explicitly when needed.",
            schema_for_type::<ExecuteRawArgs>(),
        )
        .with_title("Execute WinDbg command")
    }

    fn state_tool(&self) -> Tool {
        Tool::new(
            "windbg_get_state",
            "Query the current debugger execution state before deciding whether to interrupt or execute a command.",
            schema_for_type::<GetExecutionStateArgs>(),
        )
        .with_title("Get debugger execution state")
    }

    fn interrupt_tool(&self) -> Tool {
        Tool::new(
            "windbg_interrupt_target",
            "Request a debugger break into the currently running target and wait until debugger commands are accepted again.",
            schema_for_type::<InterruptTargetArgs>(),
        )
        .with_title("Interrupt running target")
    }

    fn search_tool(&self) -> Tool {
        Tool::new(
            "windbg_search_catalog",
            "Search the static debugger command catalog extracted from debugger.chm and return the best low-context resources to read before execution.",
            schema_for_type::<SearchCatalogArgs>(),
        )
        .with_title("Search WinDbg catalog")
    }

    fn syntax_preview(&self, entry: &CatalogEntry) -> Option<String> {
        let syntax = entry.syntax_block()?;
        let syntax = syntax.trim();
        if syntax.is_empty() {
            return None;
        }

        let preview = syntax
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(syntax);
        let preview = preview.trim();
        if preview.len() <= 160 {
            Some(preview.to_string())
        } else {
            Some(format!("{}...", &preview[..157]))
        }
    }

    #[cfg(test)]
    async fn run_entry_tool(
        &self,
        entry: &CatalogEntry,
        variant: Option<&str>,
        arguments: Option<&str>,
    ) -> Result<CallToolResult, McpError> {
        if !entry.supports_text_execution {
            let content = format!(
                "{} is documented as a keyboard action or non-text entry and cannot be executed as a raw debugger command string. Read {} for the official documentation.",
                entry.title,
                entry.resource_uri()
            );
            return Ok(CallToolResult::error(vec![Content::text(content)]));
        }

        let command = build_command(entry, variant, arguments)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let execution = self
            .dispatcher
            .execute(command.clone())
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;

        Ok(CallToolResult::structured(json!({
            "entry_id": entry.id,
            "title": entry.title,
            "command": command,
            "output": execution.output,
            "state_before": execution.state_before,
            "state_after": execution.state_after,
        })))
    }
}

impl ServerHandler for WindbgMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_instructions(
            "This server is organized around low-context resources plus a small toolset. Start with `windbg_search_catalog`, read `windbg://command/{id}`, optionally escalate to `windbg://command-full/{id}`, then call `windbg_get_state`. If the debugger is running or busy, call `windbg_interrupt_target` and verify state again before calling `windbg_execute_command`.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: vec![
                self.generic_command_tool(),
                self.state_tool(),
                self.search_tool(),
                self.interrupt_tool(),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        match name {
            "windbg_execute_command" => Some(self.generic_command_tool()),
            "windbg_get_state" => Some(self.state_tool()),
            "windbg_search_catalog" => Some(self.search_tool()),
            "windbg_interrupt_target" => Some(self.interrupt_tool()),
            _ => None,
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "windbg_execute_command" => {
                let args: ExecuteRawArgs = self.parse_arguments(request.arguments)?;
                let execution = self
                    .dispatcher
                    .execute(args.command.clone())
                    .await
                    .map_err(|error| McpError::internal_error(error.to_string(), None))?;
                Ok(CallToolResult::structured(json!({
                    "command": execution.command,
                    "output": execution.output,
                    "state_before": execution.state_before,
                    "state_after": execution.state_after,
                })))
            }
            "windbg_get_state" => {
                let _: GetExecutionStateArgs = self.parse_arguments(request.arguments)?;
                let state = self
                    .dispatcher
                    .state()
                    .await
                    .map_err(|error| McpError::internal_error(error.to_string(), None))?;
                Ok(CallToolResult::structured(json!({
                    "state": state,
                })))
            }
            "windbg_search_catalog" => {
                let args: SearchCatalogArgs = self.parse_arguments(request.arguments)?;
                let limit = args.limit.unwrap_or(10).clamp(1, 100) as usize;
                let matches = self.catalog().search(&args.query, args.section, limit);
                let payload: Vec<Value> = matches
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "id": entry.id,
                            "primary_token": entry.primary_token(),
                            "title": entry.title,
                            "tokens": entry.tokens,
                            "summary": entry.summary,
                            "supports_text_execution": entry.supports_text_execution,
                            "syntax_preview": self.syntax_preview(entry),
                            "resource": entry.resource_uri(),
                            "full_resource": entry.full_resource_uri(),
                            "routing": entry.tool_routing_name(),
                            "recommended_tool": entry.recommended_tool(),
                            "execution_state_tool": "windbg_get_state",
                        })
                    })
                    .collect();
                Ok(CallToolResult::structured(json!({
                    "query": args.query,
                    "recommended_flow": [
                        "call windbg_search_catalog",
                        "read the compact resource for the best match",
                        "read the full resource only if needed",
                        "call windbg_get_state",
                        "if needed, call windbg_interrupt_target and verify state again",
                        "call windbg_execute_command or another recommended tool"
                    ],
                    "matches": payload,
                })))
            }
            "windbg_interrupt_target" => {
                let _: InterruptTargetArgs = self.parse_arguments(request.arguments)?;
                let state = self
                    .dispatcher
                    .interrupt()
                    .await
                    .map_err(|error| McpError::internal_error(error.to_string(), None))?;
                Ok(CallToolResult::structured(json!({
                    "state": state,
                })))
            }
            _ => Err(McpError::method_not_found::<CallToolRequestMethod>()),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let guide = render_guide(self.catalog());

        Ok(ListResourcesResult {
            resources: vec![
                RawResource::new(GUIDE_URI, "windbg guide")
                    .with_title("WinDbg MCP guide")
                    .with_description(
                        "Low-context workflow for mapping debugger requests to resources and tools",
                    )
                    .with_mime_type("text/plain")
                    .with_size(guide.len() as u32)
                    .no_annotation(),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            resource_templates: vec![
                RawResourceTemplate::new(
                    self.catalog().command_template_uri(),
                    "windbg compact command card",
                )
                .with_description(
                    "Compact syntax-first WinDbg command card by extracted catalog id",
                )
                .with_mime_type("text/plain")
                .no_annotation(),
                RawResourceTemplate::new(
                    self.catalog().full_command_template_uri(),
                    "windbg full command page",
                )
                .with_description("Full extracted debugger command topic by extracted catalog id")
                .with_mime_type("text/plain")
                .no_annotation(),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        if request.uri == GUIDE_URI || request.uri == self.catalog().command_index_uri() {
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                render_guide(self.catalog()),
                request.uri,
            )]));
        }

        let (kind, entry) = self
            .catalog()
            .resolve_resource_uri(&request.uri)
            .ok_or_else(|| {
                McpError::resource_not_found(
                    "unknown_resource",
                    Some(json!({ "uri": request.uri })),
                )
            })?;
        let content = match kind {
            CatalogResourceKind::Compact => render_compact_command(entry),
            CatalogResourceKind::Full => render_full_command(entry),
        };

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            content,
            request.uri,
        )]))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::executor::{DebuggerExecutionState, ExecutionMode};

    #[tokio::test]
    async fn command_tool_uses_mock_dispatcher() {
        let mut responses = HashMap::new();
        responses.insert(
            "dt _PEB_LDR_DATA".to_string(),
            "ntdll!_PEB_LDR_DATA".to_string(),
        );
        let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock { responses })
            .expect("dispatcher should start");
        let server = WindbgMcpServer::new(dispatcher);
        let entry = server
            .catalog()
            .lookup("dt")
            .expect("dt entry should exist");

        let result = server
            .run_entry_tool(entry, None, Some("_PEB_LDR_DATA"))
            .await
            .expect("tool should succeed");

        let payload = result
            .structured_content
            .expect("structured payload expected");
        assert_eq!(payload["command"], "dt _PEB_LDR_DATA");
        assert_eq!(payload["output"], "ntdll!_PEB_LDR_DATA");
    }

    #[test]
    fn interrupt_tool_is_exposed() {
        let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock {
            responses: HashMap::new(),
        })
        .expect("dispatcher should start");
        let server = WindbgMcpServer::new(dispatcher);

        let tool = server
            .get_tool("windbg_interrupt_target")
            .expect("interrupt tool should be listed");
        assert_eq!(tool.name, "windbg_interrupt_target");
    }

    #[test]
    fn command_tool_is_exposed() {
        let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock {
            responses: HashMap::new(),
        })
        .expect("dispatcher should start");
        let server = WindbgMcpServer::new(dispatcher);

        let tool = server
            .get_tool("windbg_execute_command")
            .expect("command tool should be listed");
        assert_eq!(tool.name, "windbg_execute_command");
    }

    #[test]
    fn state_tool_is_exposed() {
        let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock {
            responses: HashMap::new(),
        })
        .expect("dispatcher should start");
        let server = WindbgMcpServer::new(dispatcher);

        let tool = server
            .get_tool("windbg_get_state")
            .expect("state tool should be listed");
        assert_eq!(tool.name, "windbg_get_state");
    }

    #[test]
    fn compact_resource_stays_small_and_points_to_full_doc() {
        let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock {
            responses: HashMap::new(),
        })
        .expect("dispatcher should start");
        let server = WindbgMcpServer::new(dispatcher);
        let entry = server
            .catalog()
            .lookup("bp")
            .expect("bp entry should exist");

        let resource = render_compact_command(entry);
        assert!(resource.contains("Syntax"));
        assert!(resource.contains("windbg://command-full/bp_bu_bm_set_breakpoint"));
        assert!(resource.contains("Next Step"));
    }

    #[test]
    fn syntax_preview_uses_inferred_syntax_when_structured_syntax_is_missing() {
        let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock {
            responses: HashMap::new(),
        })
        .expect("dispatcher should start");
        let server = WindbgMcpServer::new(dispatcher);
        let entry = server
            .catalog()
            .lookup("bp")
            .expect("bp entry should exist");

        let preview = server.syntax_preview(entry).expect("preview should exist");
        assert!(preview.contains("User-Mode"));
    }

    #[tokio::test]
    async fn state_tool_returns_mock_break_state() {
        let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock {
            responses: HashMap::new(),
        })
        .expect("dispatcher should start");
        let state = dispatcher
            .state()
            .await
            .expect("state query should succeed");
        assert_eq!(state, DebuggerExecutionState::break_state());
    }
}

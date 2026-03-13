use std::sync::Arc;

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, handler::server::common::schema_for_type,
    model::*, schemars::JsonSchema, service::RequestContext,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    catalog::{Catalog, CatalogEntry, CatalogSection},
    executor::{CommandDispatcher, build_command},
};

#[derive(Debug, Deserialize, JsonSchema)]
struct ExecuteRawArgs {
    command: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchCatalogArgs {
    query: String,
    section: Option<CatalogSection>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DocumentedCommandArgs {
    variant: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CommandPromptArgs {
    user_request: String,
    variant: Option<String>,
    notes: Option<String>,
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
            "windbg_execute_raw",
            "Execute a raw WinDbg command string through dbgeng.",
            schema_for_type::<ExecuteRawArgs>(),
        )
        .with_title("Execute raw WinDbg command")
    }

    fn search_tool(&self) -> Tool {
        Tool::new(
            "windbg_search_catalog",
            "Search the static debugger command catalog extracted from debugger.chm.",
            schema_for_type::<SearchCatalogArgs>(),
        )
        .with_title("Search WinDbg catalog")
    }

    fn entry_tool(&self, entry: &CatalogEntry) -> Tool {
        let mut description = entry.summary.clone();
        if entry.variants_required() {
            description.push_str(" Variants: ");
            description.push_str(&entry.tokens.join(", "));
            description.push('.');
        }
        if !entry.supports_text_execution {
            description.push_str(
                " This topic is documented as a keyboard action, so the tool returns documentation guidance instead of executing debugger text.",
            );
        }

        Tool::new(
            entry.tool_name(),
            description,
            schema_for_type::<DocumentedCommandArgs>(),
        )
        .with_title(entry.title.clone())
    }

    fn entry_prompt(&self, entry: &CatalogEntry) -> Prompt {
        Prompt::new(
            entry.prompt_name(),
            Some(format!(
                "Use the official documentation for {} to plan a tool call.",
                entry.title
            )),
            Some(vec![
                PromptArgument::new("user_request")
                    .with_description("What the user wants to learn or do in the debugger")
                    .with_required(true),
                PromptArgument::new("variant").with_description(
                    "Optional documented command token when the page covers multiple tokens",
                ),
                PromptArgument::new("notes")
                    .with_description("Optional extra constraints for the command construction"),
            ]),
        )
        .with_title(entry.title.clone())
    }

    fn render_resource(&self, entry: &CatalogEntry) -> String {
        let mut output = String::new();
        output.push_str(&format!("Title: {}\n", entry.title));
        output.push_str(&format!("Section: {:?}\n", entry.section));
        output.push_str(&format!("Tokens: {}\n", entry.tokens.join(", ")));
        output.push_str(&format!("Topic: {}\n", entry.topic_path));
        output.push_str(&format!("Tool: {}\n", entry.tool_name()));
        output.push_str(&format!("Prompt: {}\n", entry.prompt_name()));
        output.push_str(&format!("Summary: {}\n", entry.summary));

        if let Some(syntax) = &entry.user_mode_syntax {
            output.push_str("\nUser-Mode Syntax\n----------------\n");
            output.push_str(syntax);
            output.push('\n');
        }

        if let Some(syntax) = &entry.kernel_mode_syntax {
            output.push_str("\nKernel-Mode Syntax\n------------------\n");
            output.push_str(syntax);
            output.push('\n');
        }

        output.push_str("\nDocumentation\n-------------\n");
        output.push_str(&entry.documentation);
        output
    }

    fn render_prompt(&self, entry: &CatalogEntry, args: CommandPromptArgs) -> GetPromptResult {
        let tool_name = entry.tool_name();
        let variants = entry.tokens.join(", ");
        let syntax = entry
            .user_mode_syntax
            .as_deref()
            .or(entry.kernel_mode_syntax.as_deref())
            .unwrap_or("No syntax block was present in this topic.");

        let mut guidance = format!(
            "Official WinDbg topic: {}\n\nSummary: {}\n\nDocumented tokens: {}\n\nSyntax:\n{}\n\nUse only the documented command variants and arguments from this topic. After deciding on the exact invocation, call the MCP tool `{}`.",
            entry.title, entry.summary, variants, syntax, tool_name
        );

        if let Some(notes) = args.notes.as_deref() {
            guidance.push_str("\n\nExtra notes: ");
            guidance.push_str(notes);
        }

        if let Some(variant) = args.variant.as_deref() {
            guidance.push_str("\n\nPreferred variant: ");
            guidance.push_str(variant);
        }

        GetPromptResult::new(vec![
            PromptMessage::new_text(PromptMessageRole::Assistant, guidance),
            PromptMessage::new_text(PromptMessageRole::User, args.user_request),
        ])
        .with_description(format!("Prompt for {}", entry.title))
    }

    async fn run_entry_tool(
        &self,
        entry: &CatalogEntry,
        args: DocumentedCommandArgs,
    ) -> Result<CallToolResult, McpError> {
        if !entry.supports_text_execution {
            let content = format!(
                "{} is documented as a keyboard action or non-text entry and cannot be executed as a raw debugger command string. Read {} for the official documentation.",
                entry.title,
                entry.resource_uri()
            );
            return Ok(CallToolResult::error(vec![Content::text(content)]));
        }

        let command = build_command(entry, args.variant.as_deref(), args.arguments.as_deref())
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        let output = self
            .dispatcher
            .execute(command.clone())
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;

        Ok(CallToolResult::structured(json!({
            "entry_id": entry.id,
            "title": entry.title,
            "command": command,
            "output": output,
        })))
    }
}

impl ServerHandler for WindbgMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_prompts()
                .enable_resources()
                .enable_tools()
                .build(),
        )
        .with_instructions(
            "This server exposes WinDbg debugger command documentation extracted from debugger.chm. Use command resources for official documentation, prompt entries to plan a documented invocation, and tools to execute debugger commands through dbgeng without inventing undocumented behavior.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut tools = vec![self.generic_command_tool(), self.search_tool()];
        tools.extend(
            self.catalog()
                .entries()
                .iter()
                .map(|entry| self.entry_tool(entry)),
        );
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        if name == "windbg_execute_raw" {
            return Some(self.generic_command_tool());
        }
        if name == "windbg_search_catalog" {
            return Some(self.search_tool());
        }
        self.catalog()
            .get_by_tool_name(name)
            .map(|entry| self.entry_tool(entry))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "windbg_execute_raw" => {
                let args: ExecuteRawArgs = self.parse_arguments(request.arguments)?;
                let output = self
                    .dispatcher
                    .execute(args.command.clone())
                    .await
                    .map_err(|error| McpError::internal_error(error.to_string(), None))?;
                Ok(CallToolResult::structured(json!({
                    "command": args.command,
                    "output": output,
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
                            "title": entry.title,
                            "tokens": entry.tokens,
                            "summary": entry.summary,
                            "tool": entry.tool_name(),
                            "resource": entry.resource_uri(),
                        })
                    })
                    .collect();
                Ok(CallToolResult::structured(json!({
                    "query": args.query,
                    "matches": payload,
                })))
            }
            name => {
                let entry = self
                    .catalog()
                    .get_by_tool_name(name)
                    .ok_or_else(|| McpError::method_not_found::<CallToolRequestMethod>())?;
                let args: DocumentedCommandArgs = self.parse_arguments(request.arguments)?;
                self.run_entry_tool(entry, args).await
            }
        }
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let prompts = self
            .catalog()
            .entries()
            .iter()
            .map(|entry| self.entry_prompt(entry))
            .collect();
        Ok(ListPromptsResult {
            prompts,
            next_cursor: None,
            meta: None,
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let entry = self
            .catalog()
            .get_by_prompt_name(&request.name)
            .ok_or_else(|| McpError::method_not_found::<GetPromptRequestMethod>())?;
        let args: CommandPromptArgs = self.parse_arguments(request.arguments)?;
        Ok(self.render_prompt(entry, args))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let mut resources = vec![
            RawResource::new(self.catalog().command_index_uri(), "windbg catalog")
                .with_title("WinDbg command catalog index")
                .with_description("All command and meta-command topics extracted from debugger.chm")
                .with_mime_type("text/plain")
                .with_size(self.catalog().render_index().len() as u32)
                .no_annotation(),
        ];

        resources.extend(self.catalog().entries().iter().map(|entry| {
            RawResource::new(entry.resource_uri(), entry.title.clone())
                .with_description(entry.summary.clone())
                .with_mime_type("text/plain")
                .with_size(entry.documentation.len() as u32)
                .no_annotation()
        }));

        Ok(ListResourcesResult {
            resources,
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
                    "windbg command documentation",
                )
                .with_description("Official debugger command topic by extracted catalog id")
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
        if request.uri == self.catalog().command_index_uri() {
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                self.catalog().render_index(),
                request.uri,
            )]));
        }

        let entry = self
            .catalog()
            .get_by_resource_uri(&request.uri)
            .ok_or_else(|| {
                McpError::resource_not_found(
                    "unknown_resource",
                    Some(json!({ "uri": request.uri })),
                )
            })?;
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            self.render_resource(entry),
            request.uri,
        )]))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::executor::ExecutionMode;

    #[tokio::test]
    async fn documented_tool_uses_mock_dispatcher() {
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
            .run_entry_tool(
                entry,
                DocumentedCommandArgs {
                    variant: None,
                    arguments: Some("_PEB_LDR_DATA".to_string()),
                },
            )
            .await
            .expect("tool should succeed");

        let payload = result
            .structured_content
            .expect("structured payload expected");
        assert_eq!(payload["command"], "dt _PEB_LDR_DATA");
        assert_eq!(payload["output"], "ntdll!_PEB_LDR_DATA");
    }
}

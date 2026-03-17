use crate::catalog::{Catalog, CatalogEntry, ToolRouting};

pub const GUIDE_URI: &str = "windbg://guide/overview";

pub fn render_guide(catalog: &Catalog) -> String {
    let mut output = String::new();
    output.push_str("WinDbg MCP overview\n\n");
    output.push_str("Use resources to decide what to execute before calling tools.\n\n");
    output.push_str("Low-context workflow\n");
    output.push_str("--------------------\n");
    output.push_str("1. If the user request is not already a precise WinDbg command, call `windbg_search_catalog`.\n");
    output.push_str("2. Read `windbg://command/{id}` for the best match. It keeps only the metadata and syntax needed to build the command string.\n");
    output.push_str(
        "3. Read `windbg://command-full/{id}` only when the compact card is not enough.\n",
    );
    output.push_str("4. Call `windbg_get_state` before execution.\n");
    output.push_str("5. If the debugger is running or busy, call `windbg_interrupt_target`, then re-check state.\n");
    output.push_str(
        "6. Call `windbg_execute_command` only when the debugger is ready for commands.\n\n",
    );
    output.push_str("Key resources\n");
    output.push_str("-------------\n");
    output.push_str(&format!("- Guide: {}\n", GUIDE_URI));
    output.push_str(&format!(
        "- Compact command card template: {}\n",
        catalog.command_template_uri()
    ));
    output.push_str(&format!(
        "- Full command page template: {}\n\n",
        catalog.full_command_template_uri()
    ));
    output.push_str("Key tools\n");
    output.push_str("---------\n");
    output.push_str("- windbg_search_catalog\n");
    output.push_str("- windbg_get_state\n");
    output.push_str("- windbg_interrupt_target\n");
    output.push_str("- windbg_execute_command\n\n");
    output.push_str(&catalog.render_index());
    output
}

pub fn render_compact_command(entry: &CatalogEntry) -> String {
    let mut output = String::new();
    output.push_str(&format!("Title: {}\n", entry.title));
    output.push_str(&format!("Catalog Id: {}\n", entry.id));
    output.push_str(&format!("Tokens: {}\n", entry.tokens.join(", ")));
    output.push_str(&format!("Summary: {}\n", entry.summary));
    output.push_str(&format!(
        "Tool Route: {}\n",
        tool_route_label(entry.tool_routing())
    ));

    match entry.recommended_tool() {
        Some(tool) => output.push_str(&format!("Recommended Tool: {}\n", tool)),
        None => output.push_str("Recommended Tool: documentation only\n"),
    }

    output.push_str(&format!("Full Resource: {}\n", entry.full_resource_uri()));

    if let Some(syntax) = entry.syntax_block() {
        output.push_str("\nSyntax\n------\n");
        output.push_str(&syntax);
        output.push('\n');
    }

    output.push_str("\nNext Step\n---------\n");
    match entry.tool_routing() {
        ToolRouting::ExecuteCommand => output.push_str(
            "Build the final WinDbg command string from the syntax above, call `windbg_get_state`, interrupt if needed, and then call `windbg_execute_command`.\n",
        ),
        ToolRouting::InterruptTarget => output.push_str(
            "This topic maps to an engine-level break action. Use `windbg_interrupt_target` instead of `windbg_execute_command`.\n",
        ),
        ToolRouting::DocumentationOnly => output.push_str(
            "This topic is documentation-only in MCP because it describes a UI shortcut or non-text action.\n",
        ),
    }

    output
}

pub fn render_full_command(entry: &CatalogEntry) -> String {
    let mut output = render_compact_command(entry);
    output.push_str("\nDocumentation\n-------------\n");
    output.push_str(&entry.documentation);
    output
}

fn tool_route_label(route: ToolRouting) -> &'static str {
    match route {
        ToolRouting::ExecuteCommand => "execute_command",
        ToolRouting::InterruptTarget => "interrupt_target",
        ToolRouting::DocumentationOnly => "documentation_only",
    }
}

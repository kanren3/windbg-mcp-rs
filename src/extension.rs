use std::ffi::CString;

use windows::{
    Win32::{
        Foundation::{E_FAIL, E_POINTER, S_OK},
        System::Diagnostics::Debug::Extensions::{
            DEBUG_OUTPUT_ERROR, DEBUG_OUTPUT_NORMAL, IDebugClient, IDebugControl,
        },
    },
    core::{HRESULT, Interface, PCSTR, Ref, Result as WinResult},
};

use crate::{Catalog, executor::DbgEngExecutor, plugin_server::PluginServerControl};

const EXTENSION_MAJOR: u32 = 0;
const EXTENSION_MINOR: u32 = 1;

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DebugExtensionInitialize(
    version: *mut u32,
    flags: *mut u32,
) -> HRESULT {
    if version.is_null() || flags.is_null() {
        return E_POINTER;
    }

    unsafe {
        *version = (EXTENSION_MAJOR << 16) | EXTENSION_MINOR;
        *flags = 0;
    }
    S_OK
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DebugExtensionUninitialize() {
    let _ = PluginServerControl::stop();
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn mcp(client: Ref<IDebugClient>, args: PCSTR) -> HRESULT {
    match run_mcp_command(client, args) {
        Ok(()) => S_OK,
        Err(error) => error.code(),
    }
}

fn run_mcp_command(client: Ref<IDebugClient>, args: PCSTR) -> WinResult<()> {
    let client = client
        .cloned()
        .ok_or_else(|| windows::core::Error::from(E_POINTER))?;
    let control = client.cast::<IDebugControl>()?;
    let raw_args = if args.is_null() {
        String::new()
    } else {
        unsafe { args.to_string() }.unwrap_or_default()
    };
    let trimmed = raw_args.trim();

    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("help") {
        return write_text(&control, help_text(), DEBUG_OUTPUT_NORMAL);
    }

    if let Some(rest) = trimmed.strip_prefix("doc ") {
        return command_doc(&control, rest.trim());
    }

    if let Some(rest) = trimmed.strip_prefix("catalog") {
        return command_catalog(&control, rest.trim());
    }

    if let Some(rest) = trimmed.strip_prefix("serve") {
        return command_serve(&control, rest.trim());
    }

    if trimmed.eq_ignore_ascii_case("status") {
        return command_status(&control);
    }

    if trimmed.eq_ignore_ascii_case("stop") {
        return command_stop(&control);
    }

    if trimmed.eq_ignore_ascii_case("break") || trimmed.eq_ignore_ascii_case("interrupt") {
        return command_interrupt(&control, client);
    }

    if let Some(rest) = trimmed.strip_prefix("exec ") {
        return command_exec(&control, client, rest.trim());
    }

    command_exec(&control, client, trimmed)
}

fn help_text() -> &'static str {
    "windbg-mcp commands:\n\n  !mcp help\n      Show this help text.\n\n  !mcp serve [host:port]\n      Start the MCP Streamable HTTP server inside the WinDbg plugin. Default bind: 127.0.0.1:50051, endpoint: /mcp\n\n  !mcp status\n      Show whether the in-process MCP server is running.\n\n  !mcp stop\n      Stop the in-process MCP server.\n\n  !mcp break\n      Request a debugger break into the currently running target. Alias: !mcp interrupt\n\n  !mcp catalog [query]\n      List catalog entries or search the extracted debugger command catalog.\n\n  !mcp doc <token-or-id>\n      Show the static documentation for one extracted command topic.\n\n  !mcp exec <debugger command>\n      Execute a raw debugger command through dbgeng and print the captured output.\n\nIf the first token is not recognized as a subcommand, !mcp treats the input as `exec`."
}

fn command_serve(control: &IDebugControl, bind: &str) -> WinResult<()> {
    match PluginServerControl::start((!bind.is_empty()).then_some(bind)) {
        Ok(status) => write_text(
            control,
            &format!("WinDbg MCP server is running at {}\n", status.mcp_url),
            DEBUG_OUTPUT_NORMAL,
        ),
        Err(error) => Err(windows::core::Error::new(E_FAIL, error)),
    }
}

fn command_status(control: &IDebugControl) -> WinResult<()> {
    match PluginServerControl::status() {
        Ok(Some(status)) => write_text(
            control,
            &format!("WinDbg MCP server is running at {}\n", status.mcp_url),
            DEBUG_OUTPUT_NORMAL,
        ),
        Ok(None) => write_text(
            control,
            "WinDbg MCP server is not running.\n",
            DEBUG_OUTPUT_NORMAL,
        ),
        Err(error) => Err(windows::core::Error::new(E_FAIL, error)),
    }
}

fn command_stop(control: &IDebugControl) -> WinResult<()> {
    match PluginServerControl::stop() {
        Ok(Some(status)) => write_text(
            control,
            &format!("Stopped WinDbg MCP server at {}\n", status.mcp_url),
            DEBUG_OUTPUT_NORMAL,
        ),
        Ok(None) => write_text(
            control,
            "WinDbg MCP server was not running.\n",
            DEBUG_OUTPUT_NORMAL,
        ),
        Err(error) => Err(windows::core::Error::new(E_FAIL, error)),
    }
}

fn command_interrupt(control: &IDebugControl, client: IDebugClient) -> WinResult<()> {
    let mut executor = DbgEngExecutor::from_existing_client(client)
        .map_err(|error| windows::core::Error::new(E_POINTER, error.to_string()))?;
    match executor.interrupt_target() {
        Ok(output) => write_text(control, &format!("{output}\n"), DEBUG_OUTPUT_NORMAL),
        Err(error) => write_text(control, &format!("{error}\n"), DEBUG_OUTPUT_ERROR),
    }
}

fn command_doc(control: &IDebugControl, query: &str) -> WinResult<()> {
    let catalog = Catalog::global();
    let entry = catalog
        .lookup(query)
        .or_else(|| catalog.search(query, None, 1).into_iter().next());

    match entry {
        Some(entry) => {
            let mut text = String::new();
            text.push_str(&format!("{}\n\n", entry.title));
            text.push_str(&format!("tokens: {}\n", entry.tokens.join(", ")));
            text.push_str(&format!("id: {}\n", entry.id));
            text.push_str(&format!("topic: {}\n\n", entry.topic_path));
            text.push_str(&entry.documentation);
            write_text(control, &text, DEBUG_OUTPUT_NORMAL)
        }
        None => write_text(
            control,
            &format!("No catalog entry matched `{query}`.\n"),
            DEBUG_OUTPUT_ERROR,
        ),
    }
}

fn command_catalog(control: &IDebugControl, query: &str) -> WinResult<()> {
    let catalog = Catalog::global();
    let results = if query.is_empty() {
        catalog.search("", None, 25)
    } else {
        catalog.search(query, None, 25)
    };

    let mut text = String::new();
    if query.is_empty() {
        text.push_str("First 25 extracted debugger command topics:\n\n");
    } else {
        text.push_str(&format!("Catalog matches for `{query}`:\n\n"));
    }

    for entry in results {
        text.push_str(&format!(
            "- {} | id={} | tokens={}\n  {}\n\n",
            entry.title,
            entry.id,
            entry.tokens.join(", "),
            entry.summary
        ));
    }

    write_text(control, &text, DEBUG_OUTPUT_NORMAL)
}

fn command_exec(control: &IDebugControl, client: IDebugClient, command: &str) -> WinResult<()> {
    if command.is_empty() {
        return write_text(
            control,
            "No debugger command was supplied.\n",
            DEBUG_OUTPUT_ERROR,
        );
    }

    let mut executor = DbgEngExecutor::from_existing_client(client)
        .map_err(|error| windows::core::Error::new(E_POINTER, error.to_string()))?;
    match executor.execute_command(command) {
        Ok(output) => write_text(control, &output, DEBUG_OUTPUT_NORMAL),
        Err(error) => write_text(control, &format!("{error}\n"), DEBUG_OUTPUT_ERROR),
    }
}

fn write_text(control: &IDebugControl, text: &str, mask: u32) -> WinResult<()> {
    if text.is_empty() {
        return Ok(());
    }

    for line in text.lines() {
        let mut escaped = line.replace('%', "%%");
        escaped.push('\n');
        let c_text = CString::new(escaped).map_err(|_| windows::core::Error::from(E_POINTER))?;
        unsafe {
            control.Output(mask, PCSTR(c_text.as_ptr() as _))?;
        }
    }
    Ok(())
}

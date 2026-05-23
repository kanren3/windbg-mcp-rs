# windbg-mcp-rs

`windbg-mcp-rs` is a pure WinDbg extension DLL that exposes the current debugging session as an MCP server.

- Read official WinDbg command documentation extracted from `docs/debugger.chm`
- Execute WinDbg commands through dbgeng
- Interrupt a running target from MCP
- Use the server from any MCP client over Streamable HTTP

## Screenshots

![WinDbg MCP plugin screenshot 1](images/1.png)

![WinDbg MCP plugin screenshot 2](images/2.png)

## Quick Start

### 1. Build the DLL

```powershell
# x64 (default)
cargo build --release --target x86_64-pc-windows-msvc

# ARM64
rustup target add aarch64-pc-windows-msvc
cargo build --release --target aarch64-pc-windows-msvc
```

### 2. Install the extension

**One-liner (recommended):**

```powershell
irm https://raw.githubusercontent.com/kanren3/windbg-mcp-rs/main/scripts/install.ps1 | iex
```

This downloads the latest release from GitHub and installs to all discovered WinDbg locations.  
Run as **Administrator** to install to SDK debugger paths.

**From local build (developers):**

```powershell
.\scripts\install.ps1 -LocalPath .\target
```

**Manual install:**

```text
# SDK Debuggers
copy target\release\windbg_mcp_rs.dll                  <windbg>\winext\
copy windbg_mcp_rs_GalleryManifest.xml                 <windbg>\OptionalExtensions\

# WinDbg (Store) — use install.ps1 (manual setup is complex)
# The script handles config.xml, GUID, and manifest path rewriting automatically.
# If you must do it manually:
#   DLL  → %LOCALAPPDATA%\DBG\EngineExtensions\
#   Gallery files → %LOCALAPPDATA%\DBG\ExtRepository\windbg-mcp-rs\
#   Then: .settings load %LOCALAPPDATA%\DBG\ExtRepository\windbg-mcp-rs\config.xml
#   Then: .settings save
```

### 3. Verify

Start WinDbg and run:

```text
!mcp status
```

The MCP server **auto-starts** when the extension loads.  
Endpoint: `http://127.0.0.1:50051/mcp`

### 4. Connect your MCP client

Point your client to:

```text
http://127.0.0.1:50051/mcp
```

## WinDbg Commands

Use `!mcp help` to list all plugin commands.

Common ones:

```text
!mcp help
!mcp serve 127.0.0.1:50051
!mcp status
!mcp catalog dt
!mcp doc dt
!mcp stop
```

## What MCP Exposes

- `Resources`: a low-context guide resource and compact/full WinDbg command documentation resources
- `Tools`: a compact toolset for catalog search, execution-state query, command execution, and target interrupt

Pure UI shortcut topics remain available as documentation, and command execution is exposed through a single `windbg_execute_command` tool.

Recommended agent flow: call `windbg_search_catalog`, read `windbg://command/{id}`, fall back to `windbg://command-full/{id}` only when needed, call `windbg_get_execution_state`, and then call `windbg_execute_command`.

If the debugger is running or busy, call `windbg_interrupt_target` explicitly and verify state again before executing the command.

## Development

```powershell
cargo check
cargo test
```

## Notes

- This project was written entirely with a Vibe Coding workflow
- The server runs inside the WinDbg process
- The runtime does not parse `docs/debugger.chm`; it uses the prebuilt static catalog in `src/catalog.json`
- The transport is Streamable HTTP
- Set your MCP client timeout as high as possible, because some WinDbg operations can take a long time to finish
- The server now auto-starts when the extension DLL is loaded. Run `!mcp stop` to stop it, or `!mcp serve` to start it again manually.
- `windbg_mcp_rs_GalleryManifest.xml` enables WinDbg auto-loading when placed in `OptionalExtensions\` alongside the extension DLL.
- Use `scripts\install.ps1` to automatically deploy the extension to all discovered WinDbg installations.

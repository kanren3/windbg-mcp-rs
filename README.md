# windbg-mcp-rs

`windbg-mcp-rs` is a pure WinDbg plugin DLL. It turns WinDbg debugger command documentation from `docs/debugger.chm` into a static MCP catalog and serves MCP from inside the WinDbg process.

## What is implemented

- Rust implementation with `windows-rs` for debugger engine integration.
- MCP server implementation with the official Rust SDK crate `rmcp`, hosted inside the WinDbg extension.
- Static catalog extracted from `docs/debugger.chm` into `src/command_catalog.json`.
- MCP resources for the catalog index and every extracted debugger command topic.
- MCP prompts for every extracted debugger command topic.
- MCP tools for raw execution, catalog search, and each extracted command topic.
- WinDbg extension entry point `!mcp` for starting/stopping the MCP server, catalog lookup, and direct command execution.

The current catalog contains the `Commands` and `Meta-Commands` sections from the WinDbg `Debugger Commands` documentation tree.

## Project layout

```text
windbg-mcp-rs/
  docs/
    debugger.chm
  llm_cache/
    ... extracted CHM artifacts used to prepare the static catalog
  src/
    command_catalog.json
    catalog.rs
    executor.rs
    extension.rs
    main.rs
    server.rs
  tests/
  Cargo.toml
  README.md
```

## Build output

Build the plugin DLL:

```powershell
cargo build
```

The WinDbg extension artifact is the generated DLL, for example `target\debug\windbg_mcp_rs.dll`.

Build the optimized release artifact with:

```powershell
cargo build --release
```

The release DLL is written to `target\release\windbg_mcp_rs.dll` and the matching symbols are written to `target\release\windbg_mcp_rs.pdb`.

## Automated release

The repository includes a GitHub Actions workflow at `.github/workflows/release-on-tag.yml`.

- Pushing a tag that matches `v*` builds the project in release mode on `windows-latest`.
- The workflow verifies that the pushed tag matches the version in `Cargo.toml`, for example `v0.1.0`.
- The release publishes `windbg_mcp_rs.dll`, `windbg_mcp_rs.pdb`, a versioned zip archive, and a SHA-256 checksum file.

Typical release flow:

```powershell
# update Cargo.toml version first
git tag v0.1.0
git push origin v0.1.0
```

This keeps GitHub Releases aligned with tagged versions instead of overwriting a rolling prerelease on every push.

## Using the WinDbg extension

After building the DLL, load it in WinDbg and use:

```text
!mcp help
!mcp serve 127.0.0.1:50051
!mcp status
!mcp catalog dt
!mcp doc dt
!mcp exec dt _PEB_LDR_DATA -b
!mcp stop
```

When `!mcp serve` succeeds, the MCP server is available on the reported Streamable HTTP endpoint such as `http://127.0.0.1:50051/mcp`. The server shares the current WinDbg session by creating a new dbgeng client and calling `ConnectSession`.

## Notes

- This is a fully Vibe Coding project.
- The runtime never parses `docs/debugger.chm`; it only uses the prepared static JSON catalog.
- `llm_cache/` is only used as a preparation area for extracted CHM content.
- Command execution is serialized through a dedicated worker that connects back into the active debugger session.
- There is no standalone `.exe` server target anymore; the DLL is the only intended runtime artifact.

## Verification

```powershell
cargo check
cargo test
```

# 项目概述

这个项目会将 windbg 的帮助文件 `docs/debugger.chm` 转换成一个提供 MCP 服务的 windbg 插件。

# 项目要求

- 使用 rust 语言来编写。
- 使用 [windows-rs](https://microsoft.github.io/windows-docs-rs) 来实现 windbg 插件。
- 使用 [rust-sdk](https://github.com/modelcontextprotocol/rust-sdk) 来实现 MCP 服务器。
- 将 `docs/debugger.chm` 中的 **Debugger Commands** 转换成 **[Resources](https://modelcontextprotocol.io/docs/learn/server-concepts#resources)**、**[Tools](https://modelcontextprotocol.io/docs/learn/server-concepts#tools)**、**[Prompts](https://modelcontextprotocol.io/docs/learn/server-concepts#prompts)**。
- 不要猜测 **Debugger Commands** 的功能，要严格按照 `docs/debugger.chm` 来实现。
- 不要动态解析 `docs/debugger.chm`，而是在编写代码之前就准备好，这个文件的用途只是用来告诉 LLM 如何编写代码。
- 不要在工作目录以外创建任何临时文件，如有需要存放在 `llm_cache/` 中。

# 项目结构

```
windbg-mcp-rs/
  llm_cache/
  docs/
  src/
  tests/
  Cargo.toml
  README.md
```

# 大致预期结果

```
输入：_PEB_LDR_DATA 的结构体是什么样子？
MCP Server：调用 dt _PEB_LDR_DATA
输出：ntdll!_PEB_LDR_DATA
   +0x000 Length           : Uint4B
   +0x004 Initialized      : UChar
   +0x008 SsHandle         : Ptr64 Void
   +0x010 InLoadOrderModuleList : _LIST_ENTRY
   +0x020 InMemoryOrderModuleList : _LIST_ENTRY
   +0x030 InInitializationOrderModuleList : _LIST_ENTRY
   +0x040 EntryInProgress  : Ptr64 Void
   +0x048 ShutdownInProgress : UChar
   +0x050 ShutdownThreadId : Ptr64 Void
```

```
输入：_PEB_LDR_DATA 展开后的结构体是什么样子？
MCP Server：调用 dt _PEB_LDR_DATA -b
输出：ntdll!_PEB_LDR_DATA
   +0x000 Length           : Uint4B
   +0x004 Initialized      : UChar
   +0x008 SsHandle         : Ptr64 
   +0x010 InLoadOrderModuleList : _LIST_ENTRY
      +0x000 Flink            : Ptr64 
      +0x008 Blink            : Ptr64 
   +0x020 InMemoryOrderModuleList : _LIST_ENTRY
      +0x000 Flink            : Ptr64 
      +0x008 Blink            : Ptr64 
   +0x030 InInitializationOrderModuleList : _LIST_ENTRY
      +0x000 Flink            : Ptr64 
      +0x008 Blink            : Ptr64 
   +0x040 EntryInProgress  : Ptr64 
   +0x048 ShutdownInProgress : UChar
   +0x050 ShutdownThreadId : Ptr64 

```


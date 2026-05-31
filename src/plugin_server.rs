use std::{
    ffi::CString,
    sync::{LazyLock, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use axum::Router;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use windows::{
    Win32::System::Diagnostics::Debug::Extensions::{
        DEBUG_CONNECT_SESSION_NO_ANNOUNCE, DEBUG_CONNECT_SESSION_NO_VERSION, DEBUG_OUTPUT_NORMAL,
        IDebugControl,
    },
    core::{Interface, PCSTR},
};

use crate::{
    CommandDispatcher, ExecutionMode, WindbgMcpServer, primary_client::create_client_from_primary,
};

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:50051";

static SERVER_STATE: LazyLock<Mutex<Option<RunningPluginServer>>> =
    LazyLock::new(|| Mutex::new(None));
static DISPATCHER_STATE: LazyLock<Mutex<Option<(CommandDispatcher, JoinHandle<()>)>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone)]
pub struct PluginServerStatus {
    pub mcp_url: String,
}

struct RunningPluginServer {
    status: PluginServerStatus,
    cancellation: CancellationToken,
    join_handle: JoinHandle<()>,
}

pub struct PluginServerControl;

impl PluginServerControl {
    pub fn get_or_start_dispatcher() -> Result<CommandDispatcher, String> {
        {
            let state = DISPATCHER_STATE
                .lock()
                .map_err(|_| "dispatcher state lock poisoned".to_string())?;
            if let Some((dispatcher, _)) = state.as_ref() {
                return Ok(dispatcher.clone());
            }
        }

        match CommandDispatcher::spawn(ExecutionMode::CurrentSession) {
            Ok((dispatcher, join_handle)) => {
                let mut state = DISPATCHER_STATE
                    .lock()
                    .map_err(|_| "dispatcher state lock poisoned".to_string())?;
                if let Some((existing, _)) = state.as_ref() {
                    return Ok(existing.clone());
                }
                let cloned = dispatcher.clone();
                *state = Some((dispatcher, join_handle));
                let _ = notify_windbg("WinDbg MCP debugger session connected.\n");
                Ok(cloned)
            }
            Err(error) => {
                let message = error.to_string();
                let _ = notify_windbg(&format!(
                    "WinDbg MCP debugger session connection failed: {message}\n"
                ));
                Err(message)
            }
        }
    }

    pub fn start(bind_address: Option<&str>) -> Result<PluginServerStatus, String> {
        let bind_address = bind_address
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_BIND_ADDRESS)
            .to_string();

        let mut state = SERVER_STATE
            .lock()
            .map_err(|_| "server state lock poisoned".to_string())?;
        if let Some(existing) = state.as_ref() {
            return Ok(existing.status.clone());
        }

        let running = spawn_server_thread(BindTarget::Exact(bind_address), false)?;
        let status = running.status.clone();
        *state = Some(running);
        Ok(status)
    }

    pub fn auto_start() -> Result<PluginServerStatus, String> {
        let mut state = SERVER_STATE
            .lock()
            .map_err(|_| "server state lock poisoned".to_string())?;
        if let Some(existing) = state.as_ref() {
            return Ok(existing.status.clone());
        }

        let running = spawn_server_thread(
            BindTarget::Exact(DEFAULT_BIND_ADDRESS.to_string()),
            true,
        )?;
        let status = running.status.clone();
        *state = Some(running);
        Ok(status)
    }

    pub fn status() -> Result<Option<PluginServerStatus>, String> {
        let state = SERVER_STATE
            .lock()
            .map_err(|_| "server state lock poisoned".to_string())?;
        Ok(state.as_ref().map(|running| running.status.clone()))
    }

    pub fn stop() -> Result<Option<PluginServerStatus>, String> {

        // 1. Drop dispatcher sender and join its thread.
        let dispatcher_join = {
            let mut dispatcher_state = DISPATCHER_STATE
                .lock()
                .map_err(|_| "dispatcher state lock poisoned".to_string())?;
            dispatcher_state.take().map(|(_dispatcher, handle)| handle)
        };

        // 2. Cancel and join the server thread.
        let running = {
            let mut state = SERVER_STATE
                .lock()
                .map_err(|_| "server state lock poisoned".to_string())?;
            state.take()
        };

        let Some(running) = running else {
            if let Some(handle) = dispatcher_join {
                let _ = handle.join();
            }
            return Ok(None);
        };

        running.cancellation.cancel();
        running
            .join_handle
            .join()
            .map_err(|_| "plugin server thread panicked".to_string())?;

        if let Some(handle) = dispatcher_join {
            let _ = handle.join();
        }

        Ok(Some(running.status))
    }
}

enum BindTarget {
    Exact(String),
}

fn spawn_server_thread(
    target: BindTarget,
    fallback_on_busy: bool,
) -> Result<RunningPluginServer, String> {
    let cancellation = CancellationToken::new();
    let cancellation_for_thread = cancellation.clone();
    let (startup_tx, startup_rx) = mpsc::channel::<Result<PluginServerStatus, String>>();

    let initial_addr = match &target {
        BindTarget::Exact(addr) => addr.clone(),
    };


    let join_handle = thread::Builder::new()
        .name("windbg-mcp-plugin-server".to_string())
        .spawn(move || {
            let startup_error_tx = startup_tx.clone();

            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_io()
                .enable_time()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = startup_tx.send(Err(error.to_string()));
                    return;
                }
            };

            let result = runtime.block_on(async move {
                let listener = match TcpListener::bind(&initial_addr).await {
                    Ok(l) => {
                        l
                    }
                    Err(e) if fallback_on_busy && is_addr_in_use(&e.to_string()) => {
                        TcpListener::bind("127.0.0.1:0")
                            .await
                            .map_err(|e2| {
                                e2.to_string()
                            })?
                    }
                    Err(e) => {
                        return Err(e.to_string());
                    }
                };

                let local_addr = listener.local_addr().map_err(|e| e.to_string())?;
                let status = PluginServerStatus {
                    mcp_url: format!("http://{}:{}/mcp", local_addr.ip(), local_addr.port()),
                };

                if startup_tx.send(Ok(status)).is_err() {
                    return Err("plugin server startup receiver dropped".to_string());
                }

                let serve_result = run_server_loop(listener, cancellation_for_thread).await;
                serve_result
            });

            if let Err(error) = &result {
                let _ = startup_error_tx.send(Err(error.clone()));
            } else {
            }
        })
        .map_err(|error| error.to_string())?;

    let status = startup_rx
        .recv()
        .map_err(|_| "plugin server failed to report startup status".to_string())??;

    Ok(RunningPluginServer {
        status,
        cancellation,
        join_handle,
    })
}

fn is_addr_in_use(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("address already in use")
        || lower.contains("address in use")
        || lower.contains("eaddrinuse")
        || lower.contains("10048")
}

async fn run_server_loop(
    listener: TcpListener,
    cancellation: CancellationToken,
) -> Result<(), String> {
    let service: StreamableHttpService<WindbgMcpServer> = StreamableHttpService::new(
        || Ok(WindbgMcpServer::new()),
        Default::default(),
        StreamableHttpServerConfig {
            stateful_mode: true,
            sse_keep_alive: None,
            cancellation_token: cancellation.child_token(),
            ..Default::default()
        },
    );
    let router = Router::new().nest_service("/mcp", service);

    axum::serve(listener, router)
        .with_graceful_shutdown(async move { cancellation.cancelled_owned().await })
        .await
        .map_err(|error| error.to_string())
}

pub(crate) fn notify_windbg(text: &str) -> Result<(), String> {
    let client = create_client_from_primary()?;
    unsafe {
        client
            .ConnectSession(
                DEBUG_CONNECT_SESSION_NO_VERSION | DEBUG_CONNECT_SESSION_NO_ANNOUNCE,
                0,
            )
            .map_err(|error| error.to_string())?;
    }

    let control = client
        .cast::<IDebugControl>()
        .map_err(|error| error.to_string())?;

    for line in text.lines() {
        let mut escaped = line.replace('%', "%%");
        escaped.push('\n');
        let c_text = CString::new(escaped).map_err(|_| "output text contained NUL".to_string())?;
        unsafe {
            control
                .Output(DEBUG_OUTPUT_NORMAL, PCSTR(c_text.as_ptr() as _))
                .map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

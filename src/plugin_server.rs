use std::{
    net::SocketAddr,
    sync::{LazyLock, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use axum::Router;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{CommandDispatcher, ExecutionMode, WindbgMcpServer};

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:50051";

static SERVER_STATE: LazyLock<Mutex<Option<RunningPluginServer>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Debug, Clone)]
pub struct PluginServerStatus {
    pub bind_address: String,
    pub mcp_url: String,
}

struct RunningPluginServer {
    status: PluginServerStatus,
    cancellation: CancellationToken,
    join_handle: JoinHandle<()>,
}

pub struct PluginServerControl;

impl PluginServerControl {
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

        let cancellation = CancellationToken::new();
        let cancellation_for_thread = cancellation.clone();
        let (startup_tx, startup_rx) = mpsc::channel::<Result<PluginServerStatus, String>>();
        let thread_bind = bind_address.clone();

        let join_handle = thread::Builder::new()
            .name("windbg-mcp-plugin-server".to_string())
            .spawn(move || {
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
                    run_server_loop(thread_bind, cancellation_for_thread, startup_tx).await
                });

                if let Err(error) = result {
                    tracing::error!("plugin MCP server stopped with error: {error}");
                }
            })
            .map_err(|error| error.to_string())?;

        let status = startup_rx
            .recv()
            .map_err(|_| "plugin server failed to report startup status".to_string())??;

        *state = Some(RunningPluginServer {
            status: status.clone(),
            cancellation,
            join_handle,
        });

        Ok(status)
    }

    pub fn status() -> Result<Option<PluginServerStatus>, String> {
        let state = SERVER_STATE
            .lock()
            .map_err(|_| "server state lock poisoned".to_string())?;
        Ok(state.as_ref().map(|running| running.status.clone()))
    }

    pub fn stop() -> Result<Option<PluginServerStatus>, String> {
        let running = {
            let mut state = SERVER_STATE
                .lock()
                .map_err(|_| "server state lock poisoned".to_string())?;
            state.take()
        };

        let Some(running) = running else {
            return Ok(None);
        };

        running.cancellation.cancel();
        running
            .join_handle
            .join()
            .map_err(|_| "plugin server thread panicked".to_string())?;
        Ok(Some(running.status))
    }
}

async fn run_server_loop(
    bind_address: String,
    cancellation: CancellationToken,
    startup_tx: mpsc::Sender<Result<PluginServerStatus, String>>,
) -> Result<(), String> {
    let listener = TcpListener::bind(&bind_address)
        .await
        .map_err(|error| error.to_string())?;
    let local_addr = listener.local_addr().map_err(|error| error.to_string())?;
    let bind_address = socket_addr_to_string(local_addr);
    let status = PluginServerStatus {
        mcp_url: format!("http://{bind_address}/mcp"),
        bind_address,
    };
    startup_tx
        .send(Ok(status))
        .map_err(|_| "plugin server startup receiver dropped".to_string())?;

    let service: StreamableHttpService<WindbgMcpServer> = StreamableHttpService::new(
        || {
            let dispatcher = CommandDispatcher::spawn(ExecutionMode::CurrentSession)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            Ok(WindbgMcpServer::new(dispatcher))
        },
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

fn socket_addr_to_string(address: SocketAddr) -> String {
    format!("{}:{}", address.ip(), address.port())
}

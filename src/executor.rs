use std::{collections::HashMap, ffi::CString, path::PathBuf, sync::mpsc, thread};

use tokio::sync::oneshot;

use crate::catalog::CatalogEntry;

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("command topic `{0}` cannot be executed as plain debugger text")]
    NonTextualCommand(String),
    #[error("variant `{variant}` is not documented for `{command}`")]
    InvalidVariant { command: String, variant: String },
    #[error("dispatcher worker stopped")]
    WorkerStopped,
    #[error("debugger session failed to start: {0}")]
    Startup(String),
    #[error("command execution failed: {0}")]
    Command(String),
    #[error("string contains an embedded NUL byte")]
    InvalidCString,
    #[error("this execution mode is only available on Windows")]
    WindowsOnly,
}

pub enum ExecutionMode {
    CurrentSession,
    AttachProcess { pid: u32, noninvasive: bool },
    DumpFile { path: PathBuf },
    Mock { responses: HashMap<String, String> },
}

struct DispatcherRequest {
    command: String,
    response: oneshot::Sender<Result<String, ExecutionError>>,
}

#[derive(Clone)]
pub struct CommandDispatcher {
    sender: tokio::sync::mpsc::UnboundedSender<DispatcherRequest>,
}

impl CommandDispatcher {
    pub fn spawn(mode: ExecutionMode) -> Result<Self, ExecutionError> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<DispatcherRequest>();
        let (ready_tx, ready_rx) = mpsc::channel();

        thread::Builder::new()
            .name("windbg-mcp-dispatcher".to_string())
            .spawn(move || {
                let mut executor = match build_executor(mode) {
                    Ok(executor) => {
                        let _ = ready_tx.send(Ok(()));
                        executor
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };

                while let Some(request) = receiver.blocking_recv() {
                    let result = executor.execute(&request.command);
                    let _ = request.response.send(result);
                }
            })
            .map_err(|error| ExecutionError::Startup(error.to_string()))?;

        ready_rx
            .recv()
            .map_err(|_| ExecutionError::WorkerStopped)??;

        Ok(Self { sender })
    }

    pub async fn execute(&self, command: impl Into<String>) -> Result<String, ExecutionError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(DispatcherRequest {
                command: command.into(),
                response: response_tx,
            })
            .map_err(|_| ExecutionError::WorkerStopped)?;

        response_rx
            .await
            .map_err(|_| ExecutionError::WorkerStopped)?
    }
}

pub fn build_command(
    entry: &CatalogEntry,
    variant: Option<&str>,
    arguments: Option<&str>,
) -> Result<String, ExecutionError> {
    if !entry.supports_text_execution {
        return Err(ExecutionError::NonTextualCommand(entry.title.clone()));
    }

    let selected = match variant.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => entry
            .tokens
            .iter()
            .find(|token| token.eq_ignore_ascii_case(value))
            .map(String::as_str)
            .ok_or_else(|| ExecutionError::InvalidVariant {
                command: entry.title.clone(),
                variant: value.to_string(),
            })?,
        None => entry.primary_token(),
    };

    let trimmed_args = arguments.map(str::trim).filter(|value| !value.is_empty());
    Ok(match trimmed_args {
        Some(arguments) => format!("{selected} {arguments}"),
        None => selected.to_string(),
    })
}

trait BlockingExecutor {
    fn execute(&mut self, command: &str) -> Result<String, ExecutionError>;
}

fn build_executor(mode: ExecutionMode) -> Result<Box<dyn BlockingExecutor>, ExecutionError> {
    match mode {
        ExecutionMode::Mock { responses } => Ok(Box::new(MockExecutor { responses })),
        ExecutionMode::CurrentSession => {
            #[cfg(windows)]
            {
                Ok(Box::new(DbgEngExecutor::connect_session()?))
            }
            #[cfg(not(windows))]
            {
                Err(ExecutionError::WindowsOnly)
            }
        }
        ExecutionMode::AttachProcess { pid, noninvasive } => {
            #[cfg(windows)]
            {
                Ok(Box::new(DbgEngExecutor::attach_process(pid, noninvasive)?))
            }
            #[cfg(not(windows))]
            {
                let _ = (pid, noninvasive);
                Err(ExecutionError::WindowsOnly)
            }
        }
        ExecutionMode::DumpFile { path } => {
            #[cfg(windows)]
            {
                Ok(Box::new(DbgEngExecutor::open_dump_file(path)?))
            }
            #[cfg(not(windows))]
            {
                let _ = path;
                Err(ExecutionError::WindowsOnly)
            }
        }
    }
}

struct MockExecutor {
    responses: HashMap<String, String>,
}

impl BlockingExecutor for MockExecutor {
    fn execute(&mut self, command: &str) -> Result<String, ExecutionError> {
        Ok(self
            .responses
            .get(command)
            .cloned()
            .unwrap_or_else(|| format!("mock-executed: {command}")))
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::sync::{Arc, Mutex};

    use windows::{
        Win32::System::Diagnostics::Debug::Extensions::{
            DEBUG_ATTACH_DEFAULT, DEBUG_ATTACH_NONINVASIVE, DEBUG_EXECUTE_DEFAULT,
            DEBUG_OUTCTL_THIS_CLIENT, DebugCreate, IDebugClient, IDebugControl,
            DEBUG_CONNECT_SESSION_NO_ANNOUNCE, DEBUG_CONNECT_SESSION_NO_VERSION,
            IDebugOutputCallbacks, IDebugOutputCallbacks_Impl,
        },
        core::{Interface, PCSTR, Result as WinResult, implement},
    };

    use super::{BlockingExecutor, CString, ExecutionError, PathBuf};

    #[implement(IDebugOutputCallbacks)]
    struct OutputCollector {
        buffer: Arc<Mutex<String>>,
    }

    impl OutputCollector {
        fn new(buffer: Arc<Mutex<String>>) -> Self {
            Self { buffer }
        }
    }

    impl IDebugOutputCallbacks_Impl for OutputCollector_Impl {
        fn Output(&self, _mask: u32, text: &PCSTR) -> WinResult<()> {
            if !text.is_null() {
                let fragment = unsafe { text.to_string() }.unwrap_or_default();
                self.buffer
                    .lock()
                    .expect("buffer lock poisoned")
                    .push_str(&fragment);
            }
            Ok(())
        }
    }

    pub(crate) struct DbgEngExecutor {
        client: IDebugClient,
    }

    impl DbgEngExecutor {
        pub(crate) fn connect_session() -> Result<Self, ExecutionError> {
            let client = unsafe { DebugCreate::<IDebugClient>() }
                .map_err(|error| ExecutionError::Startup(error.to_string()))?;

            unsafe {
                client
                    .ConnectSession(
                        DEBUG_CONNECT_SESSION_NO_VERSION | DEBUG_CONNECT_SESSION_NO_ANNOUNCE,
                        0,
                    )
                    .map_err(|error| ExecutionError::Startup(error.to_string()))?;
            }

            Ok(Self { client })
        }

        pub(crate) fn open_dump_file(path: PathBuf) -> Result<Self, ExecutionError> {
            let c_path = CString::new(path.to_string_lossy().as_bytes())
                .map_err(|_| ExecutionError::InvalidCString)?;
            let client = unsafe { DebugCreate::<IDebugClient>() }
                .map_err(|error| ExecutionError::Startup(error.to_string()))?;
            let control = client
                .cast::<IDebugControl>()
                .map_err(|error| ExecutionError::Startup(error.to_string()))?;

            unsafe {
                client
                    .OpenDumpFile(PCSTR(c_path.as_ptr() as _))
                    .map_err(|error| ExecutionError::Startup(error.to_string()))?;
                control
                    .WaitForEvent(0, u32::MAX)
                    .map_err(|error| ExecutionError::Startup(error.to_string()))?;
            }

            Ok(Self { client })
        }

        pub(crate) fn attach_process(pid: u32, noninvasive: bool) -> Result<Self, ExecutionError> {
            let client = unsafe { DebugCreate::<IDebugClient>() }
                .map_err(|error| ExecutionError::Startup(error.to_string()))?;
            let control = client
                .cast::<IDebugControl>()
                .map_err(|error| ExecutionError::Startup(error.to_string()))?;
            let flags = if noninvasive {
                DEBUG_ATTACH_NONINVASIVE
            } else {
                DEBUG_ATTACH_DEFAULT
            };

            unsafe {
                client
                    .AttachProcess(0, pid, flags)
                    .map_err(|error| ExecutionError::Startup(error.to_string()))?;
                control
                    .WaitForEvent(0, u32::MAX)
                    .map_err(|error| ExecutionError::Startup(error.to_string()))?;
            }

            Ok(Self { client })
        }

        pub(crate) fn from_existing_client(client: IDebugClient) -> Result<Self, ExecutionError> {
            let _ = client
                .cast::<IDebugControl>()
                .map_err(|error| ExecutionError::Startup(error.to_string()))?;
            Ok(Self { client })
        }

        pub(crate) fn execute_command(&mut self, command: &str) -> Result<String, ExecutionError> {
            <Self as BlockingExecutor>::execute(self, command)
        }
    }

    impl BlockingExecutor for DbgEngExecutor {
        fn execute(&mut self, command: &str) -> Result<String, ExecutionError> {
            let captured = Arc::new(Mutex::new(String::new()));
            let callback: IDebugOutputCallbacks = OutputCollector::new(captured.clone()).into();
            let child = unsafe { self.client.CreateClient() }
                .map_err(|error| ExecutionError::Command(error.to_string()))?;
            let child_control = child
                .cast::<IDebugControl>()
                .map_err(|error| ExecutionError::Command(error.to_string()))?;
            let c_command = CString::new(command).map_err(|_| ExecutionError::InvalidCString)?;

            unsafe {
                child
                    .SetOutputCallbacks(&callback)
                    .map_err(|error| ExecutionError::Command(error.to_string()))?;
                child_control
                    .Execute(
                        DEBUG_OUTCTL_THIS_CLIENT,
                        PCSTR(c_command.as_ptr() as _),
                        DEBUG_EXECUTE_DEFAULT,
                    )
                    .map_err(|error| ExecutionError::Command(error.to_string()))?;
                child
                    .FlushCallbacks()
                    .map_err(|error| ExecutionError::Command(error.to_string()))?;
            }

            Ok(captured.lock().expect("buffer lock poisoned").clone())
        }
    }
}

#[cfg(windows)]
pub(crate) use windows_impl::DbgEngExecutor;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    #[test]
    fn build_command_uses_first_variant_by_default() {
        let catalog = Catalog::global();
        let entry = catalog.lookup("bp").expect("bp entry should exist");
        let rendered =
            build_command(entry, None, Some("ntdll!NtClose")).expect("command should render");
        assert_eq!(rendered, "bp ntdll!NtClose");
    }

    #[test]
    fn build_command_rejects_unknown_variant() {
        let catalog = Catalog::global();
        let entry = catalog.lookup("bp").expect("bp entry should exist");
        let error = build_command(entry, Some("bogus"), None).expect_err("variant must fail");
        assert!(matches!(error, ExecutionError::InvalidVariant { .. }));
    }
}

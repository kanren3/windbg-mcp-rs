use std::{
    collections::HashMap,
    ffi::CString,
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::sync::oneshot;

use crate::catalog::CatalogEntry;

const EXECUTION_STATUS_NO_CHANGE: u32 = 0;
const EXECUTION_STATUS_GO: u32 = 1;
const EXECUTION_STATUS_GO_HANDLED: u32 = 2;
const EXECUTION_STATUS_GO_NOT_HANDLED: u32 = 3;
const EXECUTION_STATUS_STEP_OVER: u32 = 4;
const EXECUTION_STATUS_STEP_INTO: u32 = 5;
const EXECUTION_STATUS_BREAK: u32 = 6;
const EXECUTION_STATUS_NO_DEBUGGEE: u32 = 7;
const EXECUTION_STATUS_STEP_BRANCH: u32 = 8;
const EXECUTION_STATUS_IGNORE_EVENT: u32 = 9;
const EXECUTION_STATUS_RESTART_REQUESTED: u32 = 10;
const EXECUTION_STATUS_REVERSE_GO: u32 = 11;
const EXECUTION_STATUS_REVERSE_STEP_BRANCH: u32 = 12;
const EXECUTION_STATUS_REVERSE_STEP_OVER: u32 = 13;
const EXECUTION_STATUS_REVERSE_STEP_INTO: u32 = 14;
const EXECUTION_STATUS_OUT_OF_SYNC: u32 = 15;
const EXECUTION_STATUS_WAIT_INPUT: u32 = 16;
const EXECUTION_STATUS_TIMEOUT: u32 = 17;

const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const INTERRUPT_WAIT_TIMEOUT: Duration = Duration::from_secs(15);

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebuggerExecutionState {
    pub raw_status: u32,
    pub status_name: String,
    pub running: bool,
    pub busy: bool,
    pub ready_for_commands: bool,
    pub requires_interrupt_before_command: bool,
    pub summary: String,
}

impl DebuggerExecutionState {
    pub fn from_raw(raw_status: u32) -> Self {
        let (status_name, running, busy, summary) = match raw_status {
            EXECUTION_STATUS_NO_CHANGE => (
                "no_change",
                false,
                false,
                "Debugger state is unchanged and commands can be issued.",
            ),
            EXECUTION_STATUS_GO => ("go", true, false, "The target is running."),
            EXECUTION_STATUS_GO_HANDLED => (
                "go_handled",
                true,
                false,
                "The target is running after a handled event.",
            ),
            EXECUTION_STATUS_GO_NOT_HANDLED => (
                "go_not_handled",
                true,
                false,
                "The target is running after an unhandled event.",
            ),
            EXECUTION_STATUS_STEP_OVER => (
                "step_over",
                true,
                false,
                "The target is running while step-over is in progress.",
            ),
            EXECUTION_STATUS_STEP_INTO => (
                "step_into",
                true,
                false,
                "The target is running while step-into is in progress.",
            ),
            EXECUTION_STATUS_BREAK => (
                "break",
                false,
                false,
                "The target is broken in and ready for debugger commands.",
            ),
            EXECUTION_STATUS_NO_DEBUGGEE => (
                "no_debuggee",
                false,
                false,
                "No debuggee is currently active.",
            ),
            EXECUTION_STATUS_STEP_BRANCH => (
                "step_branch",
                true,
                false,
                "The target is running while step-branch is in progress.",
            ),
            EXECUTION_STATUS_IGNORE_EVENT => (
                "ignore_event",
                false,
                true,
                "The debugger is processing an event and is not ready for commands.",
            ),
            EXECUTION_STATUS_RESTART_REQUESTED => (
                "restart_requested",
                false,
                true,
                "The debugger is restarting the target.",
            ),
            EXECUTION_STATUS_REVERSE_GO => (
                "reverse_go",
                true,
                false,
                "The target is running in reverse execution mode.",
            ),
            EXECUTION_STATUS_REVERSE_STEP_BRANCH => (
                "reverse_step_branch",
                true,
                false,
                "The target is reverse-stepping through a branch.",
            ),
            EXECUTION_STATUS_REVERSE_STEP_OVER => (
                "reverse_step_over",
                true,
                false,
                "The target is reverse step-over running.",
            ),
            EXECUTION_STATUS_REVERSE_STEP_INTO => (
                "reverse_step_into",
                true,
                false,
                "The target is reverse step-into running.",
            ),
            EXECUTION_STATUS_OUT_OF_SYNC => (
                "out_of_sync",
                false,
                true,
                "The debugger is out of sync and not ready for commands.",
            ),
            EXECUTION_STATUS_WAIT_INPUT => (
                "wait_input",
                false,
                true,
                "The debugger is waiting for input and is treated as busy.",
            ),
            EXECUTION_STATUS_TIMEOUT => (
                "timeout",
                false,
                true,
                "The debugger reported a timeout and is treated as busy.",
            ),
            _ => (
                "unknown",
                false,
                true,
                "The debugger returned an unknown execution status; interrupt before issuing commands.",
            ),
        };
        let ready_for_commands = !running && !busy;
        Self {
            raw_status,
            status_name: status_name.to_string(),
            running,
            busy,
            ready_for_commands,
            requires_interrupt_before_command: !ready_for_commands,
            summary: summary.to_string(),
        }
    }

    pub fn break_state() -> Self {
        Self::from_raw(EXECUTION_STATUS_BREAK)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandExecutionResult {
    pub command: String,
    pub output: String,
    pub state_before: DebuggerExecutionState,
    pub state_after: DebuggerExecutionState,
}

enum DispatcherRequest {
    Execute {
        command: String,
        response: oneshot::Sender<Result<CommandExecutionResult, ExecutionError>>,
    },
    Interrupt {
        response: oneshot::Sender<Result<DebuggerExecutionState, ExecutionError>>,
    },
    State {
        response: oneshot::Sender<Result<DebuggerExecutionState, ExecutionError>>,
    },
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
                    match request {
                        DispatcherRequest::Execute { command, response } => {
                            let result = executor.execute(&command);
                            let _ = response.send(result);
                        }
                        DispatcherRequest::Interrupt { response } => {
                            let result = executor.interrupt();
                            let _ = response.send(result);
                        }
                        DispatcherRequest::State { response } => {
                            let result = executor.query_state();
                            let _ = response.send(result);
                        }
                    }
                }
            })
            .map_err(|error| ExecutionError::Startup(error.to_string()))?;

        ready_rx
            .recv()
            .map_err(|_| ExecutionError::WorkerStopped)??;

        Ok(Self { sender })
    }

    pub async fn execute(
        &self,
        command: impl Into<String>,
    ) -> Result<CommandExecutionResult, ExecutionError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(DispatcherRequest::Execute {
                command: command.into(),
                response: response_tx,
            })
            .map_err(|_| ExecutionError::WorkerStopped)?;

        response_rx
            .await
            .map_err(|_| ExecutionError::WorkerStopped)?
    }

    pub async fn interrupt(&self) -> Result<DebuggerExecutionState, ExecutionError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(DispatcherRequest::Interrupt {
                response: response_tx,
            })
            .map_err(|_| ExecutionError::WorkerStopped)?;

        response_rx
            .await
            .map_err(|_| ExecutionError::WorkerStopped)?
    }

    pub async fn state(&self) -> Result<DebuggerExecutionState, ExecutionError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.sender
            .send(DispatcherRequest::State {
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
    fn query_state(&mut self) -> Result<DebuggerExecutionState, ExecutionError>;

    fn execute_ready(&mut self, command: &str) -> Result<String, ExecutionError>;

    fn interrupt(&mut self) -> Result<DebuggerExecutionState, ExecutionError> {
        Err(ExecutionError::Command(
            "interrupt is not supported for this execution mode".to_string(),
        ))
    }

    fn execute(&mut self, command: &str) -> Result<CommandExecutionResult, ExecutionError> {
        let state_before = self.query_state()?;
        if state_before.requires_interrupt_before_command {
            return Err(ExecutionError::Command(format!(
                "debugger is not ready for commands (status: {}). {} Query execution state first and call `windbg_interrupt_target` if you need to break in.",
                state_before.status_name, state_before.summary
            )));
        }
        let output = self.execute_ready(command)?;
        let state_after = self.query_state()?;

        Ok(CommandExecutionResult {
            command: command.to_string(),
            output,
            state_before,
            state_after,
        })
    }
}

fn build_executor(mode: ExecutionMode) -> Result<Box<dyn BlockingExecutor>, ExecutionError> {
    match mode {
        ExecutionMode::Mock { responses } => Ok(Box::new(MockExecutor {
            responses,
            state: DebuggerExecutionState::break_state(),
        })),
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
    state: DebuggerExecutionState,
}

impl BlockingExecutor for MockExecutor {
    fn query_state(&mut self) -> Result<DebuggerExecutionState, ExecutionError> {
        Ok(self.state.clone())
    }

    fn execute_ready(&mut self, command: &str) -> Result<String, ExecutionError> {
        Ok(self
            .responses
            .get(command)
            .cloned()
            .unwrap_or_else(|| format!("mock-executed: {command}")))
    }

    fn interrupt(&mut self) -> Result<DebuggerExecutionState, ExecutionError> {
        self.state = DebuggerExecutionState::break_state();
        Ok(self.state.clone())
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::sync::{Arc, Mutex};

    use windows::{
        Win32::System::Diagnostics::Debug::Extensions::{
            DEBUG_ATTACH_DEFAULT, DEBUG_ATTACH_NONINVASIVE, DEBUG_CONNECT_SESSION_NO_ANNOUNCE,
            DEBUG_CONNECT_SESSION_NO_VERSION, DEBUG_EXECUTE_DEFAULT, DEBUG_INTERRUPT_ACTIVE,
            DEBUG_OUTCTL_THIS_CLIENT, DebugCreate, IDebugClient, IDebugControl,
            IDebugOutputCallbacks, IDebugOutputCallbacks_Impl,
        },
        core::{Interface, PCSTR, Result as WinResult, implement},
    };

    use super::{
        BlockingExecutor, CString, DebuggerExecutionState, ExecutionError, INTERRUPT_POLL_INTERVAL,
        INTERRUPT_WAIT_TIMEOUT, Instant, PathBuf,
    };

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

        pub(crate) fn execute_command(
            &mut self,
            command: &str,
        ) -> Result<super::CommandExecutionResult, ExecutionError> {
            <Self as BlockingExecutor>::execute(self, command)
        }

        pub(crate) fn interrupt_target(
            &mut self,
        ) -> Result<DebuggerExecutionState, ExecutionError> {
            <Self as BlockingExecutor>::interrupt(self)
        }

        pub(crate) fn query_execution_state(
            &mut self,
        ) -> Result<DebuggerExecutionState, ExecutionError> {
            <Self as BlockingExecutor>::query_state(self)
        }

        fn control(&self) -> Result<IDebugControl, ExecutionError> {
            self.client
                .cast::<IDebugControl>()
                .map_err(|error| ExecutionError::Command(error.to_string()))
        }

        fn wait_until_ready_for_commands(
            control: &IDebugControl,
        ) -> Result<DebuggerExecutionState, ExecutionError> {
            let deadline = Instant::now() + INTERRUPT_WAIT_TIMEOUT;
            loop {
                let state = current_state(control)?;
                if state.ready_for_commands {
                    return Ok(state);
                }
                if Instant::now() >= deadline {
                    return Err(ExecutionError::Command(format!(
                        "timed out waiting for debugger to become ready; last status was {} ({})",
                        state.status_name, state.raw_status
                    )));
                }

                let timeout = INTERRUPT_POLL_INTERVAL.as_millis() as u32;
                let _ = unsafe { control.WaitForEvent(0, timeout) };
            }
        }
    }

    impl BlockingExecutor for DbgEngExecutor {
        fn query_state(&mut self) -> Result<DebuggerExecutionState, ExecutionError> {
            current_state(&self.control()?)
        }

        fn execute_ready(&mut self, command: &str) -> Result<String, ExecutionError> {
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

        fn interrupt(&mut self) -> Result<DebuggerExecutionState, ExecutionError> {
            let control = self.control()?;
            let state = current_state(&control)?;
            if state.ready_for_commands {
                return Ok(state);
            }

            unsafe {
                control
                    .SetInterrupt(DEBUG_INTERRUPT_ACTIVE)
                    .map_err(|error| ExecutionError::Command(error.to_string()))?;
            }

            Self::wait_until_ready_for_commands(&control)
        }
    }

    fn current_state(control: &IDebugControl) -> Result<DebuggerExecutionState, ExecutionError> {
        let raw_status = unsafe { control.GetExecutionStatus() }
            .map_err(|error| ExecutionError::Command(error.to_string()))?;
        Ok(DebuggerExecutionState::from_raw(raw_status))
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

    #[tokio::test]
    async fn mock_dispatcher_supports_interrupt() {
        let dispatcher = CommandDispatcher::spawn(ExecutionMode::Mock {
            responses: HashMap::new(),
        })
        .expect("mock dispatcher should start");

        let result = dispatcher
            .interrupt()
            .await
            .expect("interrupt should succeed");
        assert!(result.ready_for_commands);
        assert_eq!(result.status_name, "break");
    }

    #[test]
    fn mock_executor_rejects_execution_when_running() {
        let mut executor = MockExecutor {
            responses: HashMap::from([("g".to_string(), "continued execution".to_string())]),
            state: DebuggerExecutionState::from_raw(EXECUTION_STATUS_GO),
        };

        let error = executor.execute("g").expect_err("execute should fail");
        assert!(
            error
                .to_string()
                .contains("debugger is not ready for commands")
        );
    }
}

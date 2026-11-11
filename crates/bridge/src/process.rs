use std::{cell::RefCell, ffi::OsString, fmt, path::Path, process::Stdio, rc::Rc, sync::Mutex, time::Duration};

use compio::{
    io::AsyncWriteExt,
    runtime::{JoinError, JoinHandle},
    time::Elapsed,
};
use flume::Sender;
use process_wrap::std::{ChildWrapper, CommandWrap};
use serde::Serialize;
use snafu::{OptionExt, ResultExt, Snafu};

use crate::{app::Event, config::Config, wire};

#[derive(Debug, Snafu)]
pub(crate) enum Error {
    #[snafu(display("cannot prepare VsRocq pipes: {source}"))]
    PreparePipes { source: std::io::Error },

    #[cfg(unix)]
    #[snafu(display("cannot read VsRocq pipe flags: {source}"))]
    ReadPipeFlags { source: rustix::io::Errno },

    #[cfg(unix)]
    #[snafu(display("cannot set VsRocq pipes to nonblocking mode: {source}"))]
    SetPipeNonblocking { source: rustix::io::Errno },

    #[cfg(windows)]
    #[snafu(display("cannot prepare secure VsRocq pipes: {source}"))]
    PrepareSecurePipes { source: crate::secure::Error },

    #[snafu(display("cannot start {}: {source}", server.to_string_lossy()))]
    StartServer { server: OsString, source: std::io::Error },

    #[cfg(windows)]
    #[snafu(display("cannot connect VsRocq input: {source}"))]
    ConnectInput { source: std::io::Error },

    #[cfg(windows)]
    #[snafu(display("cannot connect VsRocq output: {source}"))]
    ConnectOutput { source: std::io::Error },

    #[snafu(display("cannot write to VsRocq: {source}"))]
    WriteInput { source: std::io::Error },

    #[snafu(display("VsRocq closed its output"))]
    OutputClosed,

    #[snafu(display("cannot read VsRocq output: {source}"))]
    ReadOutput { source: wire::Error },

    #[snafu(display("cannot encode VsRocq input: {source}"))]
    EncodeInput { source: wire::Error },

    #[snafu(display("VsRocq input has stopped"))]
    InputStopped,

    #[snafu(display("VsRocq input has closed"))]
    InputClosed { source: flume::SendError<Vec<u8>> },

    #[snafu(display("cannot stop VsRocq: {source}"))]
    StopServer { source: std::io::Error },

    #[snafu(display("cannot reap VsRocq: {source}"))]
    ReapServer { source: std::io::Error },

    #[snafu(display("VsRocq did not stop before the cleanup deadline"))]
    CleanupDeadline { source: Elapsed },

    #[snafu(display("VsRocq cleanup task failed: {source}"))]
    CleanupTask {
        #[snafu(source(from(JoinError, TaskFailure::from)))]
        source: TaskFailure,
    },
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

// Compio retains a Send, but not Sync, panic payload in JoinError. The mutex
// keeps that complete failure in errors that must be Send and Sync. A source
// reference cannot outlive the mutex guard, so this is the source-chain
// boundary.
#[derive(Debug)]
pub(crate) struct TaskFailure(Mutex<JoinError>);

impl From<JoinError> for TaskFailure {
    fn from(error: JoinError) -> Self {
        Self(Mutex::new(error))
    }
}

impl fmt::Display for TaskFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failure = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        fmt::Display::fmt(&*failure, formatter)
    }
}

impl std::error::Error for TaskFailure {}

const REAP_TIMEOUT: Duration = Duration::from_secs(3);
const REAP_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    Primary,
    Proof,
}

#[derive(Clone, Default)]
pub(crate) struct Reaper {
    tasks: Rc<RefCell<Vec<JoinHandle<Result<()>>>>>,
}

impl Reaper {
    pub(crate) async fn drain(&self) -> Result<()> {
        let mut error = None;
        loop {
            let tasks = std::mem::take(&mut *self.tasks.borrow_mut());
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                let result = task.await.context(CleanupTaskSnafu).and_then(|result| result);
                if let Err(failure) = result {
                    error.get_or_insert(failure);
                }
            }
        }
        error.map_or(Ok(()), Err)
    }
}

pub(crate) struct Server {
    child: Option<Box<dyn ChildWrapper>>,
    input: Option<Sender<Vec<u8>>>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    reaper: Reaper,
}

impl Server {
    pub(crate) fn start(
        config: &Config,
        root: &Path,
        role: Role,
        epoch: u64,
        tx: Sender<Event>,
        reaper: Reaper,
    ) -> Result<Self> {
        let pipes = Pipes::new()?;
        let Pipes {
            mut input,
            output,
            child_input,
            child_output,
        } = pipes;
        let mut command = CommandWrap::with_new(&config.server, |command| {
            command
                .args(&config.args)
                .current_dir(root)
                .stdin(child_input)
                .stdout(child_output)
                .stderr(Stdio::inherit());
        });
        #[cfg(unix)]
        command.wrap(process_wrap::std::ProcessGroup::leader());
        #[cfg(windows)]
        {
            use process_wrap::std::{CreationFlags, JobObject};

            command
                .wrap(CreationFlags(windows::Win32::System::Threading::CREATE_NO_WINDOW))
                .wrap(JobObject);
        }
        let child = command.spawn().with_context(|_| StartServerSnafu {
            server: config.server.clone(),
        })?;
        drop(command);
        let (frames, pending) = flume::unbounded::<Vec<u8>>();
        let mut server = Self {
            child: Some(child),
            input: Some(frames),
            writer: None,
            reader: None,
            reaper,
        };
        let writer_tx = tx.clone();
        server.writer = Some(compio::runtime::spawn(async move {
            #[cfg(windows)]
            if let Err(error) = input.connect().await {
                let _ = writer_tx.send(Event::ServerClosed {
                    role,
                    epoch,
                    error: Error::ConnectInput { source: error }.into(),
                });
                return;
            }
            while let Ok(frame) = pending.recv_async().await {
                if let Err(error) = input.write_all(frame).await.0 {
                    let _ = writer_tx.send(Event::ServerClosed {
                        role,
                        epoch,
                        error: Error::WriteInput { source: error }.into(),
                    });
                    break;
                }
            }
        }));
        server.reader = Some(compio::runtime::spawn(async move {
            #[cfg(windows)]
            if let Err(error) = output.connect().await {
                let _ = tx.send(Event::ServerClosed {
                    role,
                    epoch,
                    error: Error::ConnectOutput { source: error }.into(),
                });
                return;
            }
            let mut output = wire::Reader::new(output);
            let error = loop {
                match output.read().await {
                    Ok(Some(message)) => {
                        if tx.send(Event::Server { role, epoch, message }).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break Error::OutputClosed,
                    Err(error) => break Error::ReadOutput { source: error },
                }
            };
            let _ = tx.send(Event::ServerClosed {
                role,
                epoch,
                error: error.into(),
            });
        }));
        Ok(server)
    }

    pub(crate) fn send<T: Serialize + ?Sized>(&mut self, message: &T) -> Result<()> {
        let frame = wire::encode(message).context(EncodeInputSnafu)?;
        self.input
            .as_ref()
            .context(InputStoppedSnafu)?
            .send(frame)
            .context(InputClosedSnafu)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.input.take();
        let Some(mut child) = self.child.take() else { return };
        let killed = match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            _ => child.start_kill().context(StopServerSnafu),
        };
        let writer = self.writer.take();
        let reader = self.reader.take();
        let task = compio::runtime::spawn(async move {
            let reaped = compio::time::timeout(REAP_TIMEOUT, async {
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => return Ok(()),
                        Ok(None) => compio::time::sleep(REAP_INTERVAL).await,
                        Err(error) => return Err(error).context(ReapServerSnafu),
                    }
                }
            })
            .await
            .context(CleanupDeadlineSnafu)
            .and_then(|result| result);
            if let Some(writer) = writer {
                let _ = writer.cancel().await;
            }
            if let Some(reader) = reader {
                let _ = reader.cancel().await;
            }
            killed.and(reaped)
        });
        self.reaper.tasks.borrow_mut().push(task);
    }
}

#[cfg(unix)]
type Pipe = compio::runtime::fd::AsyncFd<std::os::fd::OwnedFd>;
#[cfg(windows)]
type Pipe = compio::fs::named_pipe::NamedPipeServer;

struct Pipes {
    input: Pipe,
    output: Pipe,
    child_input: Stdio,
    child_output: Stdio,
}

#[cfg(unix)]
impl Pipes {
    fn new() -> Result<Self> {
        use std::os::fd::OwnedFd;

        use compio::runtime::{Runtime, fd::AsyncFd};
        use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

        let (child_input, input) = std::io::pipe().context(PreparePipesSnafu)?;
        let (output, child_output) = std::io::pipe().context(PreparePipesSnafu)?;
        let input = OwnedFd::from(input);
        let output = OwnedFd::from(output);
        if Runtime::with_current(|runtime| runtime.driver_type()).is_polling() {
            for pipe in [&input, &output] {
                let flags = fcntl_getfl(pipe).context(ReadPipeFlagsSnafu)?;
                fcntl_setfl(pipe, flags | OFlags::NONBLOCK).context(SetPipeNonblockingSnafu)?;
            }
        }
        Ok(Self {
            input: AsyncFd::new(input).context(PreparePipesSnafu)?,
            output: AsyncFd::new(output).context(PreparePipesSnafu)?,
            child_input: Stdio::from(child_input),
            child_output: Stdio::from(child_output),
        })
    }
}

#[cfg(windows)]
impl Pipes {
    fn new() -> Result<Self> {
        use std::fs::OpenOptions;

        let token = crate::secure::token().context(PrepareSecurePipesSnafu)?;
        let name = format!(r"\\.\pipe\vsrocq-{}-{token}", std::process::id());
        let input_name = format!("{name}-input");
        let output_name = format!("{name}-output");
        let input = crate::secure::pipe(&input_name, false).context(PrepareSecurePipesSnafu)?;
        let output = crate::secure::pipe(&output_name, true).context(PrepareSecurePipesSnafu)?;
        let child_input = OpenOptions::new()
            .read(true)
            .open(input_name)
            .context(PreparePipesSnafu)?;
        let child_output = OpenOptions::new()
            .write(true)
            .open(output_name)
            .context(PreparePipesSnafu)?;
        Ok(Self {
            input,
            output,
            child_input: Stdio::from(child_input),
            child_output: Stdio::from(child_output),
        })
    }
}

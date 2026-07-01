#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCapture {
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_artifact_path: Option<PathBuf>,
    pub stderr_artifact_path: Option<PathBuf>,
}

impl ProcessCapture {
    pub fn from_memory(
        exit_code: Option<i32>,
        duration_ms: u128,
        stdout: String,
        stderr: String,
    ) -> Self {
        Self {
            stdout_bytes: stdout.len().try_into().unwrap_or(u64::MAX),
            stderr_bytes: stderr.len().try_into().unwrap_or(u64::MAX),
            exit_code,
            duration_ms,
            stdout,
            stderr,
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_artifact_path: None,
            stderr_artifact_path: None,
        }
    }

    fn to_markdown(&self, title: &str, argv: &[String]) -> String {
        format!(
            "# {title}\n\ncommand: `{}`\nexit_code: {}\nduration_ms: {}\n\n## stdout\n\nbytes: {}\npreview_truncated: {}\n{}\n\n```text\n{}\n```\n\n## stderr\n\nbytes: {}\npreview_truncated: {}\n{}\n\n```text\n{}\n```\n",
            if argv.is_empty() { "dry-run".to_owned() } else { render_command(argv) },
            self.exit_code
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            self.duration_ms,
            self.stdout_bytes,
            self.stdout_truncated,
            output_artifact_line(self.stdout_artifact_path.as_deref()),
            self.stdout,
            self.stderr_bytes,
            self.stderr_truncated,
            output_artifact_line(self.stderr_artifact_path.as_deref()),
            self.stderr
        )
    }
}

fn default_agentctl_argv() -> Vec<String> {
    vec![
        "agentctl".to_owned(),
        "run".to_owned(),
        std::env::var("LDGR_AGENTCTL_TASK").unwrap_or_else(|_| "ldgr-loop".to_owned()),
    ]
}

fn agent_output_argv(agent: &LoopAgent) -> Vec<String> {
    match agent {
        LoopAgent::Argv(argv) => argv.clone(),
        LoopAgent::Agentctl => default_agentctl_argv(),
        LoopAgent::DryRun => Vec::new(),
    }
}

fn enrich_agentctl_failure_output(mut capture: ProcessCapture) -> ProcessCapture {
    if capture.exit_code == Some(0) {
        return capture;
    }
    let task = std::env::var("LDGR_AGENTCTL_TASK").unwrap_or_else(|_| "ldgr-loop".to_owned());
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return capture;
    };
    let Some(log_path) = latest_agentctl_log_path(&home, &task) else {
        return capture;
    };
    let Ok(log) = fs::read_to_string(&log_path) else {
        return capture;
    };
    let excerpt = tail_text(&log, 12_000);
    capture.stderr.push_str(&format!(
        "\n\n--- latest agentctl raw log ({}) ---\n{}\n--- end latest agentctl raw log ---\n",
        log_path.display(),
        excerpt.trim_end()
    ));
    capture.stderr_bytes = capture.stderr.len().try_into().unwrap_or(u64::MAX);
    capture
}

fn latest_agentctl_log_path(home: &Path, task: &str) -> Option<PathBuf> {
    let jobs = home.join(".agentctl/jobs");
    let mut candidates = fs::read_dir(jobs)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.contains(task) {
                return None;
            }
            let path = entry.path().join("output.log");
            path.is_file().then_some((name, path))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates.pop().map(|(_, path)| path)
}

fn tail_text(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut start = value.len() - max_bytes;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

#[derive(Clone, Copy)]
enum StreamTarget {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessOutputPaths {
    stdin: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedProcessStream {
    preview: Vec<u8>,
    total_bytes: u64,
    artifact_path: PathBuf,
}

trait ReadSend: Read + Send {}
impl<T: Read + Send> ReadSend for T {}

fn output_artifact_line(path: Option<&Path>) -> String {
    match path {
        Some(path) => format!("full_output: `{}`", path.display()),
        None => "full_output: inline".to_owned(),
    }
}

fn process_output_paths(
    artifact_root: &Path,
    run_id: i64,
    label: &str,
) -> anyhow::Result<ProcessOutputPaths> {
    fs::create_dir_all(artifact_root).with_context(|| {
        format!(
            "failed to create artifact root directory {}",
            artifact_root.display()
        )
    })?;
    let label = sanitize_output_label(label);
    Ok(ProcessOutputPaths {
        stdin: artifact_root.join(format!("loop-run-{run_id}-{label}-stdin.txt")),
        stdout: artifact_root.join(format!("loop-run-{run_id}-{label}-stdout.txt")),
        stderr: artifact_root.join(format!("loop-run-{run_id}-{label}-stderr.txt")),
    })
}

fn sanitize_output_label(value: &str) -> String {
    let label = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if label.is_empty() {
        "process".to_owned()
    } else {
        label
    }
}

struct ProcessStreamReader {
    stream_name: &'static str,
    artifact_path: PathBuf,
    receiver: mpsc::Receiver<anyhow::Result<CapturedProcessStream>>,
}

fn read_process_stream(
    stream: Box<dyn ReadSend>,
    stream_target: Option<StreamTarget>,
    stream_name: &'static str,
    artifact_path: PathBuf,
) -> ProcessStreamReader {
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader_artifact_path = artifact_path.clone();
    thread::spawn(move || {
        let result = capture_process_stream(stream, stream_target, reader_artifact_path);
        let _ = sender.send(result);
    });
    ProcessStreamReader {
        stream_name,
        artifact_path,
        receiver,
    }
}

fn capture_process_stream(
    mut stream: Box<dyn ReadSend>,
    stream_target: Option<StreamTarget>,
    artifact_path: PathBuf,
) -> anyhow::Result<CapturedProcessStream> {
    let mut artifact = fs::File::create(&artifact_path)
        .with_context(|| format!("failed to create {}", artifact_path.display()))?;
    let mut preview = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut buffer = [0_u8; 8192];
    loop {
        let bytes_read = stream.read(&mut buffer).with_context(|| {
            format!(
                "failed to read process output for {}",
                artifact_path.display()
            )
        })?;
        if bytes_read == 0 {
            break;
        }
        let chunk = &buffer[..bytes_read];
        artifact
            .write_all(chunk)
            .with_context(|| format!("failed to write {}", artifact_path.display()))?;
        if preview.len() < LOOP_PROCESS_OUTPUT_PREVIEW_BYTES {
            let remaining = LOOP_PROCESS_OUTPUT_PREVIEW_BYTES - preview.len();
            preview.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        total_bytes = total_bytes.saturating_add(bytes_read.try_into().unwrap_or(u64::MAX));
        match stream_target {
            Some(StreamTarget::Stdout) => {
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(chunk)?;
                stdout.flush()?;
            }
            Some(StreamTarget::Stderr) => {
                let mut stderr = std::io::stderr().lock();
                stderr.write_all(chunk)?;
                stderr.flush()?;
            }
            None => {}
        }
    }
    artifact
        .flush()
        .with_context(|| format!("failed to flush {}", artifact_path.display()))?;
    Ok(CapturedProcessStream {
        preview,
        total_bytes,
        artifact_path,
    })
}

fn receive_process_stream_with_timeout(
    reader: &ProcessStreamReader,
    command: &str,
    timeout: Duration,
) -> anyhow::Result<Option<CapturedProcessStream>> {
    match reader.receiver.recv_timeout(timeout) {
        Ok(result) => result
            .with_context(|| format!("failed to read {} for `{command}`", reader.stream_name))
            .map(Some),
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!(
                "{} reader stopped before returning output for `{command}`",
                reader.stream_name
            )
        }
    }
}

fn run_process_with_stdin(
    argv: &[String],
    stdin_text: &str,
    stream_output: bool,
    output_paths: ProcessOutputPaths,
    timeout: Duration,
) -> anyhow::Result<ProcessCapture> {
    run_process_with_stdin_timeout(argv, stdin_text, stream_output, output_paths, timeout)
}

fn run_process_with_stdin_timeout(
    argv: &[String],
    stdin_text: &str,
    stream_output: bool,
    output_paths: ProcessOutputPaths,
    timeout: Duration,
) -> anyhow::Result<ProcessCapture> {
    run_process_with_stdin_timeouts(
        argv,
        stdin_text,
        stream_output,
        output_paths,
        timeout,
        LOOP_PROCESS_PIPE_DRAIN_TIMEOUT,
        LOOP_PROCESS_PIPE_DRAIN_TIMEOUT,
    )
}

fn run_process_with_stdin_timeouts(
    argv: &[String],
    stdin_text: &str,
    stream_output: bool,
    output_paths: ProcessOutputPaths,
    process_timeout: Duration,
    pipe_drain_timeout: Duration,
    kill_drain_timeout: Duration,
) -> anyhow::Result<ProcessCapture> {
    if argv.is_empty() {
        bail!("process argv must not be empty");
    }
    let started = Instant::now();
    fs::write(&output_paths.stdin, stdin_text).with_context(|| {
        format!(
            "failed to write child stdin file {}",
            output_paths.stdin.display()
        )
    })?;
    let stdin_file = fs::File::open(&output_paths.stdin).with_context(|| {
        format!(
            "failed to open child stdin file {}",
            output_paths.stdin.display()
        )
    })?;

    let command_text = render_command(argv);
    let prepared_process_tree = PreparedProcessTree::new()?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::from(stdin_file))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn `{command_text}`"))?;
    let process_tree = match prepared_process_tree.attach(&child) {
        Ok(process_tree) => process_tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let stdout = child.stdout.take().context("failed to open child stdout")?;
    let stderr = child.stderr.take().context("failed to open child stderr")?;
    let stdout_reader = read_process_stream(
        Box::new(stdout),
        stream_output.then_some(StreamTarget::Stdout),
        "stdout",
        output_paths.stdout.clone(),
    );
    let stderr_reader = read_process_stream(
        Box::new(stderr),
        stream_output.then_some(StreamTarget::Stderr),
        "stderr",
        output_paths.stderr.clone(),
    );
    let status_result =
        wait_child_with_timeout(&mut child, &process_tree, process_timeout, &command_text);
    let (stdout, stderr) = collect_process_streams(
        &stdout_reader,
        &stderr_reader,
        &process_tree,
        &command_text,
        pipe_drain_timeout,
        kill_drain_timeout,
    )?;
    let status = match status_result {
        Ok(status) => status,
        Err(error) => {
            bail!(
                "{error}; stdout captured at {}; stderr captured at {}",
                stdout.artifact_path.display(),
                stderr.artifact_path.display()
            );
        }
    };
    Ok(ProcessCapture {
        exit_code: status.code(),
        duration_ms: started.elapsed().as_millis(),
        stdout: String::from_utf8_lossy(&stdout.preview).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.preview).into_owned(),
        stdout_truncated: stdout.total_bytes > stdout.preview.len().try_into().unwrap_or(u64::MAX),
        stderr_truncated: stderr.total_bytes > stderr.preview.len().try_into().unwrap_or(u64::MAX),
        stdout_bytes: stdout.total_bytes,
        stderr_bytes: stderr.total_bytes,
        stdout_artifact_path: Some(stdout.artifact_path),
        stderr_artifact_path: Some(stderr.artifact_path),
    })
}

fn collect_process_streams(
    stdout_reader: &ProcessStreamReader,
    stderr_reader: &ProcessStreamReader,
    process_tree: &ProcessTree,
    command: &str,
    pipe_drain_timeout: Duration,
    kill_drain_timeout: Duration,
) -> anyhow::Result<(CapturedProcessStream, CapturedProcessStream)> {
    let mut stdout =
        receive_process_stream_with_timeout(stdout_reader, command, pipe_drain_timeout)?;
    let mut stderr =
        receive_process_stream_with_timeout(stderr_reader, command, pipe_drain_timeout)?;
    if stdout.is_none() || stderr.is_none() {
        process_tree.terminate();
    }
    if stdout.is_none() {
        stdout = receive_process_stream_with_timeout(stdout_reader, command, kill_drain_timeout)?;
    }
    if stderr.is_none() {
        stderr = receive_process_stream_with_timeout(stderr_reader, command, kill_drain_timeout)?;
    }
    let stdout = stdout.with_context(|| {
        format!(
            "stdout reader did not finish for `{command}` after process group termination; stdout captured at {}",
            stdout_reader.artifact_path.display()
        )
    })?;
    let stderr = stderr.with_context(|| {
        format!(
            "stderr reader did not finish for `{command}` after process group termination; stderr captured at {}",
            stderr_reader.artifact_path.display()
        )
    })?;
    Ok((stdout, stderr))
}

fn wait_child_with_timeout(
    child: &mut Child,
    process_tree: &ProcessTree,
    timeout: Duration,
    command: &str,
) -> anyhow::Result<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("failed to wait for `{command}`"))?
        {
            return Ok(status);
        }
        if !timeout.is_zero() && started.elapsed() >= timeout {
            terminate_child_process_tree(child, process_tree);
            bail!(
                "process `{command}` timed out after {} seconds",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn terminate_child_process_tree(child: &mut Child, process_tree: &ProcessTree) {
    process_tree.terminate();
    let started = Instant::now();
    while started.elapsed() < LOOP_PROCESS_TERMINATION_GRACE {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

struct PreparedProcessTree {
    #[cfg(windows)]
    job: WindowsJob,
}

impl PreparedProcessTree {
    fn new() -> anyhow::Result<Self> {
        #[cfg(windows)]
        {
            return Ok(Self {
                job: WindowsJob::new()?,
            });
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    fn attach(self, child: &Child) -> anyhow::Result<ProcessTree> {
        #[cfg(windows)]
        {
            self.job.assign(child)?;
            return Ok(ProcessTree { job: self.job });
        }
        #[cfg(not(windows))]
        {
            Ok(ProcessTree {
                child_id: child.id(),
            })
        }
    }
}

struct ProcessTree {
    #[cfg(not(windows))]
    child_id: u32,
    #[cfg(windows)]
    job: WindowsJob,
}

impl ProcessTree {
    fn terminate(&self) {
        #[cfg(windows)]
        {
            self.job.terminate();
        }
        #[cfg(not(windows))]
        {
            signal_process_group(self.child_id, TerminationSignal::Terminate);
            thread::sleep(LOOP_PROCESS_TERMINATION_GRACE);
            signal_process_group(self.child_id, TerminationSignal::Kill);
        }
    }
}

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy)]
enum TerminationSignal {
    Terminate,
    Kill,
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(all(not(unix), not(windows)))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(windows)]
struct WindowsJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl WindowsJob {
    fn new() -> anyhow::Result<Self> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            bail!(
                "failed to create Windows job object for loop subprocess tree: {}",
                std::io::Error::last_os_error()
            );
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
            bail!("failed to configure Windows job object: {error}");
        }
        Ok(Self { handle })
    }

    fn assign(&self, child: &Child) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let process = child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let ok = unsafe { AssignProcessToJobObject(self.handle, process) };
        if ok == 0 {
            bail!(
                "failed to assign loop subprocess to Windows job object: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }

    fn terminate(&self) {
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;

    #[test]
    fn default_agentctl_argv_matches_current_agentctl_cli() {
        assert_eq!(
            default_agentctl_argv(),
            vec![
                "agentctl".to_string(),
                "run".to_string(),
                "ldgr-loop".to_string()
            ]
        );
    }

    #[test]
    fn latest_agentctl_log_path_prefers_matching_newest_job() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let jobs = home.path().join(".agentctl/jobs");
        fs::create_dir_all(jobs.join("100-other-iteration-1"))?;
        fs::write(jobs.join("100-other-iteration-1/output.log"), "wrong")?;
        fs::create_dir_all(jobs.join("101-ldgr-loop-iteration-1"))?;
        fs::write(jobs.join("101-ldgr-loop-iteration-1/output.log"), "old")?;
        fs::create_dir_all(jobs.join("102-ldgr-loop-iteration-1"))?;
        fs::write(jobs.join("102-ldgr-loop-iteration-1/output.log"), "new")?;

        let path = latest_agentctl_log_path(home.path(), "ldgr-loop").expect("matching log");
        assert!(path.ends_with("102-ldgr-loop-iteration-1/output.log"));
        Ok(())
    }

    #[test]
    fn tail_text_preserves_utf8_boundary() {
        assert_eq!(tail_text("abc😀def", 6), "def");
        assert_eq!(tail_text("abc😀def", 7), "😀def");
    }
}

#[cfg(unix)]
fn signal_process_group(child_id: u32, signal: TerminationSignal) {
    let signal = match signal {
        TerminationSignal::Terminate => libc::SIGTERM,
        TerminationSignal::Kill => libc::SIGKILL,
    };
    let process_group = -(child_id as libc::pid_t);
    // SAFETY: libc::kill does not retain pointers. The negative PID targets the
    // process group created for this child; errors are intentionally ignored
    // because the process group may already have exited.
    unsafe {
        libc::kill(process_group, signal);
    }
}

#[cfg(all(not(unix), not(windows)))]
fn signal_process_group(_child_id: u32, _signal: TerminationSignal) {}


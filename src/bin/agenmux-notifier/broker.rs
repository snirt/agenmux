use super::NotificationRequest;
use std::fs::{self, File, OpenOptions};
#[cfg(any(target_os = "macos", test))]
use std::future::Future;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", test))]
use std::pin::Pin;
use std::process::{Command, Stdio};
#[cfg(any(target_os = "macos", test))]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use std::task::Context;
#[cfg(any(target_os = "macos", test))]
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

const MAX_FIELD_BYTES: usize = 64 * 1024;
const NONE_LENGTH: u32 = u32::MAX;
const ACK_ACCEPTED: u8 = 1;
const ACK_REJECTED: u8 = 0;
const SOCKET_NAME: &str = "agenmux-notifier.sock";
const LOCK_NAME: &str = "agenmux-notifier.lock";
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(2);
const SUBMIT_RETRY_INTERVAL: Duration = Duration::from_millis(50);
const SUBMIT_RETRY_WINDOW: Duration = Duration::from_secs(5);
const BROKER_RESPAWN_INTERVAL: Duration = Duration::from_secs(1);

#[cfg(any(target_os = "macos", test))]
type PendingNotification = Pin<Box<dyn Future<Output = PendingResult>>>;

#[cfg(target_os = "macos")]
struct PendingResult {
    click: Option<String>,
    response: Result<mac_usernotifications::NotificationResponse, mac_usernotifications::Error>,
}

#[cfg(all(test, not(target_os = "macos")))]
struct PendingResult;

#[cfg(any(target_os = "macos", test))]
struct AuthorizationRequest {
    state: Arc<Mutex<AuthorizationRequestState>>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct AuthorizationRequestState {
    result: Option<bool>,
    waker: Option<std::task::Waker>,
}

#[cfg(any(target_os = "macos", test))]
impl AuthorizationRequest {
    fn spawn(request: impl FnOnce() -> bool + Send + 'static) -> io::Result<Self> {
        let state = Arc::new(Mutex::new(AuthorizationRequestState::default()));
        let worker_state = Arc::clone(&state);
        thread::Builder::new()
            .name("agenmux-authorization".into())
            .spawn(move || {
                let result = request();
                let mut state = worker_state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                state.result = Some(result);
                if let Some(waker) = state.waker.take() {
                    waker.wake();
                }
            })?;
        Ok(Self { state })
    }
}

#[cfg(any(target_os = "macos", test))]
impl Future for AuthorizationRequest {
    type Output = bool;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(result) = state.result.take() {
            Poll::Ready(result)
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

#[cfg(any(target_os = "macos", test))]
struct ClickCommand {
    state: Arc<Mutex<ClickCommandState>>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct ClickCommandState {
    result: Option<io::Result<std::process::ExitStatus>>,
    waker: Option<std::task::Waker>,
}

#[cfg(any(target_os = "macos", test))]
impl ClickCommand {
    fn spawn(command: String) -> io::Result<Self> {
        let state = Arc::new(Mutex::new(ClickCommandState::default()));
        let worker_state = Arc::clone(&state);
        thread::Builder::new()
            .name("agenmux-click".into())
            .spawn(move || {
                let result = Command::new("/bin/sh")
                    .args(["-c", &command])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                let mut state = worker_state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                state.result = Some(result);
                if let Some(waker) = state.waker.take() {
                    waker.wake();
                }
            })?;
        Ok(Self { state })
    }
}

#[cfg(any(target_os = "macos", test))]
impl Future for ClickCommand {
    type Output = io::Result<std::process::ExitStatus>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(result) = state.result.take() {
            std::task::Poll::Ready(result)
        } else {
            state.waker = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct BrokerState {
    accepted_any: bool,
    queued: Vec<NotificationRequest>,
    pending: Vec<PendingNotification>,
    authorization: Option<bool>,
    authorization_request: Option<AuthorizationRequest>,
    clicks: Vec<ClickCommand>,
}

#[cfg(any(target_os = "macos", test))]
impl BrokerState {
    fn should_exit(&self) -> bool {
        self.accepted_any
            && self.queued.is_empty()
            && self.pending.is_empty()
            && self.authorization_request.is_none()
            && self.clicks.is_empty()
    }

    fn accept_request(&mut self, mut stream: UnixStream) {
        let timeout = Some(Duration::from_millis(250));
        if stream.set_nonblocking(false).is_err()
            || stream.set_read_timeout(timeout).is_err()
            || stream.set_write_timeout(timeout).is_err()
        {
            let _ = write_ack(&mut stream, false);
            return;
        }

        let request = match read_request(&mut stream) {
            Ok(request) => request,
            Err(_) => {
                let _ = write_ack(&mut stream, false);
                return;
            }
        };

        self.queued.push(request);
        if write_ack(&mut stream, true).is_err() {
            drop(self.queued.pop());
            return;
        }
        self.accepted_any = true;
        if self.authorization == Some(false) {
            drop(self.queued.pop());
        }
    }

    fn start_authorization(
        &mut self,
        request: impl FnOnce() -> bool + Send + 'static,
    ) -> io::Result<()> {
        self.authorization_request = Some(AuthorizationRequest::spawn(request)?);
        Ok(())
    }

    fn poll_authorization(&mut self, cx: &mut std::task::Context<'_>) -> Poll<bool> {
        Pin::new(
            self.authorization_request
                .as_mut()
                .expect("authorization must be pending"),
        )
        .poll(cx)
    }

    fn complete_authorization(&mut self, authorized: bool) {
        self.authorization_request = None;
        self.authorization = Some(authorized);
        if !authorized {
            self.queued.clear();
        }
    }

    fn take_authorized_requests(&mut self) -> Vec<NotificationRequest> {
        if self.authorization == Some(true) {
            std::mem::take(&mut self.queued)
        } else {
            Vec::new()
        }
    }

    fn poll_clicks(&mut self, cx: &mut std::task::Context<'_>) -> bool {
        let mut failed = false;
        let mut index = 0;
        while index < self.clicks.len() {
            match Pin::new(&mut self.clicks[index]).poll(cx) {
                std::task::Poll::Pending => index += 1,
                std::task::Poll::Ready(result) => {
                    drop(self.clicks.swap_remove(index));
                    failed |= !matches!(result, Ok(status) if status.success());
                }
            }
        }
        failed
    }
}

#[cfg(target_os = "macos")]
struct BrokerFuture {
    listener: UnixListener,
    _owner: BrokerOwner,
    state: BrokerState,
    exit_code: i32,
}

#[cfg(target_os = "macos")]
impl BrokerFuture {
    fn accept_queued(&mut self) -> io::Result<()> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => self.state.accept_request(stream),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            }
        }
    }

    fn start_authorization(&mut self) {
        if !self.state.accepted_any
            || self.state.authorization.is_some()
            || self.state.authorization_request.is_some()
        {
            return;
        }

        if self
            .state
            .start_authorization(|| {
                matches!(mac_usernotifications::blocking::request_auth(), Ok(true))
            })
            .is_err()
        {
            self.state.complete_authorization(false);
            self.exit_code = self.exit_code.max(4);
        }
    }

    fn poll_authorization(&mut self, cx: &mut Context<'_>) {
        if self.state.authorization_request.is_some() {
            if let Poll::Ready(authorized) = self.state.poll_authorization(cx) {
                self.state.complete_authorization(authorized);
                if !authorized {
                    self.exit_code = self.exit_code.max(4);
                }
            }
        }

        let requests = self.state.take_authorized_requests();
        self.state.pending.extend(requests.into_iter().map(pending));
    }

    fn poll_pending(&mut self, cx: &mut Context<'_>) {
        let mut index = 0;
        while index < self.state.pending.len() {
            match self.state.pending[index].as_mut().poll(cx) {
                Poll::Pending => index += 1,
                Poll::Ready(result) => {
                    drop(self.state.pending.swap_remove(index));
                    self.complete(result);
                }
            }
        }
    }

    fn complete(&mut self, result: PendingResult) {
        match result.response {
            Ok(response) if response.is_default_action() => {
                if let Some(click) = result.click {
                    match ClickCommand::spawn(click) {
                        Ok(command) => self.state.clicks.push(command),
                        Err(_) => self.exit_code = self.exit_code.max(6),
                    }
                }
            }
            Ok(_) => {}
            Err(_) => self.exit_code = self.exit_code.max(5),
        }
    }
}

#[cfg(target_os = "macos")]
impl Future for BrokerFuture {
    type Output = i32;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.accept_queued().is_err() {
            self.exit_code = self.exit_code.max(1);
            return Poll::Ready(self.exit_code);
        }
        self.start_authorization();
        self.poll_authorization(cx);

        self.poll_pending(cx);
        if self.state.poll_clicks(cx) {
            self.exit_code = self.exit_code.max(6);
        }
        if self.state.should_exit() {
            Poll::Ready(self.exit_code)
        } else {
            Poll::Pending
        }
    }
}

#[cfg(target_os = "macos")]
fn pending(request: NotificationRequest) -> PendingNotification {
    use mac_usernotifications as noti;

    Box::pin(async move {
        let notification = noti::Notification::new()
            .title(&request.title)
            .message(&request.body)
            .sound(noti::sound::GLASS);
        let response = match notification.send().await {
            Ok(handle) => handle.response().await,
            Err(error) => Err(error),
        };
        PendingResult {
            click: request.click,
            response,
        }
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn serve() -> i32 {
    use mac_usernotifications as noti;

    if noti::check_bundle().is_err() {
        return 3;
    }
    let owner = match BrokerOwner::acquire() {
        Ok(Some(owner)) => owner,
        Ok(None) => return 0,
        Err(_) => return 1,
    };
    let listener = match owner.bind() {
        Ok(listener) => listener,
        Err(_) => return 1,
    };
    if listener.set_nonblocking(true).is_err() {
        return 1;
    }

    noti::block_on_main(BrokerFuture {
        listener,
        _owner: owner,
        state: BrokerState::default(),
        exit_code: 0,
    })
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn serve() -> i32 {
    eprintln!("agenmux-notifier is macOS-only");
    2
}

pub(crate) fn submit(request: &NotificationRequest) -> io::Result<()> {
    let path = socket_path();
    submit_with(
        || submit_at(&path, request),
        spawn_broker,
        thread::sleep,
        SUBMIT_RETRY_WINDOW,
        BROKER_RESPAWN_INTERVAL,
    )
}

fn submit_with<Attempt, Spawn, Sleep>(
    mut attempt: Attempt,
    mut spawn: Spawn,
    mut sleep: Sleep,
    retry_window: Duration,
    respawn_interval: Duration,
) -> io::Result<()>
where
    Attempt: FnMut() -> io::Result<()>,
    Spawn: FnMut() -> io::Result<()>,
    Sleep: FnMut(Duration),
{
    match attempt() {
        Ok(()) => return Ok(()),
        Err(error) if recoverable_submit_error(&error) => {}
        Err(error) => return Err(error),
    }

    spawn()?;
    let deadline = Instant::now() + retry_window;
    let mut last_spawn = Instant::now();
    loop {
        match attempt() {
            Ok(()) => return Ok(()),
            Err(error) if recoverable_submit_error(&error) && Instant::now() < deadline => {
                let now = Instant::now();
                if broker_closed_before_ack(&error)
                    || now.duration_since(last_spawn) >= respawn_interval
                {
                    spawn()?;
                    last_spawn = Instant::now();
                }
                sleep(SUBMIT_RETRY_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
}

fn submit_at(path: &Path, request: &NotificationRequest) -> io::Result<()> {
    let stream = UnixStream::connect(path)?;
    submit_on_stream(stream, request)
}

fn submit_on_stream(mut stream: UnixStream, request: &NotificationRequest) -> io::Result<()> {
    configure_client_stream(&stream)?;
    write_request(&mut stream, request)?;
    read_ack(&mut stream)
}

fn configure_client_stream(stream: &UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT))
}

fn spawn_broker() -> io::Result<()> {
    let executable = std::env::current_exe()?;
    Command::new(executable)
        .arg("--broker")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

fn retryable_connection_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
    )
}

fn recoverable_submit_error(error: &io::Error) -> bool {
    retryable_connection_error(error) || broker_closed_before_ack(error)
}

fn socket_path() -> PathBuf {
    std::env::temp_dir().join(SOCKET_NAME)
}

fn write_request(stream: &mut UnixStream, request: &NotificationRequest) -> io::Result<()> {
    write_string(stream, &request.title)?;
    write_string(stream, &request.body)?;
    match &request.click {
        Some(click) => write_string(stream, click)?,
        None => stream.write_all(&NONE_LENGTH.to_be_bytes())?,
    }
    stream.flush()
}

fn write_string(stream: &mut UnixStream, value: &str) -> io::Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() > MAX_FIELD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "notification field exceeds maximum size",
        ));
    }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)
}

pub(crate) fn read_request(stream: &mut UnixStream) -> io::Result<NotificationRequest> {
    Ok(NotificationRequest {
        title: read_required_string(stream)?,
        body: read_required_string(stream)?,
        click: read_string(stream, true)?,
    })
}

fn read_required_string(stream: &mut UnixStream) -> io::Result<String> {
    read_string(stream, false)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "required notification field is missing",
        )
    })
}

fn read_string(stream: &mut UnixStream, optional: bool) -> io::Result<Option<String>> {
    let mut length = [0u8; std::mem::size_of::<u32>()];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length);
    if optional && length == NONE_LENGTH {
        return Ok(None);
    }
    if length > MAX_FIELD_BYTES as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "notification field exceeds maximum size",
        ));
    }
    let mut bytes = vec![0u8; length as usize];
    stream.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map(Some).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "notification field is not UTF-8",
        )
    })
}

pub(crate) fn write_ack(stream: &mut UnixStream, accepted: bool) -> io::Result<()> {
    stream.write_all(&[if accepted { ACK_ACCEPTED } else { ACK_REJECTED }])?;
    stream.flush()
}

#[derive(Debug)]
struct BrokerClosedBeforeAck;

impl std::fmt::Display for BrokerClosedBeforeAck {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("broker closed before acknowledging notification request")
    }
}

impl std::error::Error for BrokerClosedBeforeAck {}

fn broker_closed_before_ack(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<BrokerClosedBeforeAck>())
        .is_some()
}

fn read_ack(stream: &mut UnixStream) -> io::Result<()> {
    let mut ack = [0u8; 1];
    match stream.read_exact(&mut ack) {
        Ok(()) => match ack[0] {
            ACK_ACCEPTED => Ok(()),
            ACK_REJECTED => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "broker rejected notification request",
            )),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "broker sent an invalid acknowledgement",
            )),
        },
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            BrokerClosedBeforeAck,
        )),
        Err(error) => Err(error),
    }
}

pub(crate) struct BrokerOwner {
    _lock: File,
    socket_path: PathBuf,
}

impl BrokerOwner {
    pub(crate) fn acquire() -> io::Result<Option<Self>> {
        Self::acquire_at(&std::env::temp_dir())
    }

    pub(crate) fn acquire_at(dir: &Path) -> io::Result<Option<Self>> {
        let lock_path = dir.join(LOCK_NAME);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            let socket_path = dir.join(SOCKET_NAME);
            match fs::remove_file(&socket_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            Ok(Some(Self {
                _lock: file,
                socket_path,
            }))
        } else {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }

    pub(crate) fn bind(&self) -> io::Result<UnixListener> {
        let listener = UnixListener::bind(&self.socket_path)?;
        if let Err(error) =
            fs::set_permissions(&self.socket_path, fs::Permissions::from_mode(0o600))
        {
            drop(listener);
            return Err(error);
        }
        Ok(listener)
    }
}

impl Drop for BrokerOwner {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::io::{self, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tempfile_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let suffix = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "agenmux-notifier-{name}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn request_round_trip_preserves_unicode_and_optional_click() {
        let expected = NotificationRequest {
            title: "Pi finished ✓".into(),
            body: "subject · agenmux:1.2".into(),
            click: Some("'agenmux' 'notification-open' '%12'".into()),
        };
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        write_request(&mut writer, &expected).unwrap();
        assert_eq!(read_request(&mut reader).unwrap(), expected);
    }

    #[test]
    fn request_round_trip_preserves_missing_click() {
        let expected = NotificationRequest {
            title: "Agenmux".into(),
            body: "ready".into(),
            click: None,
        };
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        write_request(&mut writer, &expected).unwrap();
        assert_eq!(read_request(&mut reader).unwrap(), expected);
    }

    #[test]
    fn request_rejects_oversized_field_before_allocating_it() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        writer
            .write_all(&((MAX_FIELD_BYTES + 1) as u32).to_be_bytes())
            .unwrap();
        let error = read_request(&mut reader).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn lock_has_only_one_owner() {
        let dir = tempfile_dir("broker-lock");
        let first = BrokerOwner::acquire_at(&dir).unwrap();
        assert!(first.is_some());
        assert!(BrokerOwner::acquire_at(&dir).unwrap().is_none());
        drop(first);
        assert!(BrokerOwner::acquire_at(&dir).unwrap().is_some());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broker_stays_alive_while_any_response_is_pending() {
        let mut state = BrokerState {
            accepted_any: true,
            ..BrokerState::default()
        };
        state.pending.push(Box::pin(std::future::pending()));
        assert!(!state.should_exit());
    }

    #[test]
    fn broker_exits_after_last_response_finishes() {
        let state = BrokerState {
            accepted_any: true,
            pending: Vec::new(),
            ..BrokerState::default()
        };
        assert!(state.should_exit());
    }

    #[test]
    fn fresh_broker_waits_for_first_request() {
        assert!(!BrokerState::default().should_exit());
    }

    #[test]
    fn broker_acknowledges_and_retains_concurrent_requests_while_authorization_is_pending() {
        use std::sync::mpsc;

        let (release, wait) = mpsc::channel();
        let mut state = BrokerState::default();
        state
            .start_authorization(move || wait.recv().unwrap())
            .unwrap();

        let clients: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|title| {
                let (client, broker) = UnixStream::pair().unwrap();
                let request = NotificationRequest {
                    title: title.into(),
                    body: "body".into(),
                    click: None,
                };
                let sender = thread::spawn(move || submit_on_stream(client, &request));
                (sender, broker)
            })
            .collect();

        for (_, broker) in &clients {
            state.accept_request(broker.try_clone().unwrap());
        }
        for (sender, _) in clients {
            sender.join().unwrap().unwrap();
        }

        assert_eq!(state.queued.len(), 2);
        assert!(state.pending.is_empty());
        release.send(true).unwrap();
    }

    #[test]
    fn authorization_completion_wakes_polling_and_releases_queued_requests() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        use std::task::{Wake, Waker};

        struct TestWake(AtomicBool);
        impl Wake for TestWake {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::Release);
            }
        }

        let (release, wait) = mpsc::channel();
        let mut state = BrokerState::default();
        state.queued.push(NotificationRequest {
            title: "queued".into(),
            body: "body".into(),
            click: None,
        });
        state
            .start_authorization(move || wait.recv().unwrap())
            .unwrap();
        let woke = Arc::new(TestWake(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&woke));
        let mut cx = std::task::Context::from_waker(&waker);
        assert_eq!(state.poll_authorization(&mut cx), Poll::Pending);

        release.send(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !woke.0.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(woke.0.load(Ordering::Acquire));
        assert_eq!(state.poll_authorization(&mut cx), Poll::Ready(true));
        state.complete_authorization(true);
        let released = state.take_authorized_requests();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].title, "queued");
        assert!(state.queued.is_empty());
    }

    #[test]
    fn denied_authorization_discards_queued_requests_and_allows_exit() {
        let mut state = BrokerState {
            accepted_any: true,
            queued: vec![NotificationRequest {
                title: "discarded".into(),
                body: "body".into(),
                click: None,
            }],
            ..BrokerState::default()
        };

        state.complete_authorization(false);

        assert!(state.queued.is_empty());
        assert!(state.should_exit());
    }

    #[test]
    fn client_stream_has_bounded_read_and_write_timeouts() {
        let (stream, _peer) = UnixStream::pair().unwrap();
        configure_client_stream(&stream).unwrap();
        assert_eq!(stream.read_timeout().unwrap(), Some(CLIENT_IO_TIMEOUT));
        assert_eq!(stream.write_timeout().unwrap(), Some(CLIENT_IO_TIMEOUT));
    }

    #[test]
    fn client_write_times_out_when_broker_stops_reading() {
        let (mut client, _broker) = UnixStream::pair().unwrap();
        configure_client_stream(&client).unwrap();
        let chunk = [0u8; 64 * 1024];
        let started = Instant::now();
        let mut timed_out = None;
        for _ in 0..1024 {
            if let Err(error) = client.write_all(&chunk) {
                timed_out = Some(error);
                break;
            }
        }
        let error = timed_out.expect("64 MiB should exceed a stalled Unix socket buffer");

        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn client_times_out_when_broker_never_acknowledges() {
        let (client, mut broker) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            read_request(&mut broker).unwrap();
            thread::sleep(Duration::from_secs(3));
        });
        let request = NotificationRequest {
            title: "title".into(),
            body: "body".into(),
            click: None,
        };

        let started = Instant::now();
        let error = submit_on_stream(client, &request).unwrap_err();
        let elapsed = started.elapsed();
        server.join().unwrap();

        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        assert!(elapsed < Duration::from_millis(2500));
    }

    #[test]
    fn eof_before_ack_is_classified_as_broker_shutdown() {
        let (mut client, broker) = UnixStream::pair().unwrap();
        drop(broker);
        let error = read_ack(&mut client).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(broker_closed_before_ack(&error));
    }

    #[test]
    fn click_command_does_not_block_and_wakes_the_event_loop() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::task::{Wake, Waker};

        struct TestWake(AtomicBool);
        impl Wake for TestWake {
            fn wake(self: Arc<Self>) {
                self.0.store(true, Ordering::Release);
            }
        }

        let mut state = BrokerState {
            accepted_any: true,
            clicks: vec![ClickCommand::spawn("sleep 0.1; exit 7".into()).unwrap()],
            ..BrokerState::default()
        };
        let woke = Arc::new(TestWake(AtomicBool::new(false)));
        let waker = Waker::from(Arc::clone(&woke));
        let mut cx = std::task::Context::from_waker(&waker);
        let started = Instant::now();
        assert!(!state.poll_clicks(&mut cx));
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(!state.should_exit());

        let deadline = Instant::now() + Duration::from_secs(1);
        while !woke.0.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(woke.0.load(Ordering::Acquire));
        assert!(state.poll_clicks(&mut cx));
        assert!(state.should_exit());
    }

    #[test]
    fn submit_respawns_if_broker_exits_between_attempts() {
        let attempts = Cell::new(0);
        let spawns = Cell::new(0);
        let result = submit_with(
            || {
                attempts.set(attempts.get() + 1);
                match attempts.get() {
                    1 => Err(io::Error::new(io::ErrorKind::NotFound, "no broker")),
                    2 => Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "started broker exited",
                    )),
                    _ => Ok(()),
                }
            },
            || {
                spawns.set(spawns.get() + 1);
                Ok(())
            },
            |_| {},
            Duration::from_secs(1),
            Duration::ZERO,
        );

        assert!(result.is_ok());
        assert_eq!(attempts.get(), 3);
        assert_eq!(spawns.get(), 2);
    }

    #[test]
    fn submit_immediately_respawns_if_broker_closes_before_ack() {
        let attempts = Cell::new(0);
        let spawns = Cell::new(0);
        let result = submit_with(
            || {
                attempts.set(attempts.get() + 1);
                match attempts.get() {
                    1 => Err(io::Error::new(io::ErrorKind::NotFound, "no broker")),
                    2 => Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        BrokerClosedBeforeAck,
                    )),
                    _ => Ok(()),
                }
            },
            || {
                spawns.set(spawns.get() + 1);
                Ok(())
            },
            |_| {},
            Duration::from_secs(1),
            Duration::from_secs(30),
        );

        assert!(result.is_ok());
        assert_eq!(attempts.get(), 3);
        assert_eq!(spawns.get(), 2);
    }
}

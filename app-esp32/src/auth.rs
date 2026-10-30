//! Optional HTTP authentication. All tables are bounded; secrets never enter logs.
//!
//! Off by default. With a password set, every HTTP request and WebSocket
//! upgrade passes `gate` (after the Origin check) and the auth endpoints are
//! answered by `route`. Password derivation runs outside the state lock.

use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::*;
use minidsp_protocol::auth::{self, BasicCache, PasswordRecord, RateLimiter, Sessions};
use serde::Deserialize;
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Matches the HTTP server's `max_open_sockets`.
const MAX_SOCKETS: usize = 13;
const MAX_BODY_BYTES: usize = 768;
const RECOVERY_HOLD: Duration = Duration::from_secs(10);

static PROTECTED: AtomicBool = AtomicBool::new(false);
static PROVISIONING: AtomicBool = AtomicBool::new(false);
static PARTITION: OnceLock<EspDefaultNvsPartition> = OnceLock::new();
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static SERVER: AtomicUsize = AtomicUsize::new(0);
static TCP_FD: Mutex<i32> = Mutex::new(-1);

#[derive(Clone, Copy)]
enum Grant {
    Session([u8; 32]),
    /// Verified at upgrade; lasts for the socket. Password changes close it.
    Basic,
}

#[derive(Clone, Copy)]
struct Socket {
    fd: i32,
    grant: Grant,
}

#[derive(Default)]
struct State {
    record: Option<PasswordRecord>,
    secret: [u8; 32],
    sessions: Sessions,
    rate: RateLimiter,
    cache: BasicCache,
    sockets: [Option<Socket>; MAX_SOCKETS],
}

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

pub fn protected() -> bool {
    PROTECTED.load(Ordering::Acquire)
}

pub fn load(partition: &EspDefaultNvsPartition, record: Option<PasswordRecord>) {
    PARTITION.set(partition.clone()).ok();
    state().lock().unwrap().record = record;
    PROTECTED.store(record.is_some(), Ordering::Release);
}

/// `esp_fill_random` is only a true RNG once the radio runs, so the per-boot
/// Basic cache key is drawn after WiFi starts.
pub fn network_ready() {
    state().lock().unwrap().secret = random();
}

pub fn server_started(handle: httpd_handle_t, provisioning: bool) {
    SERVER.store(handle as usize, Ordering::Release);
    PROVISIONING.store(provisioning, Ordering::Release);
}

fn now() -> u64 {
    unsafe { esp_timer_get_time() as u64 / 1_000_000 }
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    unsafe {
        esp_fill_random(bytes.as_mut_ptr().cast(), N);
    }
    bytes
}

fn timed<T>(f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    log::debug!(
        "password derivation took {} ms",
        start.elapsed().as_millis()
    );
    result
}

/// Saves before publishing the new state, so a failed write keeps the old
/// protection. Every change revokes sessions and closes WebSockets.
pub fn set_password(password: Option<&str>) -> Result<(), String> {
    let record = match password {
        Some(password) => Some(
            timed(|| PasswordRecord::new(password, random()))
                .ok_or("password must be 1–128 UTF-8 bytes")?,
        ),
        None => None,
    };
    crate::settings::save_password(
        PARTITION.get().ok_or("settings unavailable")?,
        record.as_ref(),
    )
    .map_err(|e| e.to_string())?;
    let mut s = state().lock().unwrap();
    s.record = record;
    s.sessions = Sessions::default();
    s.cache = BasicCache::default();
    s.sockets = [None; MAX_SOCKETS];
    // Rate budgets survive changes so clearing and re-enabling can't reset an attack budget.
    PROTECTED.store(record.is_some(), Ordering::Release);
    drop(s);
    close_websockets();
    if record.is_some() {
        let fd = TCP_FD.lock().unwrap();
        if *fd >= 0 {
            unsafe {
                libc::shutdown(*fd, libc::SHUT_RDWR);
            }
        }
    }
    Ok(())
}

/// Called with a live stream owned by the bridge thread; clear before dropping it.
pub fn tcp_connected(fd: i32) {
    *TCP_FD.lock().unwrap() = fd;
    if protected() {
        unsafe {
            libc::shutdown(fd, libc::SHUT_RDWR);
        }
    }
}

pub fn tcp_disconnected() {
    *TCP_FD.lock().unwrap() = -1;
}

fn server() -> httpd_handle_t {
    SERVER.load(Ordering::Acquire) as httpd_handle_t
}

fn close_websockets() {
    let handle = server();
    if handle.is_null() {
        return;
    }
    unsafe {
        let mut fds = [0; MAX_SOCKETS];
        let mut len = fds.len();
        if httpd_get_client_list(handle, &mut len, fds.as_mut_ptr()) == ESP_OK {
            for fd in &fds[..len] {
                if httpd_ws_get_fd_info(handle, *fd)
                    == httpd_ws_client_info_t_HTTPD_WS_CLIENT_WEBSOCKET
                {
                    httpd_sess_trigger_close(handle, *fd);
                }
            }
        }
    }
}

fn valid_grant(s: &mut State, grant: Grant, touch: bool) -> bool {
    match grant {
        Grant::Session(key) => s.sessions.valid(&key, now(), touch),
        Grant::Basic => true,
    }
}

/// Closes upgraded sockets whose session expired or was revoked, including
/// ones that never send another frame.
pub fn tick() {
    if !protected() {
        return;
    }
    let handle = server();
    if handle.is_null() {
        return;
    }
    let mut s = state().lock().unwrap();
    for i in 0..s.sockets.len() {
        if let Some(socket) = s.sockets[i] {
            unsafe {
                if httpd_ws_get_fd_info(handle, socket.fd)
                    != httpd_ws_client_info_t_HTTPD_WS_CLIENT_WEBSOCKET
                {
                    s.sockets[i] = None;
                } else if !valid_grant(&mut s, socket.grant, false) {
                    httpd_sess_trigger_close(handle, socket.fd);
                    s.sockets[i] = None;
                }
            }
        }
    }
}

/// Per-frame check for upgraded sockets; frames keep a session alive.
pub fn websocket_allowed(fd: i32) -> bool {
    if !protected() {
        return true;
    }
    let mut s = state().lock().unwrap();
    let grant = s
        .sockets
        .iter()
        .flatten()
        .find(|socket| socket.fd == fd)
        .map(|socket| socket.grant);
    grant.is_some_and(|grant| valid_grant(&mut s, grant, true))
}

unsafe fn header(req: *mut httpd_req_t, name: &CStr) -> Option<String> {
    let len = httpd_req_get_hdr_value_len(req, name.as_ptr());
    if len == 0 || len > 1024 {
        return None;
    }
    let mut buf = vec![0; len + 1];
    if httpd_req_get_hdr_value_str(req, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) != ESP_OK
    {
        return None;
    }
    String::from_utf8(buf[..len].to_vec()).ok()
}

unsafe fn path(req: *mut httpd_req_t) -> &'static str {
    let uri = CStr::from_ptr((*req).uri.as_ptr()).to_str().unwrap_or("");
    uri.split('?').next().unwrap_or("")
}

/// Rate-limit key for the peer. httpd listens on an IPv6 socket
/// (`CONFIG_LWIP_IPV6`), so IPv4 clients arrive as v4-mapped addresses.
unsafe fn ip(req: *mut httpd_req_t) -> u32 {
    let mut peer: sockaddr_storage = std::mem::zeroed();
    let mut len = std::mem::size_of_val(&peer) as socklen_t;
    if lwip_getpeername(
        httpd_req_to_sockfd(req),
        (&mut peer as *mut sockaddr_storage).cast(),
        &mut len,
    ) != 0
    {
        return 0;
    }
    match u32::from(peer.ss_family) {
        AF_INET => {
            (*(&peer as *const sockaddr_storage).cast::<sockaddr_in>())
                .sin_addr
                .s_addr
        }
        AF_INET6 => {
            let addr = (*(&peer as *const sockaddr_storage).cast::<sockaddr_in6>())
                .sin6_addr
                .un
                .u32_addr;
            // A v4-mapped address keeps the IPv4 address in its last word; a
            // native IPv6 peer is keyed by a fold of the whole address.
            if addr[0] == 0 && addr[1] == 0 && addr[2] == u32::from_be(0xffff) {
                addr[3]
            } else {
                addr[0] ^ addr[1] ^ addr[2] ^ addr[3]
            }
        }
        _ => 0,
    }
}

/// Sends a JSON reply. A request body left unread closes the connection, so
/// httpd never parses leftover body bytes as the next request.
unsafe fn respond(
    req: *mut httpd_req_t,
    status: &CStr,
    body: &str,
    headers: &[(&CStr, &CStr)],
    body_read: bool,
) -> esp_err_t {
    httpd_resp_set_status(req, status.as_ptr());
    httpd_resp_set_type(req, c"application/json".as_ptr());
    httpd_resp_set_hdr(req, c"Cache-Control".as_ptr(), c"no-store".as_ptr());
    for (key, value) in headers {
        httpd_resp_set_hdr(req, key.as_ptr(), value.as_ptr());
    }
    httpd_resp_send(req, body.as_ptr().cast(), body.len() as isize);
    if !body_read && (*req).content_len > 0 {
        ESP_FAIL
    } else {
        ESP_OK
    }
}

/// Browsers show their own Basic dialog for any challenged 401, fetch()
/// included, so only native clients get `WWW-Authenticate`. Over plain HTTP
/// to a LAN address browsers omit `Sec-Fetch-*`; same-origin fetches still
/// carry `Referer`, and cross-origin ones `Origin`.
unsafe fn from_browser(req: *mut httpd_req_t) -> bool {
    [c"Sec-Fetch-Mode", c"Origin", c"Referer"]
        .iter()
        .any(|name| httpd_req_get_hdr_value_len(req, name.as_ptr()) > 0)
}

/// Gate rejection; the request body is never read.
unsafe fn unauthorized(req: *mut httpd_req_t) -> esp_err_t {
    let navigation = (*req).method as u32 == http_method_HTTP_GET
        && header(req, c"Accept").is_some_and(|h| h.contains("text/html"))
        && header(req, c"Upgrade").is_none();
    if navigation {
        let uri = CStr::from_ptr((*req).uri.as_ptr()).to_str().unwrap_or("/");
        let encoded: String = auth::next_path(uri)
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        let location = CString::new(format!("/login?next={encoded}")).unwrap();
        respond(
            req,
            c"302 Found",
            r#"{"error":"login required"}"#,
            &[(c"Location", &location)],
            false,
        )
    } else if from_browser(req) {
        respond(
            req,
            c"401 Unauthorized",
            r#"{"error":"authentication required"}"#,
            &[],
            false,
        )
    } else {
        respond(
            req,
            c"401 Unauthorized",
            r#"{"error":"authentication required"}"#,
            &[(c"WWW-Authenticate", c"Basic realm=\"minidsp-open\"")],
            false,
        )
    }
}

unsafe fn throttled(req: *mut httpd_req_t, delay: u64, body_read: bool) -> esp_err_t {
    let delay = CString::new(delay.to_string()).unwrap();
    respond(
        req,
        c"429 Too Many Requests",
        r#"{"error":"try again later"}"#,
        &[(c"Retry-After", &delay)],
        body_read,
    )
}

fn session_token<'a>(authorization: Option<&'a str>, cookies: Option<&'a str>) -> Option<&'a str> {
    authorization
        .and_then(|h| h.strip_prefix("Bearer "))
        .or_else(|| cookies.and_then(auth::cookie_token))
}

unsafe fn authorize(req: *mut httpd_req_t) -> Result<Grant, esp_err_t> {
    let authorization = header(req, c"Authorization");
    let cookies = header(req, c"Cookie");
    let now = now();
    if let Some(token) = session_token(authorization.as_deref(), cookies.as_deref()) {
        let key = auth::digest(token);
        if state().lock().unwrap().sessions.valid(&key, now, true) {
            return Ok(Grant::Session(key));
        }
    }
    let basic = authorization.as_deref().filter(|h| {
        h.split_once(' ')
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("Basic"))
    });
    if let Some(authorization) = basic {
        let ip = ip(req);
        let (key, cached, delay, record) = {
            let s = state().lock().unwrap();
            let key = auth::basic_digest(&s.secret, authorization);
            (
                key,
                s.cache.valid(&key, now),
                s.rate.retry_after(ip, now),
                s.record,
            )
        };
        if cached {
            return Ok(Grant::Basic);
        }
        if delay > 0 {
            return Err(throttled(req, delay, false));
        }
        let valid = auth::basic_password(authorization)
            .is_some_and(|password| timed(|| record.is_some_and(|r| r.verify(&password))));
        let mut s = state().lock().unwrap();
        if valid {
            s.cache.insert(key, now);
            return Ok(Grant::Basic);
        }
        s.rate.failure(ip, now);
    }
    Err(unauthorized(req))
}

/// Shared HTTP gate and WebSocket pre-upgrade gate; the caller checks Origin
/// first. `Err` carries the already-sent rejection's handler result.
pub unsafe fn gate(req: *mut httpd_req_t, websocket: bool) -> Result<(), esp_err_t> {
    if !protected() {
        return Ok(());
    }
    let path = path(req);
    let method = (*req).method as u32;
    let get = method == http_method_HTTP_GET;
    let public = !websocket
        && ((path == "/login" && (get || method == http_method_HTTP_POST))
            || (get
                && PROVISIONING.load(Ordering::Acquire)
                && matches!(
                    path,
                    "/" | "/generate_204"
                        | "/gen_204"
                        | "/hotspot-detect.html"
                        | "/library/test/success.html"
                        | "/connecttest.txt"
                        | "/ncsi.txt"
                        | "/redirect"
                        | "/canonical.html"
                        | "/success.txt"
                )));
    if public {
        return Ok(());
    }
    let grant = authorize(req)?;
    if websocket {
        let fd = httpd_req_to_sockfd(req);
        let mut s = state().lock().unwrap();
        let index = s
            .sockets
            .iter()
            .position(|socket| socket.is_some_and(|socket| socket.fd == fd))
            .or_else(|| s.sockets.iter().position(Option::is_none));
        let Some(index) = index else {
            drop(s);
            return Err(respond(
                req,
                c"503 Service Unavailable",
                r#"{"error":"too many sessions"}"#,
                &[],
                false,
            ));
        };
        s.sockets[index] = Some(Socket { fd, grant });
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordChange {
    current: Option<String>,
    new: Option<String>,
    #[serde(default)]
    clear: bool,
}

/// Reads a small body completely. `Err` means it is too large or arrived
/// short; reply with `body_read: false` so the connection closes.
unsafe fn read_body(req: *mut httpd_req_t) -> Result<Vec<u8>, ()> {
    let len = (*req).content_len;
    if len > MAX_BODY_BYTES {
        return Err(());
    }
    let mut bytes = vec![0; len];
    let mut used = 0;
    while used < len {
        let count = httpd_req_recv(req, bytes[used..].as_mut_ptr().cast(), len - used);
        if count <= 0 {
            return Err(());
        }
        used += count as usize;
    }
    Ok(bytes)
}

unsafe fn bad_body(req: *mut httpd_req_t) -> esp_err_t {
    respond(
        req,
        c"413 Content Too Large",
        r#"{"error":"request body too large or incomplete"}"#,
        &[],
        false,
    )
}

unsafe fn bad_request(req: *mut httpd_req_t, body: &str) -> esp_err_t {
    respond(req, c"400 Bad Request", body, &[], true)
}

/// Rate-limits and verifies a submitted password after its body was read.
unsafe fn check_password(
    req: *mut httpd_req_t,
    password: &str,
    status: &CStr,
    error: &str,
) -> Result<(), esp_err_t> {
    let ip = ip(req);
    let now = now();
    let (delay, record) = {
        let s = state().lock().unwrap();
        (s.rate.retry_after(ip, now), s.record)
    };
    if delay > 0 {
        return Err(throttled(req, delay, true));
    }
    if timed(|| record.is_some_and(|r| r.verify(password))) {
        return Ok(());
    }
    state().lock().unwrap().rate.failure(ip, now);
    Err(respond(req, status, error, &[], true))
}

unsafe fn login(req: *mut httpd_req_t) -> esp_err_t {
    let Ok(bytes) = read_body(req) else {
        return bad_body(req);
    };
    if !protected() {
        return bad_request(req, r#"{"error":"password is disabled"}"#);
    }
    let Ok(login) = serde_json::from_slice::<Login>(&bytes) else {
        return bad_request(req, r#"{"error":"expected {\"password\":\"...\"}"}"#);
    };
    if let Err(result) = check_password(
        req,
        &login.password,
        c"401 Unauthorized",
        r#"{"error":"incorrect password"}"#,
    ) {
        return result;
    }
    let token = auth::token(&random());
    state()
        .lock()
        .unwrap()
        .sessions
        .insert(auth::digest(&token), now());
    let cookie = CString::new(auth::cookie(&token)).unwrap();
    respond(
        req,
        c"200 OK",
        &format!(r#"{{"token":"{token}"}}"#),
        &[(c"Set-Cookie", &cookie)],
        true,
    )
}

fn clear_cookie() -> CString {
    CString::new(auth::CLEAR_COOKIE).unwrap()
}

unsafe fn logout(req: *mut httpd_req_t) -> esp_err_t {
    let authorization = header(req, c"Authorization");
    let cookies = header(req, c"Cookie");
    if let Some(token) = session_token(authorization.as_deref(), cookies.as_deref()) {
        state()
            .lock()
            .unwrap()
            .sessions
            .revoke(&auth::digest(token));
    }
    tick();
    respond(
        req,
        c"200 OK",
        "{}",
        &[(c"Set-Cookie", &clear_cookie())],
        false,
    )
}

unsafe fn change_password(req: *mut httpd_req_t) -> esp_err_t {
    let Ok(bytes) = read_body(req) else {
        return bad_body(req);
    };
    let Ok(change) = serde_json::from_slice::<PasswordChange>(&bytes) else {
        return bad_request(req, r#"{"error":"invalid password settings"}"#);
    };
    if change.clear == change.new.is_some() {
        return bad_request(req, r#"{"error":"provide new or clear"}"#);
    }
    if change
        .new
        .as_ref()
        .is_some_and(|password| password.is_empty() || password.len() > auth::MAX_PASSWORD_BYTES)
    {
        return bad_request(req, r#"{"error":"password must be 1–128 UTF-8 bytes"}"#);
    }
    if protected() {
        if let Err(result) = check_password(
            req,
            change.current.as_deref().unwrap_or(""),
            c"403 Forbidden",
            r#"{"error":"current password is incorrect"}"#,
        ) {
            return result;
        }
    }
    match set_password(change.new.as_deref()) {
        Ok(()) => respond(
            req,
            c"200 OK",
            "{}",
            &[(c"Set-Cookie", &clear_cookie())],
            true,
        ),
        Err(e) => {
            log::error!("password could not be saved: {e}");
            respond(
                req,
                c"500 Internal Server Error",
                r#"{"error":"password could not be saved"}"#,
                &[],
                true,
            )
        }
    }
}

/// Answers the auth endpoints after the gate. They are registered as normal
/// routes too, so httpd dispatches them through the wrapper.
pub unsafe fn route(req: *mut httpd_req_t) -> Option<esp_err_t> {
    let method = (*req).method as u32;
    match (path(req), method) {
        ("/settings/password", m) if m == http_method_HTTP_GET => Some(respond(
            req,
            c"200 OK",
            if protected() {
                r#"{"passwordSet":true}"#
            } else {
                r#"{"passwordSet":false}"#
            },
            &[],
            false,
        )),
        ("/settings/password", m) if m == http_method_HTTP_POST => Some(change_password(req)),
        ("/login", m) if m == http_method_HTTP_POST => Some(login(req)),
        ("/logout", m) if m == http_method_HTTP_POST => Some(logout(req)),
        _ => None,
    }
}

static MAIN_TASK: AtomicUsize = AtomicUsize::new(0);
static RECOVERY_READY: AtomicBool = AtomicBool::new(false);
static HELD_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

/// Arms the BOOT-button recovery gesture on the calling (main) task. NVS
/// writes need an internal-RAM stack, which the main task has.
pub fn recovery_ready() {
    MAIN_TASK.store(
        unsafe { xTaskGetCurrentTaskHandle() } as usize,
        Ordering::Release,
    );
    unsafe {
        gpio_set_direction(0, gpio_mode_t_GPIO_MODE_INPUT);
        gpio_set_pull_mode(0, gpio_pull_mode_t_GPIO_PULLUP_ONLY);
    }
    RECOVERY_READY.store(true, Ordering::Release);
}

pub fn on_main_task() -> bool {
    RECOVERY_READY.load(Ordering::Acquire)
        && MAIN_TASK.load(Ordering::Acquire) == unsafe { xTaskGetCurrentTaskHandle() } as usize
}

/// Expires WebSocket grants and polls the BOOT button. Also called from the
/// main task's USB waits, so recovery works without a MiniDSP attached.
pub fn maintenance() {
    tick();
    if !on_main_task() {
        return;
    }
    let mut held = HELD_SINCE.lock().unwrap();
    if unsafe { gpio_get_level(0) } != 0 {
        if held.take().is_some() {
            crate::led::set_recovery(false);
        }
        return;
    }
    let start = *held.get_or_insert_with(|| {
        crate::led::set_recovery(true);
        Instant::now()
    });
    if start.elapsed() < RECOVERY_HOLD {
        return;
    }
    log::warn!("BOOT held for {RECOVERY_HOLD:?}: clearing password and WiFi");
    let result = set_password(None).and_then(|()| {
        crate::wifi::clear_credentials(PARTITION.get().ok_or("settings unavailable")?)
            .map_err(|e| e.to_string())
    });
    match result {
        Ok(()) => unsafe {
            esp_restart();
        },
        Err(e) => {
            log::error!("physical recovery could not clear NVS: {e}");
            crate::led::set_recovery(false);
            *held = None;
        }
    }
}

/// Sleeps in 100 ms slices so the recovery gesture stays responsive.
pub fn responsive_sleep(duration: Duration) {
    if !RECOVERY_READY.load(Ordering::Acquire) {
        std::thread::sleep(duration);
        return;
    }
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        maintenance();
        std::thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100)),
        );
    }
}

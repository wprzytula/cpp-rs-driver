use crate::LOGGER;
use crate::argconv::{
    CConst, CassBorrowedSharedPtr, CassStrNulTerminated, FFI, FromRef, RefFFI, str_to_arr,
};
use crate::cass_log_types::{CassLogLevel, CassLogMessage};
use std::convert::TryFrom;
use std::fmt::Debug;
use std::fmt::Write;
use std::os::raw::{c_char, c_void};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::Level;
use tracing::debug;
use tracing::field::Field;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::reload;

impl FFI for CassLogMessage {
    type Origin = FromRef;
}

pub type CassLogCallback = Option<
    unsafe extern "C" fn(message: CassBorrowedSharedPtr<CassLogMessage, CConst>, data: *mut c_void),
>;

unsafe extern "C" fn noop_log_callback(
    _message: CassBorrowedSharedPtr<CassLogMessage, CConst>,
    _data: *mut c_void,
) {
}

pub(crate) struct Logger {
    pub(crate) cb: CassLogCallback,
    pub(crate) data: *mut c_void,
}

// The field `data` in the struct `Logger` is neither Send nor Sync.
// It can be mutated only in the user-provided `CassLogCallback`, so it is safe
// to implement Sync and Send manually for the `Logger` struct.
unsafe impl Sync for Logger {}
unsafe impl Send for Logger {}

impl From<Level> for CassLogLevel {
    fn from(level: Level) -> Self {
        match level {
            Level::TRACE => CassLogLevel::CASS_LOG_TRACE,
            Level::DEBUG => CassLogLevel::CASS_LOG_DEBUG,
            Level::INFO => CassLogLevel::CASS_LOG_INFO,
            Level::WARN => CassLogLevel::CASS_LOG_WARN,
            Level::ERROR => CassLogLevel::CASS_LOG_ERROR,
        }
    }
}

impl TryFrom<CassLogLevel> for Level {
    type Error = ();

    fn try_from(log_level: CassLogLevel) -> Result<Self, Self::Error> {
        let level = match log_level {
            CassLogLevel::CASS_LOG_TRACE => Level::TRACE,
            CassLogLevel::CASS_LOG_DEBUG => Level::DEBUG,
            CassLogLevel::CASS_LOG_INFO => Level::INFO,
            CassLogLevel::CASS_LOG_WARN => Level::WARN,
            CassLogLevel::CASS_LOG_ERROR => Level::ERROR,
            CassLogLevel::CASS_LOG_CRITICAL => Level::ERROR,
            _ => return Err(()),
        };

        Ok(level)
    }
}

pub(crate) const CASS_LOG_MAX_MESSAGE_SIZE: usize = 1024;

pub(crate) unsafe extern "C" fn stderr_log_callback(
    message: CassBorrowedSharedPtr<CassLogMessage, CConst>,
    _data: *mut c_void,
) {
    let message =
        RefFFI::as_ref(message).expect("message pointer is always valid; set by on_event");

    eprintln!(
        "{} [{}] ({}:{}) {}",
        message.time_ms,
        unsafe { cass_log_level_string(message.severity).to_str() }
            .expect("cass_log_level_string always returns a valid UTF-8 c-string literal"),
        unsafe { CassStrNulTerminated::from_raw(message.file).to_str() }
            .expect("file is set to a null-terminated Rust string by on_event"),
        message.line,
        unsafe { CassStrNulTerminated::from_raw(message.message.as_ptr()).to_str() }
            .expect("message is populated from a Rust String via str_to_arr"),
    )
}

pub(crate) struct CustomLayer;

pub(crate) struct PrintlnVisitor {
    log_message: String,
}

// Collects all fields and values in a single log event into a single String
// to set into CassLogMessage::message.
impl tracing::field::Visit for PrintlnVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if self.log_message.is_empty() {
            write!(self.log_message, "{field}: {value:?}").unwrap();
        } else {
            write!(self.log_message, ", {field}: {value:?}").unwrap();
        }
    }
}

impl<S> Layer<S> for CustomLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let start = SystemTime::now();
        let since_the_epoch = start
            .duration_since(UNIX_EPOCH)
            .expect("Current time is before UNIX_EPOCH");
        let log_time_ms = since_the_epoch.as_millis() as u64;

        let message = "";
        let mut target = event.metadata().target().to_string();
        target.push('\0');

        let mut log_message = CassLogMessage {
            time_ms: log_time_ms,
            severity: (*event.metadata().level()).into(),
            file: target.as_ptr() as *const c_char,
            line: event.metadata().line().unwrap_or(0) as i32,
            function: c"".as_ptr() as *const c_char, // ignored, as cannot be fetched from event metadata
            message: str_to_arr(message),
        };

        let mut visitor = PrintlnVisitor {
            log_message: message.to_string(),
        };
        event.record(&mut visitor);

        visitor.log_message.push('\0');
        log_message.message =
            str_to_arr::<{ CASS_LOG_MAX_MESSAGE_SIZE }>(visitor.log_message.as_str());

        let logger = LOGGER.read().unwrap();

        if let Some(log_cb) = logger.cb {
            unsafe {
                log_cb(RefFFI::as_ptr(&log_message), logger.data);
            }
        }
    }
}

/// The log level that the driver starts with, before the user calls
/// [`cass_log_set_level`]. It matches the cpp-driver's default.
const DEFAULT_LOG_LEVEL: Level = Level::WARN;

/// A handle that allows mutating the level of the filter of the tracing
/// subscriber installed by the driver.
type LogLevelHandle = reload::Handle<LevelFilter, tracing_subscriber::Registry>;

/// Installs the driver's global tracing subscriber upon first access,
/// and yields the handle used to mutate its log level later on.
///
/// The handle is `None` if some other tracing subscriber was already installed
/// globally - be it by the application that embeds the driver, or by the
/// driver's own unit tests. In such case the driver does not own the logging
/// configuration, and thus must not (and cannot) change the log level.
static LOG_LEVEL_HANDLE: LazyLock<Option<LogLevelHandle>> = LazyLock::new(|| {
    let (filter, handle) = reload::Layer::new(LevelFilter::from_level(DEFAULT_LOG_LEVEL));

    tracing::subscriber::set_global_default(
        tracing_subscriber::registry()
            .with(filter)
            .with(CustomLayer),
    )
    .ok()
    .map(|()| handle)
});

/// Makes sure that the driver's tracing subscriber is installed.
///
/// This is idempotent and cheap (an atomic load after the first call), so it
/// can be called from any entry point that could be the first one that the
/// application calls.
pub(crate) fn init_logging() {
    LazyLock::force(&LOG_LEVEL_HANDLE);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cass_log_set_level(log_level: CassLogLevel) {
    init_logging();

    if log_level == CassLogLevel::CASS_LOG_DISABLED {
        debug!("Logging is disabled!");
        return;
    }

    let level = Level::try_from(log_level).unwrap_or(DEFAULT_LOG_LEVEL);

    let Some(handle) = LOG_LEVEL_HANDLE.as_ref() else {
        // Some other tracing subscriber is installed globally. Not ours to reconfigure.
        return;
    };

    // The only possible error is a poisoned lock inside the handle, which can
    // only happen if a previous `modify` panicked. Nothing we can do about it.
    let _ = handle.modify(|filter| *filter = LevelFilter::from_level(level));

    // Emitted after the update, so that it appears iff the new level admits it.
    debug!("Log level is set to {}", level);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cass_log_level_string(
    log_level: CassLogLevel,
) -> CassStrNulTerminated<'static> {
    let log_level_str = match log_level {
        CassLogLevel::CASS_LOG_TRACE => c"TRACE",
        CassLogLevel::CASS_LOG_DEBUG => c"DEBUG",
        CassLogLevel::CASS_LOG_INFO => c"INFO",
        CassLogLevel::CASS_LOG_WARN => c"WARN",
        CassLogLevel::CASS_LOG_ERROR => c"ERROR",
        CassLogLevel::CASS_LOG_CRITICAL => c"CRITICAL",
        CassLogLevel::CASS_LOG_DISABLED => c"DISABLED",
        _ => c"",
    };

    CassStrNulTerminated::from_cstr(log_level_str)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cass_log_set_callback(callback: CassLogCallback, data: *mut c_void) {
    init_logging();

    let logger = Logger {
        cb: Some(callback.unwrap_or(noop_log_callback)),
        data,
    };

    *LOGGER.write().unwrap() = logger;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cass_log_get_callback_and_data(
    callback_out: *mut CassLogCallback,
    data_out: *mut *const c_void,
) {
    let logger = LOGGER.read().unwrap();

    unsafe {
        *callback_out = logger.cb;
        *data_out = logger.data;
    }
}

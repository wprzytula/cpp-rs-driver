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

impl TryFrom<CassLogLevel> for LevelFilter {
    type Error = ();

    fn try_from(log_level: CassLogLevel) -> Result<Self, Self::Error> {
        match log_level {
            // `CASS_LOG_DISABLED` has no `Level` counterpart - disabling logging
            // is expressed as a filter that admits nothing.
            CassLogLevel::CASS_LOG_DISABLED => Ok(LevelFilter::OFF),
            other => Level::try_from(other).map(LevelFilter::from),
        }
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
const DEFAULT_LOG_LEVEL_FILTER: LevelFilter = LevelFilter::WARN;

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
    let (filter, handle) = reload::Layer::new(DEFAULT_LOG_LEVEL_FILTER);

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

/// Sets the log level.
///
/// Only setting the log level before any other interaction with the driver's
/// API is fully supported. Support for altering the log level during the
/// driver's operation is experimental, might misbehave (by displaying more or
/// fewer logs than expected), and may be removed in the future.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cass_log_set_level(log_level: CassLogLevel) {
    init_logging();

    let filter = LevelFilter::try_from(log_level).unwrap_or(DEFAULT_LOG_LEVEL_FILTER);

    let Some(handle) = LOG_LEVEL_HANDLE.as_ref() else {
        // Some other tracing subscriber is installed globally. Not ours to reconfigure.
        return;
    };

    if filter == LevelFilter::OFF {
        // Emitted before the update, so that it is not filtered out by it.
        debug!("Logging is disabled!");
    }

    // The only possible error is a poisoned lock inside the handle, which can
    // only happen if a previous `modify` panicked. Nothing we can do about it.
    let _ = handle.modify(|f| *f = filter);

    // Emitted after the update, so that it appears iff the new level admits it.
    debug!("Log level is set to {}", filter);
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

#[cfg(test)]
mod tests {
    use std::os::raw::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rusty_fork::rusty_fork_test;
    use tracing::{error, info, trace_span, warn};

    use crate::argconv::{CConst, CassBorrowedSharedPtr};
    use crate::cass_log_types::{CassLogLevel, CassLogMessage};

    use super::{cass_log_set_callback, cass_log_set_level};

    /// Counts the log events that made it through the driver's filter
    /// to the user-provided log callback.
    #[derive(Default)]
    struct EventCounter(AtomicUsize);

    impl EventCounter {
        /// Returns the number of events captured since the previous call.
        fn take(&self) -> usize {
            self.0.swap(0, Ordering::SeqCst)
        }
    }

    unsafe extern "C" fn counting_log_callback(
        _message: CassBorrowedSharedPtr<CassLogMessage, CConst>,
        data: *mut c_void,
    ) {
        let counter = unsafe { &*(data as *const EventCounter) };
        counter.0.fetch_add(1, Ordering::SeqCst);
    }

    rusty_fork_test! {
        #[test]
        /// Verifies that the log level can be set at any point of the driver's
        /// lifetime, any number of times.
        ///
        /// This is run with rusty_fork in order to have a fresh process. The
        /// tracing subscriber is global and can only be installed once, so the
        /// test must neither share a process with other tests (which install
        /// their own subscriber via `setup_tracing`) nor with another run of
        /// itself. For the same reason the test must not call `setup_tracing`.
        fn log_level_is_mutable() {
            let counter = EventCounter::default();
            let data = std::ptr::from_ref(&counter).cast::<c_void>().cast_mut();

            // Each of these emits from a single tracing callsite, no matter how
            // many times it is called. Reusing them across log level changes is
            // what makes this test cover the invalidation of tracing's interest
            // cache, which memoizes per-callsite whether anyone is interested
            // in its events.
            let emit_info = || info!("An INFO event");
            let emit_warn = || warn!("A WARN event");
            let emit_error = || error!("An ERROR event");

            // An event emitted before any `cass_log_set_level` call already
            // reaches the callback, at the default (WARN) level...
            unsafe { cass_log_set_callback(Some(counting_log_callback), data) };
            emit_warn();
            assert_eq!(counter.take(), 1);

            // ...and one below that level does not.
            emit_info();
            assert_eq!(counter.take(), 0);

            // Lowering the level admits more events, including from callsites
            // whose events were already filtered out above.
            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_TRACE) };
            counter.take(); // Discard the driver's own log about the change.
            emit_info();
            assert_eq!(counter.take(), 1);

            // Raising it filters them out again - the level is mutable both
            // ways, also for callsites that were admitted a moment ago.
            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_ERROR) };
            counter.take();
            emit_info();
            emit_warn();
            assert_eq!(counter.take(), 0);
            emit_error();
            assert_eq!(counter.take(), 1);

            // Logging can be disabled entirely...
            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_DISABLED) };
            counter.take();
            emit_error();
            assert_eq!(counter.take(), 0);

            // ...and enabled back again.
            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_WARN) };
            counter.take();
            emit_warn();
            assert_eq!(counter.take(), 1);
        }

        #[test]
        /// Verifies that the enabledness of a newly created span follows the
        /// current log level.
        ///
        /// The Rust driver relies on this to skip costly work: it creates a
        /// fresh `trace_span!` per request (`RequestSpan` in the driver's
        /// `observability/driver_tracing.rs`) and builds the replica listing
        /// for that request only `if !span.span().is_disabled()` (in the
        /// driver's `client/session.rs`). Were span enabledness not to follow
        /// the log level, lowering the level at runtime would not make the
        /// driver start collecting that information.
        ///
        /// Run with rusty_fork for the same reason as
        /// `log_level_is_mutable` - see the comment there.
        fn span_enabledness_follows_log_level() {
            // A single callsite, called repeatedly - just like the driver,
            // which creates all its request spans from a handful of callsites.
            let make_request_span = || trace_span!("Request");

            // The default level (WARN) does not admit a TRACE span, so the
            // driver skips building the replica listing.
            assert!(make_request_span().is_disabled());

            // Lowering the level enables the spans created from now on, so new
            // requests do collect the replica listing.
            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_TRACE) };
            assert!(!make_request_span().is_disabled());

            // Raising it back makes the driver skip that work again.
            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_ERROR) };
            assert!(make_request_span().is_disabled());
        }

        #[test]
        /// Verifies that log events follow the current log level even when they
        /// are emitted from inside a span that was created before the level
        /// changed - possibly while that span was disabled.
        ///
        /// This is what makes the logs of the driver's long-running tasks
        /// (the per-connection router, the connection pool refiller, the
        /// cluster and metadata workers) react to a level change. Those tasks
        /// are not instrumented with any span at all, and all their events are
        /// plain callsites, so both cases covered here apply to them.
        ///
        /// The enabledness of a span, on the other hand, is fixed when the span
        /// is constructed and never re-evaluated - this test asserts that too,
        /// in both directions, so that the limitation is documented rather than
        /// assumed. It does not affect the driver, whose spans never outlive a
        /// single request, but it does mean that span *fields* recorded on a
        /// long-lived span (and any work guarded by its enabledness) would not
        /// react to a level change.
        ///
        /// Run with rusty_fork for the same reason as
        /// `log_level_is_mutable` - see the comment there.
        fn long_lived_spans_do_not_freeze_the_log_level() {
            let counter = EventCounter::default();
            let data = std::ptr::from_ref(&counter).cast::<c_void>().cast_mut();
            unsafe { cass_log_set_callback(Some(counting_log_callback), data) };

            let make_span = || trace_span!("Long-running task");
            let emit_info = || info!("An INFO event");

            // Created at the default (WARN) level, so the span is disabled.
            let long_lived = make_span();
            assert!(long_lived.is_disabled());

            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_TRACE) };
            counter.take(); // Discard the driver's own log about the change.

            // An event emitted from inside that disabled span is admitted, as
            // its enabledness depends on the event's own callsite and the
            // current level only.
            {
                let _guard = long_lived.enter();
                emit_info();
                assert_eq!(counter.take(), 1);
            }

            // The same holds with no span in scope at all, which is how the
            // driver's long-running tasks emit their events.
            emit_info();
            assert_eq!(counter.take(), 1);

            // Raising the level silences them again, still inside that span.
            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_ERROR) };
            counter.take();
            {
                let _guard = long_lived.enter();
                emit_info();
                assert_eq!(counter.take(), 0);
            }

            // The span itself, however, keeps the enabledness it was created
            // with. It was created while disabled, and stays disabled even
            // though TRACE was enabled in between.
            assert!(long_lived.is_disabled());

            // And the other way around: a span created while enabled stays
            // enabled after the level is raised.
            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_TRACE) };
            let created_while_enabled = make_span();
            unsafe { cass_log_set_level(CassLogLevel::CASS_LOG_ERROR) };
            assert!(!created_while_enabled.is_disabled());
        }
    }
}

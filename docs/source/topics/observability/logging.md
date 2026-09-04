# Logging

The driver's logging system uses `stderr` by default and the log level `CASS_LOG_WARN`. Both of these settings can be changed using the driver's `cass_log_*()` configuration functions.

Logging can be configured at any point of the driver's lifetime, any number of times. Note, though, that events emitted before a custom logging callback is installed are handled by the default one, i.e. printed to `stderr`.

## Log Level

To update the log level use `cass_log_set_level()`. It may be called at any time, and as many times as needed - the new level applies to all events emitted afterwards. Pass `CASS_LOG_DISABLED` to silence the driver completely.

```c
cass_log_set_level(CASS_LOG_INFO);

/* Create cluster and connect session */
```

## Custom Logging Callback

The use of a logging callback allows an application to log messages to a file, syslog, or any other logging mechanism. This callback must be thread-safe because it is possible for it to be called from multiple threads concurrently. The `data` parameter allows custom resources to be passed to the logging callback.

```c
void on_log(const CassLogMessage* message, void* data) {
  /* Handle logging */
}

int main() {
  void* log_data = NULL /* Custom log resource */;
  cass_log_set_callback(on_log, log_data);
  cass_log_set_level(CASS_LOG_INFO);

  /* Create cluster and connect session */

}
```

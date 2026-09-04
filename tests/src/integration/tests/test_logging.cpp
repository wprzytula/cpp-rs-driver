/*
  Copyright (c) DataStax, Inc.

  Licensed under the Apache License, Version 2.0 (the "License");
  you may not use this file except in compliance with the License.
  You may obtain a copy of the License at

  http://www.apache.org/licenses/LICENSE-2.0

  Unless required by applicable law or agreed to in writing, software
  distributed under the License is distributed on an "AS IS" BASIS,
  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
  See the License for the specific language governing permissions and
  limitations under the License.
*/

#include "integration.hpp"

#include <atomic>

class LoggingTests : public Integration {
public:
  LoggingTests() { is_ccm_requested_ = false; }

  // The driver emits log events from its own background threads, so the flag
  // is atomic - both the write here and the read done by the assertion in the
  // test body can race with those threads.
  static void log(const CassLogMessage* log, void* data) {
    std::atomic<bool>* is_triggered = static_cast<std::atomic<bool>*>(data);
    is_triggered->store(true);
  }

  // Strictly speaking, `CassLogCallback` is a function type with C language
  // linkage (the typedef sits inside the `extern "C"` block in cassandra.h),
  // whereas this is a C++ function, so passing it as a `CassLogCallback` is
  // ill-formed by the letter of the standard. It cannot be fixed here either:
  // `extern "C"` on a class member is ill-formed as well; it would take a free
  // function at namespace scope.
  // In practice this is a non-issue: no mainstream compiler implements that
  // rule (CWG 1555 - GCC, Clang and MSVC all ignore language linkage in the
  // type system), and no supported platform uses a different calling
  // convention for `extern "C"` and `extern "C++"` functions. The sibling
  // `log` callback above and the test framework's own `Logger::log` rely on
  // exactly the same thing.
  // Atomic for the same reason as `log` above.
  static void count(const CassLogMessage* log, void* data) {
    std::atomic<int>* counter = static_cast<std::atomic<int>*>(data);
    ++(*counter);
  }
};

/**
 * Ensure the driver is calling the client logging callback
 */
CASSANDRA_INTEGRATION_TEST_F(LoggingTests, Callback) {
  CHECK_FAILURE;

  std::atomic<bool> is_triggered(false);
  cass_log_set_callback(LoggingTests::log, &is_triggered);
  // This will emit a log event in on debug level which will trigger the `log` callback.
  cass_log_set_level(CASS_LOG_DEBUG);
  default_cluster().connect("", false);
  EXPECT_TRUE(is_triggered.load());
}

/**
 * Ensure that the log level can be changed at any time, any number of times.
 *
 * This used to be impossible: only the first call to `cass_log_set_level` had
 * any effect, and it had to happen before anything logged.
 */
CASSANDRA_INTEGRATION_TEST_F(LoggingTests, SetLevelIsMutable) {
  CHECK_FAILURE;

  std::atomic<int> count(0);
  cass_log_set_callback(LoggingTests::count, &count);

  // `cass_log_set_level` itself emits a DEBUG event confirming the new level,
  // so the callback is triggered iff the new level admits DEBUG events.
  cass_log_set_level(CASS_LOG_DEBUG);
  EXPECT_GT(count.load(), 0);

  // Raising the level must take effect...
  count.store(0);
  cass_log_set_level(CASS_LOG_ERROR);
  EXPECT_EQ(count.load(), 0);

  // ...and so must lowering it back.
  cass_log_set_level(CASS_LOG_TRACE);
  EXPECT_GT(count.load(), 0);

  // CASS_LOG_DISABLED silences everything, including the events emitted while
  // the driver attempts (and fails) to connect.
  cass_log_set_level(CASS_LOG_DISABLED);
  count.store(0);
  default_cluster().connect("", false);
  EXPECT_EQ(count.load(), 0);

  // Logging can be enabled back again.
  cass_log_set_level(CASS_LOG_TRACE);
  EXPECT_GT(count.load(), 0);
}

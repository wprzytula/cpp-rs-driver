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
# This directory contains C++ utility code inherited from the legacy DataStax
# driver. The Rust driver does not use it; only integration tests compile a
# subset of these sources directly (see tests/CMakeLists.txt).
#
# This CMakeLists.txt:
#  1. Builds the sparsehash headers (provides HASH_NAMESPACE)
#  2. Generates driver_config.hpp (included transitively by test code)

add_subdirectory(third_party/sparsehash)

# Determine if hash is in the tr1 namespace (used by string.hpp, address.hpp)
string(REPLACE "::" ";" HASH_NAMESPACE_LIST ${HASH_NAMESPACE})
foreach(NAMESPACE ${HASH_NAMESPACE_LIST})
  if(NAMESPACE STREQUAL "tr1")
    set(HASH_IN_TR1 1)
  endif()
endforeach()

set(HAVE_STD_ATOMIC ON)

# Generate the driver_config.hpp file
configure_file(
  ${CASS_ROOT_DIR}/driver_config.hpp.in
  ${CMAKE_CURRENT_SOURCE_DIR}/driver_config.hpp)

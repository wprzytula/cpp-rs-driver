include(CheckSymbolExists)
include(CheckCXXSourceCompiles)

# add_subdirectory(third_party/curl)
# add_subdirectory(third_party/hdr_histogram)
# add_subdirectory(third_party/http-parser)
# add_subdirectory(third_party/minizip)
add_subdirectory(third_party/sparsehash)

#------------------------------
# Build configured header
#------------------------------

# Determine random availability
if(CMAKE_SYSTEM_NAME MATCHES "Linux")
  check_symbol_exists(GRND_NONBLOCK "linux/random.h" HAVE_GETRANDOM)
else()
  check_symbol_exists(arc4random_buf "stdlib.h" HAVE_ARC4RANDOM)
endif()

# Determine if sigpipe is available
check_symbol_exists(SO_NOSIGPIPE "sys/socket.h;sys/types.h" HAVE_NOSIGPIPE)
check_symbol_exists(sigtimedwait "signal.h" HAVE_SIGTIMEDWAIT)
if (NOT WIN32 AND NOT HAVE_NOSIGPIPE AND NOT HAVE_SIGTIMEDWAIT)
  message(WARNING "Unable to handle SIGPIPE on your platform")
endif()

# Determine if hash is in the tr1 namespace
string(REPLACE "::" ";" HASH_NAMESPACE_LIST ${HASH_NAMESPACE})
foreach(NAMESPACE ${HASH_NAMESPACE_LIST})
  if(NAMESPACE STREQUAL "tr1")
    set(HASH_IN_TR1 1)
  endif()
endforeach()

# Check for GCC compiler builtins
if(NOT "${CMAKE_CXX_COMPILER_ID}" STREQUAL "MSVC")
  check_cxx_source_compiles("int main() { return __builtin_bswap32(42); }" HAVE_BUILTIN_BSWAP32)
  check_cxx_source_compiles("int main() { return __builtin_bswap64(42); }" HAVE_BUILTIN_BSWAP64)
endif()

set(HAVE_STD_ATOMIC ON)
set(HAVE_OPENSSL ${CASS_USE_OPENSSL})

# Generate the driver_config.hpp file
configure_file(
  ${CASS_ROOT_DIR}/driver_config.hpp.in 
  ${CMAKE_CURRENT_SOURCE_DIR}/driver_config.hpp)


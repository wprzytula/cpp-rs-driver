# add_subdirectory(third_party/curl)
# add_subdirectory(third_party/hdr_histogram)
# add_subdirectory(third_party/http-parser)
# add_subdirectory(third_party/minizip)
add_subdirectory(third_party/sparsehash)

#------------------------------
# Build configured header
#------------------------------

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

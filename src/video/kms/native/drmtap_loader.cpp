// libdrmtap, loaded at run time, and the display listing that needs only
// libdrmtap. The service links this file; the encoder library
// (libipkvm-kms.so, loaded only for video) carries its own copy, so the
// service starts and lists displays on a computer without VAAPI, EGL or GLES.

#include <dlfcn.h>
#include <stdlib.h>

#include <string>

#include "kms_capture.h"

#define LOG_MODULE "KMS"
#include "log.h"

namespace {

Drmtap load_drmtap() {
  Drmtap lib;
  // A packaged sender carries its own copy and names it in the environment;
  // otherwise the loader's search path finds a system install.
  const char *bundled = getenv("OS_SERVICES_DRMTAP_LIB");
  if (bundled && bundled[0])
    lib.handle = dlopen(bundled, RTLD_NOW | RTLD_LOCAL);
  if (!lib.handle)
    lib.handle = dlopen("libdrmtap.so.0", RTLD_NOW | RTLD_LOCAL);
  if (!lib.handle) {
    LOG_ERROR(std::string("libdrmtap.so.0 not loadable: ") + dlerror());
    return lib;
  }
  bool complete = true;
  auto resolve = [&](const char *name) {
    void *symbol = dlsym(lib.handle, name);
    if (!symbol) {
      LOG_ERROR(std::string("libdrmtap.so.0 lacks ") + name);
      complete = false;
    }
    return symbol;
  };
  auto version = (int (*)())resolve("drmtap_version");
  lib.open = (decltype(lib.open))resolve("drmtap_open");
  lib.close = (decltype(lib.close))resolve("drmtap_close");
  lib.list_displays = (decltype(lib.list_displays))resolve("drmtap_list_displays");
  lib.displays_changed = (decltype(lib.displays_changed))resolve("drmtap_displays_changed");
  lib.grab_desc = (decltype(lib.grab_desc))resolve("drmtap_grab_desc");
  lib.frame_release =
      (decltype(lib.frame_release))resolve("drmtap_frame_release");
  lib.get_cursor = (decltype(lib.get_cursor))resolve("drmtap_get_cursor");
  lib.cursor_release = (decltype(lib.cursor_release))resolve("drmtap_cursor_release");
  lib.error = (decltype(lib.error))resolve("drmtap_error");
  lib.render_node = (decltype(lib.render_node))resolve("drmtap_render_node");
  // The struct layouts come from the vendored header, so a library built
  // from another minor version would be read with the wrong offsets.
  if (complete) {
    int found = version();
    int wanted = (DRMTAP_VERSION_MAJOR << 16) | (DRMTAP_VERSION_MINOR << 8);
    if ((found & ~0xFF) != wanted) {
      LOG_ERROR("libdrmtap " + std::to_string((found >> 16) & 0xFF) + "." +
                std::to_string((found >> 8) & 0xFF) + " does not match the " +
                std::to_string(DRMTAP_VERSION_MAJOR) + "." +
                std::to_string(DRMTAP_VERSION_MINOR) + " this was built for");
      complete = false;
    }
  }
  if (!complete) {
    dlclose(lib.handle);
    lib.handle = nullptr;
  }
  return lib;
}

} // namespace

const Drmtap *drmtap() {
  static Drmtap lib = load_drmtap();
  return lib.handle ? &lib : nullptr;
}

void log_drmtap_error(const Drmtap *lib, drmtap_ctx *ctx, const char *what) {
  const char *why = lib->error(ctx);
  LOG_ERROR(std::string(what) + ": " + (why ? why : "unknown"));
}

extern "C" int kms_list_displays(const char *device, drmtap_display *out,
                                 int count) {
  const Drmtap *lib = drmtap();
  if (!lib)
    return -1;
  drmtap_config config = {};
  config.device_path = device;
  drmtap_ctx *ctx = lib->open(&config);
  if (!ctx)
    return -1;
  const int result = lib->list_displays(ctx, out, count);
  lib->close(ctx);
  return result;
}

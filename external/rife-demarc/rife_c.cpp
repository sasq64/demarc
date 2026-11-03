// C API over rife-ncnn-vulkan's RIFE class. Pixels are packed RGBA, as demarc
// keeps its frames.

#include <cstdint>
#include <cstring>
#include <vector>

#include "gpu.h"
#include "rife.h"

#if _WIN32
#define RIFE_API extern "C" __declspec(dllexport)
#else
#define RIFE_API extern "C" __attribute__((visibility("default")))
#endif

namespace {

struct Rife {
    RIFE rife;
    std::vector<unsigned char> a, b, out;
    explicit Rife(int gpu) : rife(gpu, false, false, false, 1, false, true) {}
};

// RIFE reads and writes BGR on Windows, RGB elsewhere.
void to_rgb(const uint32_t* src, size_t n, std::vector<unsigned char>& dst)
{
    dst.resize(n * 3);
    const unsigned char* s = reinterpret_cast<const unsigned char*>(src);
    for (size_t i = 0; i < n; i++) {
#if _WIN32
        dst[i * 3] = s[i * 4 + 2];
        dst[i * 3 + 1] = s[i * 4 + 1];
        dst[i * 3 + 2] = s[i * 4];
#else
        memcpy(&dst[i * 3], &s[i * 4], 3);
#endif
    }
}

} // namespace

RIFE_API void* rife_new(int gpu, const char* model_dir)
{
    ncnn::create_gpu_instance();
    if (gpu < 0)
        gpu = ncnn::get_default_gpu_index();
    if (gpu >= ncnn::get_gpu_count())
        return nullptr;
    auto* r = new Rife(gpu);
#if _WIN32
    std::string dir(model_dir);
    int err = r->rife.load(std::wstring(dir.begin(), dir.end()));
#else
    int err = r->rife.load(model_dir);
#endif
    if (err != 0) {
        delete r;
        return nullptr;
    }
    return r;
}

RIFE_API int rife_process(void* handle, const uint32_t* a, const uint32_t* b, int w, int h,
                          float t, uint32_t* out)
{
    auto* r = static_cast<Rife*>(handle);
    size_t n = size_t(w) * h;
    to_rgb(a, n, r->a);
    to_rgb(b, n, r->b);
    r->out.resize(n * 3);
    ncnn::Mat in0(w, h, r->a.data(), (size_t)3, 3);
    ncnn::Mat in1(w, h, r->b.data(), (size_t)3, 3);
    ncnn::Mat res(w, h, r->out.data(), (size_t)3, 3);
    int err = r->rife.process(in0, in1, t, res);
    if (err != 0)
        return err;
    unsigned char* d = reinterpret_cast<unsigned char*>(out);
    for (size_t i = 0; i < n; i++) {
#if _WIN32
        d[i * 4] = r->out[i * 3 + 2];
        d[i * 4 + 1] = r->out[i * 3 + 1];
        d[i * 4 + 2] = r->out[i * 3];
#else
        memcpy(&d[i * 4], &r->out[i * 3], 3);
#endif
        d[i * 4 + 3] = 0xff;
    }
    return 0;
}

RIFE_API void rife_free(void* handle)
{
    delete static_cast<Rife*>(handle);
}

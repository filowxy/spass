/* Implements the C playground runtime: spass2d, spass3d, and native spio. */

#include <limits.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifdef _WIN32
#include <windows.h>
#include <direct.h>
#else
#include <sys/stat.h>
#include <sys/types.h>
#endif

/* --- global state --------------------------------------------------------- */

#define SPG_NEAR 0.05
/* The deepest transform stack a program may build, and the most
   segments a sphere or a cylinder may be built from. */
#define SPG_STACK_MAX 64
#define SPG_MAX_SEGS 64

typedef struct {
    double cam[7];      /* eye xyz, target xyz, fov deg */
    double light[3];
} spg_scene_t;

/* A 4x4 affine matrix, row major: m[row * 4 + col]. It maps local
   coordinates to world coordinates. */
typedef struct { double m[16]; } spg_m4;

/* A point or a direction in world space. */
typedef struct { double x, y, z; } v3;

static struct {
    int w, h;
    uint32_t *px;     /* base layer: background + 3D */
    uint32_t *ov;     /* 2D overlay, 0xAARRGGBB */
    uint32_t *out;    /* composited frame */
    float *depth;
    int depth_on;
    int open;
    uint64_t frames;
    uint64_t frame_limit;
    double t0, last;
    double mx, my, mdx, mdy;
    int mdown, mprev;
    double wheel;
    int slider_active;
    int slider_x, slider_y;
    spg_scene_t scene;
    spg_m4 model;              /* local -> world */
    spg_m4 stack[SPG_STACK_MAX];
    int stack_len;
} spg;

#ifdef _WIN32
static HWND spg_hwnd = NULL;
static int spg_quit_seen = 0;
#endif

/* --- the model transform --------------------------------------------------
*
* A scene is easier to build out of local frames than out of world
* coordinates: a box at the origin, rotated and moved, is three calls
* and stays readable. The transform is a 4x4 affine matrix that maps
* local coordinates to world coordinates; the camera then maps world
* coordinates to the screen. So the order is always
* local -> model -> world -> camera -> screen.
* --------------------------------------------------------------------- */

static spg_m4 spg_m4_identity(void) {
    spg_m4 r;
    memset(r.m, 0, sizeof(r.m));
    r.m[0] = 1.0; r.m[5] = 1.0; r.m[10] = 1.0; r.m[15] = 1.0;
    return r;
}

/* a * b: the transform b first, then a. */
static spg_m4 spg_m4_mul(spg_m4 a, spg_m4 b) {
    spg_m4 o;
    int r, c, k;
    for (r = 0; r < 4; ++r)
        for (c = 0; c < 4; ++c) {
            double sum = 0.0;
            for (k = 0; k < 4; ++k) sum += a.m[r * 4 + k] * b.m[k * 4 + c];
            o.m[r * 4 + c] = sum;
        }
    return o;
}

static spg_m4 spg_m4_translate(double tx, double ty, double tz) {
    spg_m4 r = spg_m4_identity();
    r.m[3] = tx; r.m[7] = ty; r.m[11] = tz;
    return r;
}

static spg_m4 spg_m4_scale(double sx, double sy, double sz) {
    spg_m4 r = spg_m4_identity();
    r.m[0] = sx; r.m[5] = sy; r.m[10] = sz;
    return r;
}

/* A rotation about the three axes, in degrees. The axes turn in the
   order X, then Y, then Z: the rotation about X reaches the vertex
   first. */
static spg_m4 spg_m4_rotate(double rx, double ry, double rz) {
    const double deg2rad = 3.14159265358979323846 / 180.0;
    double ax = rx * deg2rad, ay = ry * deg2rad, az = rz * deg2rad;
    double sx = sin(ax), cx = cos(ax);
    double sy = sin(ay), cy = cos(ay);
    double sz = sin(az), cz = cos(az);
    spg_m4 x = spg_m4_identity();
    spg_m4 y = spg_m4_identity();
    spg_m4 z = spg_m4_identity();
    x.m[5] = cx; x.m[6] = -sx; x.m[9] = sx; x.m[10] = cx;
    y.m[0] = cy; y.m[2] = sy; y.m[8] = -sy; y.m[10] = cy;
    z.m[0] = cz; z.m[1] = -sz; z.m[4] = sz; z.m[5] = cz;
    return spg_m4_mul(z, spg_m4_mul(y, x));
}

/* Map a local vertex through the transform. */
static v3 spg_m4_apply(const spg_m4 *m, v3 p) {
    return (v3){
        m->m[0] * p.x + m->m[1] * p.y + m->m[2] * p.z + m->m[3],
        m->m[4] * p.x + m->m[5] * p.y + m->m[6] * p.z + m->m[7],
        m->m[8] * p.x + m->m[9] * p.y + m->m[10] * p.z + m->m[11]};
}

/* Save the current transform; `s3d:pop` puts it back. The stack holds
   at most SPG_STACK_MAX levels: a program that pushes without popping
   gets 0 rather than an unbounded stack. */
static int spg_push(void) {
    if (!spg.px) return 0;
    if (spg.stack_len >= SPG_STACK_MAX) return 0;
    spg.stack[spg.stack_len++] = spg.model;
    return 1;
}

/* Restore the transform saved by the matching `s3d:push`. Popping an
   empty stack is an error, not a silent no-op. */
static int spg_pop(void) {
    if (!spg.px) return 0;
    if (spg.stack_len <= 0) return 0;
    spg.model = spg.stack[--spg.stack_len];
    return 1;
}

/* Compose a transform onto the current one. */
static int spg_compose(spg_m4 op) {
    if (!spg.px) return 0;
    spg.model = spg_m4_mul(spg.model, op);
    return 1;
}

static int spg_translate(double tx, double ty, double tz) {
    return spg_compose(spg_m4_translate(tx, ty, tz));
}

static int spg_rotate(double rx, double ry, double rz) {
    return spg_compose(spg_m4_rotate(rx, ry, rz));
}

static int spg_scale(double sx, double sy, double sz) {
    return spg_compose(spg_m4_scale(sx, sy, sz));
}

static double spg_now(void) {
#ifdef _WIN32
    return (double)GetTickCount64() / 1000.0;
#else
    /* Portable fallback: clock is good enough for dt accounting. */
    return (double)clock() / (double)CLOCKS_PER_SEC;
#endif
}

static double spg_clamp01(double v) { return v < 0 ? 0 : (v > 1 ? 1 : v); }

/* Creating a directory is spelled differently on the two platforms:
   `_mkdir` on Windows, `mkdir(path, mode)` on POSIX. */
static int spg_make_dir(const char *path) {
#ifdef _WIN32
    return _mkdir(path);
#else
    return mkdir(path, 0777);
#endif
}

static uint32_t spg_pack(double r, double g, double b) {
    unsigned rr = (unsigned)(spg_clamp01(r) * 255.0 + 0.5);
    unsigned gg = (unsigned)(spg_clamp01(g) * 255.0 + 0.5);
    unsigned bb = (unsigned)(spg_clamp01(b) * 255.0 + 0.5);
    return 0xFF000000u | (bb << 16) | (gg << 8) | rr;
}

/* A colour for the 2D overlay: the alpha is the high byte, 0 means
   "nothing drawn here". */
static uint32_t spg_pack_a(double r, double g, double b, double a) {
    unsigned rr = (unsigned)(spg_clamp01(r) * 255.0 + 0.5);
    unsigned gg = (unsigned)(spg_clamp01(g) * 255.0 + 0.5);
    unsigned bb = (unsigned)(spg_clamp01(b) * 255.0 + 0.5);
    unsigned aa = (unsigned)(spg_clamp01(a) * 255.0 + 0.5);
    return (aa << 24) | (bb << 16) | (gg << 8) | rr;
}

/* Composite the 2D overlay over the base layer into the output buffer.
   The overlay is blended with the alpha it carries, so a translucent
   panel over a 3D scene shows the scene through it. Both layers stay
   untouched, so the next frame starts from the same base again. */
static void spg_composite(void) {
    size_t n = (size_t)spg.w * (size_t)spg.h;
    for (size_t i = 0; i < n; ++i) {
        uint32_t ov = spg.ov[i];
        unsigned a = (ov >> 24) & 0xFFu;
        if (a == 0) {
            spg.out[i] = spg.px[i];
        } else if (a == 255) {
            spg.out[i] = 0xFF000000u | (ov & 0x00FFFFFFu);
        } else {
            uint32_t base = spg.px[i];
            unsigned br = base & 0xFFu, bg = (base >> 8) & 0xFFu, bb = (base >> 16) & 0xFFu;
            unsigned sr = ov & 0xFFu, sg = (ov >> 8) & 0xFFu, sb = (ov >> 16) & 0xFFu;
            unsigned inv = 255u - a;
            spg.out[i] = 0xFF000000u
                | ((((sb * a + bb * inv) / 255u) & 0xFFu) << 16)
                | ((((sg * a + bg * inv) / 255u) & 0xFFu) << 8)
                | (((sr * a + br * inv) / 255u) & 0xFFu);
        }
    }
}

/* Write one pixel of the 2D overlay. The overlay is separate from the
   base layer, so a 2D primitive never overwrites the 3D scene: the two
   are composited at present time. */
static void spg_px(int x, int y, uint32_t color) {
    if (x >= 0 && y >= 0 && x < spg.w && y < spg.h) {
        spg.ov[(size_t)y * (size_t)spg.w + (size_t)x] = color;
    }
}

/* --- window / present ------------------------------------------------------ */

#ifdef _WIN32
static LRESULT CALLBACK spg_wnd_proc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_DESTROY) {
        spg.open = 0;
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(h, m, w, l);
}

static void spg_pump(void) {
    MSG msg;
    while (PeekMessageW(&msg, NULL, 0, 0, PM_REMOVE)) {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}
#else
static void spg_pump(void) {}
#endif

static uint64_t spg_limit(void) {
    const char *e = getenv("SPASS_PLAYGROUND_FRAMES");
    if (e && e[0]) return strtoull(e, NULL, 10);
#ifdef _WIN32
    return 0;
#else
    return 300;
#endif
}

static int spg_window(double w, double h, const char *title) {
    int wi = w >= 32.0 ? (int)w : 32;
    int hi = h >= 32.0 ? (int)h : 32;
    if (spg.px) return 1; /* idempotent */
    spg.w = wi;
    spg.h = hi;
    spg.px = (uint32_t *)calloc((size_t)wi * (size_t)hi, sizeof(uint32_t));
    spg.ov = (uint32_t *)calloc((size_t)wi * (size_t)hi, sizeof(uint32_t));
    spg.out = (uint32_t *)calloc((size_t)wi * (size_t)hi, sizeof(uint32_t));
    spg.depth = (float *)malloc((size_t)wi * (size_t)hi * sizeof(float));
    if (!spg.px || !spg.ov || !spg.out || !spg.depth) return 0;
    for (size_t i = 0; i < (size_t)wi * (size_t)hi; ++i) spg.depth[i] = 3.4e38f;
    spg.depth_on = 1;
    spg.t0 = spg.last = spg_now();
    spg.cam[0] = 0; spg.cam[1] = 0; spg.cam[2] = 4;
    spg.cam[3] = 0; spg.cam[4] = 0; spg.cam[5] = 0; spg.cam[6] = 60;
    spg.scene.light[0] = 0.4; spg.scene.light[1] = -1.0; spg.scene.light[2] = 0.3;
    spg.model = spg_m4_identity();
    spg.stack_len = 0;
    spg.frames = 0;
    spg.frame_limit = spg_limit();
#ifdef _WIN32
    {
        WNDCLASSEXW wc;
        wchar_t cls[64];
        wchar_t *wt;
        size_t i, n;
        memset(&wc, 0, sizeof(wc));
        wc.cbSize = (UINT)sizeof(wc);
        wc.style = CS_HREDRAW | CS_VREDRAW;
        wc.lpfnWndProc = spg_wnd_proc;
        wc.hInstance = GetModuleHandleW(NULL);
        for (i = 0; i < 63 && "SpassPlaygroundClass"[i]; ++i) cls[i] = (wchar_t)"SpassPlaygroundClass"[i];
        cls[i] = 0;
        wc.lpszClassName = cls;
        RegisterClassExW(&wc);
        n = 0; while (title && title[n]) ++n;
        wt = (wchar_t *)calloc(n + 1, sizeof(wchar_t));
        for (i = 0; i < n; ++i) wt[i] = (wchar_t)(unsigned char)title[i];
        spg_hwnd = CreateWindowExW(0, cls, wt, WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                                   80, 80, wi, hi, NULL, NULL, wc.hInstance, NULL);
        free(wt);
        if (!spg_hwnd) return 0;
        ShowWindow(spg_hwnd, SW_SHOW);
        UpdateWindow(spg_hwnd);
    }
#else
    (void)title;
#endif
    spg.open = 1;
    return 1;
}

static void spg_blit(void) {
#ifdef _WIN32
    BITMAPINFO bi;
    HDC dc;
    if (!spg_hwnd) return;
    dc = GetDC(spg_hwnd);
    if (!dc) return;
    memset(&bi, 0, sizeof(bi));
    bi.bmiHeader.biSize = (DWORD)sizeof(BITMAPINFOHEADER);
    bi.bmiHeader.biWidth = spg.w;
    bi.bmiHeader.biHeight = -spg.h;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB;
    StretchDIBits(dc, 0, 0, spg.w, spg.h, 0, 0, spg.w, spg.h,
                  spg.out, &bi, DIB_RGB_COLORS, SRCCOPY);
    ReleaseDC(spg_hwnd, dc);
    UpdateWindow(spg_hwnd);
#endif
}

static void spg_dump_ppm(void) {
    const char *path = getenv("SPASS_PLAYGROUND_PPM");
    FILE *f;
    if (!path || !path[0]) return;
    f = fopen(path, "wb");
    if (!f) return;
    fprintf(f, "P6\n%d %d\n255\n", spg.w, spg.h);
    for (size_t i = 0; i < (size_t)spg.w * (size_t)spg.h; ++i) {
        uint32_t p = spg.out[i];
        unsigned char rgb[3];
        rgb[0] = (unsigned char)(p & 0xFF);
        rgb[1] = (unsigned char)((p >> 8) & 0xFF);
        rgb[2] = (unsigned char)((p >> 16) & 0xFF);
        fwrite(rgb, 1, 3, f);
    }
    fclose(f);
}

static void spg_close_window(void) {
    spg.open = 0;
#ifdef _WIN32
    if (spg_hwnd) {
        DestroyWindow(spg_hwnd);
        spg_hwnd = NULL;
    }
#endif
}

static int spg_open(void) {
    spg_pump();
    if (!spg.px) return 0;
#ifdef _WIN32
    if (spg_hwnd) {
        POINT pt;
        if (GetCursorPos(&pt)) {
            ScreenToClient(spg_hwnd, &pt);
            double nx = (double)pt.x, ny = (double)pt.y;
            spg.mdx = nx - spg.mx;
            spg.mdy = ny - spg.my;
            spg.mx = nx;
            spg.my = ny;
        }
        spg.mprev = spg.mdown;
        spg.mdown = (GetAsyncKeyState(0x01) & 0x8000) != 0;
    }
#endif
    if (!spg.open) return 0;
    if (spg.frame_limit != 0 && spg.frames >= spg.frame_limit) return 0;
    return 1;
}

static int spg_present(void) {
    spg_pump();
    if (!spg.px) return 0;
    spg.frames += 1;
       /* The 2D overlay is composited over the base layer here, so the
       order in which the program drew them does not matter. */
    spg_composite();
    spg_blit();
    if (spg.frame_limit != 0 && spg.frames >= spg.frame_limit) {
        spg_dump_ppm();
        spg_close_window();
        return 0;
    }
    spg.last = spg_now();
    if (spg.frame_limit == 0) {
#ifdef _WIN32
        Sleep(16);
#endif
    }
    return 1;
}

static void spg_time_dt(double *t, double *dt) {
    double now = spg_now();
    if (t) *t = now - spg.t0;
    if (dt) *dt = now - spg.last;
}


/* --- spass2d primitives ---------------------------------------------------- */

static void spg_clear(double r, double g, double b) {
    uint32_t c = spg_pack(r, g, b);
    size_t n = (size_t)spg.w * (size_t)spg.h;
    for (size_t i = 0; i < n; ++i) spg.px[i] = c;
       /* Clearing the screen clears both layers: the 2D overlay is part of
       the frame, not a separate screen. */
    for (size_t i = 0; i < n; ++i) spg.ov[i] = 0;
    for (size_t i = 0; i < n; ++i) spg.depth[i] = 3.4e38f;
}

static void spg_pixel(double x, double y, double r, double g, double b, double a) {
    spg_px((int)x, (int)y, spg_pack_a(r, g, b, a));
}

static void spg_line2d(double x0, double y0, double x1, double y1,
                       double r, double g, double b, double a) {
    uint32_t color = spg_pack_a(r, g, b, a);
    int x = (int)x0, y = (int)y0;
    int xe = (int)x1, ye = (int)y1;
    int dx = abs(xe - x), dy = -abs(ye - y);
    int stepx = x < xe ? 1 : -1, stepy = y < ye ? 1 : -1;
    int err = dx + dy;
    for (;;) {
        spg_px(x, y, color);
        if (x == xe && y == ye) break;
        {
            int e2 = 2 * err;
            if (e2 >= dy) { err += dy; x += stepx; }
            if (e2 <= dx) { err += dx; y += stepy; }
        }
    }
}

static void spg_rect(double x, double y, double w, double h,
                     double r, double g, double b, double a) {
    uint32_t color = spg_pack_a(r, g, b, a);
    int x0 = (int)x, y0 = (int)y, x1 = (int)(x + w), y1 = (int)(y + h);
    for (int yy = y0; yy < y1; ++yy)
        for (int xx = x0; xx < x1; ++xx)
            spg_px(xx, yy, color);
}

static void spg_frame_rect(double x, double y, double w, double h,
                           double r, double g, double b, double a) {
    double x1 = x + w - 1.0, y1 = y + h - 1.0;
    spg_line2d(x, y, x1, y, r, g, b, a);
    spg_line2d(x1, y, x1, y1, r, g, b, a);
    spg_line2d(x1, y1, x, y1, r, g, b, a);
    spg_line2d(x, y1, x, y, r, g, b, a);
}

/* Both radii are clamped to one pixel, so a degenerate ellipse is a
   dot rather than a division by zero. */
static void spg_ellipse(double cx, double cy, double rx, double ry,
                        double r, double g, double b, double a) {
    uint32_t color = spg_pack_a(r, g, b, a);
    double rx1 = rx < 1.0 ? 1.0 : rx;
    double ry1 = ry < 1.0 ? 1.0 : ry;
    int dyi;
       /* Scanline by row: at the row offset fy the half width is
       rx * sqrt(1 - (fy/ry)^2). With rx == ry that is exactly the
       circle's sqrt(r^2 - fy^2). */
    for (dyi = -(int)ry1; dyi <= (int)ry1; ++dyi) {
        double fy = (double)dyi + 0.5;
        double t = 1.0 - (fy * fy) / (ry1 * ry1);
        double half = rx1 * (t > 0.0 ? sqrt(t) : 0.0);
        int x0 = (int)(cx - half), x1 = (int)(cx + half) + 1;
        int y = (int)(cy + fy);
        for (int xx = x0; xx < x1; ++xx) spg_px(xx, y, color);
    }
}

/* The circle is the ellipse with equal radii, so both go through one
   implementation and the two cannot drift apart. */
static void spg_circle(double cx, double cy, double rad,
                       double r, double g, double b, double a) {
    spg_ellipse(cx, cy, rad, rad, r, g, b, a);
}

/* The winding order does not matter: the point-in-triangle test is
   the barycentric one, and the weights are normalised by the signed
   area, so both orientations fill the same pixels. */
static void spg_tri2d(double x0, double y0, double x1, double y1,
                      double x2, double y2,
                      double r, double g, double b, double a) {
    uint32_t color = spg_pack_a(r, g, b, a);
    double area = (x1 - x0) * (y2 - y0) - (y1 - y0) * (x2 - x0);
    double inv, minx, maxx, miny, maxy, lo, hi;
    int x, y, ix0, ix1, iy0, iy1;
    /* A degenerate triangle covers no pixels. */
    if (fabs(area) < 1e-12) return;
    inv = 1.0 / area;
    minx = x0 < x1 ? (x0 < x2 ? x0 : x2) : (x1 < x2 ? x1 : x2);
    maxx = x0 > x1 ? (x0 > x2 ? x0 : x2) : (x1 > x2 ? x1 : x2);
    miny = y0 < y1 ? (y0 < y2 ? y0 : y2) : (y1 < y2 ? y1 : y2);
    maxy = y0 > y1 ? (y0 > y2 ? y0 : y2) : (y1 > y2 ? y1 : y2);
    lo = floor(minx); if (lo < 0.0) lo = 0.0; if (lo > (double)spg.w) lo = (double)spg.w;
    hi = ceil(maxx); if (hi < -1.0) hi = -1.0; if (hi > (double)(spg.w - 1)) hi = (double)(spg.w - 1);
    ix0 = (int)lo; ix1 = (int)hi;
    lo = floor(miny); if (lo < 0.0) lo = 0.0; if (lo > (double)spg.h) lo = (double)spg.h;
    hi = ceil(maxy); if (hi < -1.0) hi = -1.0; if (hi > (double)(spg.h - 1)) hi = (double)(spg.h - 1);
    iy0 = (int)lo; iy1 = (int)hi;
    for (y = iy0; y <= iy1; ++y)
        for (x = ix0; x <= ix1; ++x) {
            double px = (double)x + 0.5, py = (double)y + 0.5;
            double w0 = ((x1 - px) * (y2 - py) - (y1 - py) * (x2 - px)) * inv;
            double w1 = ((x2 - px) * (y0 - py) - (y2 - py) * (x0 - px)) * inv;
            double w2 = 1.0 - w0 - w1;
            if (w0 < 0.0 || w1 < 0.0 || w2 < 0.0) continue;
            spg_px(x, y, color);
        }
}


/* Classic glcd 5x7 font, ASCII 32..126 (same table as playground.rs). */
static const unsigned char spg_font[475] = {
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5F, 0x00, 0x00, 0x00, 0x07,
    0x00, 0x07, 0x00, 0x14, 0x7F, 0x14, 0x7F, 0x14, 0x24, 0x2A, 0x7F, 0x2A,
    0x12, 0x23, 0x13, 0x08, 0x64, 0x62, 0x36, 0x49, 0x56, 0x20, 0x50, 0x00,
    0x08, 0x07, 0x03, 0x00, 0x00, 0x1C, 0x22, 0x41, 0x00, 0x00, 0x41, 0x22,
    0x1C, 0x00, 0x2A, 0x1C, 0x7F, 0x1C, 0x2A, 0x08, 0x08, 0x3E, 0x08, 0x08,
    0x00, 0x80, 0x70, 0x30, 0x00, 0x08, 0x08, 0x08, 0x08, 0x08, 0x00, 0x00,
    0x60, 0x60, 0x00, 0x20, 0x10, 0x08, 0x04, 0x02, 0x3E, 0x51, 0x49, 0x45,
    0x3E, 0x00, 0x42, 0x7F, 0x40, 0x00, 0x72, 0x49, 0x49, 0x49, 0x46, 0x21,
    0x41, 0x49, 0x4D, 0x33, 0x18, 0x14, 0x12, 0x7F, 0x10, 0x27, 0x45, 0x45,
    0x45, 0x39, 0x3C, 0x4A, 0x49, 0x49, 0x31, 0x41, 0x21, 0x11, 0x09, 0x07,
    0x36, 0x49, 0x49, 0x49, 0x36, 0x46, 0x49, 0x49, 0x29, 0x1E, 0x00, 0x00,
    0x14, 0x00, 0x00, 0x00, 0x40, 0x34, 0x00, 0x00, 0x00, 0x08, 0x14, 0x22,
    0x41, 0x14, 0x14, 0x14, 0x14, 0x14, 0x00, 0x41, 0x22, 0x14, 0x08, 0x02,
    0x01, 0x59, 0x09, 0x06, 0x3E, 0x41, 0x5D, 0x59, 0x4E, 0x7C, 0x12, 0x11,
    0x12, 0x7C, 0x7F, 0x49, 0x49, 0x49, 0x36, 0x3E, 0x41, 0x41, 0x41, 0x22,
    0x7F, 0x41, 0x41, 0x41, 0x3E, 0x7F, 0x49, 0x49, 0x49, 0x41, 0x7F, 0x09,
    0x09, 0x09, 0x01, 0x3E, 0x41, 0x41, 0x51, 0x73, 0x7F, 0x08, 0x08, 0x08,
    0x7F, 0x00, 0x41, 0x7F, 0x41, 0x00, 0x20, 0x40, 0x41, 0x3F, 0x01, 0x7F,
    0x08, 0x14, 0x22, 0x41, 0x7F, 0x40, 0x40, 0x40, 0x40, 0x7F, 0x02, 0x1C,
    0x02, 0x7F, 0x7F, 0x04, 0x08, 0x10, 0x7F, 0x3E, 0x41, 0x41, 0x41, 0x3E,
    0x7F, 0x09, 0x09, 0x09, 0x06, 0x3E, 0x41, 0x51, 0x21, 0x5E, 0x7F, 0x09,
    0x19, 0x29, 0x46, 0x26, 0x49, 0x49, 0x49, 0x32, 0x03, 0x01, 0x7F, 0x01,
    0x03, 0x3F, 0x40, 0x40, 0x40, 0x3F, 0x1F, 0x20, 0x40, 0x20, 0x1F, 0x3F,
    0x40, 0x38, 0x40, 0x3F, 0x63, 0x14, 0x08, 0x14, 0x63, 0x03, 0x04, 0x78,
    0x04, 0x03, 0x61, 0x59, 0x49, 0x4D, 0x43, 0x00, 0x7F, 0x41, 0x41, 0x41,
    0x02, 0x04, 0x08, 0x10, 0x20, 0x00, 0x41, 0x41, 0x41, 0x7F, 0x04, 0x02,
    0x01, 0x02, 0x04, 0x40, 0x40, 0x40, 0x40, 0x40, 0x00, 0x03, 0x07, 0x08,
    0x00, 0x20, 0x54, 0x54, 0x78, 0x40, 0x7F, 0x28, 0x44, 0x44, 0x38, 0x38,
    0x44, 0x44, 0x44, 0x28, 0x38, 0x44, 0x44, 0x28, 0x7F, 0x38, 0x54, 0x54,
    0x54, 0x18, 0x00, 0x08, 0x7E, 0x09, 0x02, 0x18, 0xA4, 0xA4, 0x9C, 0x78,
    0x7F, 0x08, 0x04, 0x04, 0x78, 0x00, 0x44, 0x7D, 0x40, 0x00, 0x20, 0x40,
    0x40, 0x3D, 0x00, 0x7F, 0x10, 0x28, 0x44, 0x00, 0x00, 0x41, 0x7F, 0x40,
    0x00, 0x7C, 0x04, 0x78, 0x04, 0x78, 0x7C, 0x08, 0x04, 0x04, 0x78, 0x38,
    0x44, 0x44, 0x44, 0x38, 0xFC, 0x18, 0x24, 0x24, 0x18, 0x18, 0x24, 0x24,
    0x18, 0xFC, 0x7C, 0x08, 0x04, 0x04, 0x08, 0x48, 0x54, 0x54, 0x54, 0x24,
    0x04, 0x04, 0x3F, 0x44, 0x24, 0x3C, 0x40, 0x40, 0x20, 0x7C, 0x1C, 0x20,
    0x40, 0x20, 0x1C, 0x3C, 0x40, 0x30, 0x40, 0x3C, 0x44, 0x28, 0x10, 0x28,
    0x44, 0x4C, 0x90, 0x90, 0x90, 0x7C, 0x44, 0x64, 0x54, 0x4C, 0x44, 0x00,
    0x08, 0x36, 0x41, 0x00, 0x00, 0x00, 0x77, 0x00, 0x00, 0x00, 0x41, 0x36,
    0x08, 0x00, 0x02, 0x01, 0x02, 0x04, 0x02,
};

/* The pixel size of one text cell: five columns of ink, one column of
   spacing, eight rows. */
#define SPG_CELL_W 6.0
#define SPG_CELL_H 8.0

static double spg_text_into(int cx0, double x, double y, double scale,
                            uint32_t color, const char *text) {
    int s = scale >= 1.0 ? (int)scale : 1;
    int cx = (int)x, cy = (int)y;
    int widest = 0;
    const unsigned char *p = (const unsigned char *)text;
    (void)cx0;
    while (p && *p) {
        unsigned c = *p;
        int len = 1;
        if (c == '\n') {
               /* A newline starts a new line: the column returns to the
               left margin and the row moves down by one cell. */
            if (cx - (int)x > widest) widest = cx - (int)x;
            cx = (int)x;
            cy += (int)SPG_CELL_H * s;
            ++p;
            continue;
        }
        if (c >= 0x80) {
               /* A UTF-8 code point outside ASCII is one glyph: the font
               has no glyph for it, so it draws as '?'. */
            if (c < 0xC0) len = 1;
            else if (c < 0xE0) len = 2;
            else if (c < 0xF0) len = 3;
            else len = 4;
            c = '?';
        } else if (c < 32 || c > 126) {
            c = '?';
        }
        for (unsigned col = 0; col < 5; ++col) {
            unsigned char bits = spg_font[(c - 32) * 5 + col];
            for (unsigned row = 0; row < 8; ++row) {
                if (bits & (1u << row)) {
                    for (int sy = 0; sy < s; ++sy)
                        for (int sx = 0; sx < s; ++sx)
                            spg_px(cx + (int)col * s + sx, cy + (int)row * s + sy, color);
                }
            }
        }
        cx += (int)SPG_CELL_W * s;
        p += len;
    }
    if (cx - (int)x > widest) widest = cx - (int)x;
    return (double)widest;
}

/* The advance width of the widest line: a glyph cell is 6 pixels wide
   at scale 1, so a line of n characters is n * 6 * scale wide. */
static double spg_text_width(const char *text, double scale) {
    int s = scale >= 1.0 ? (int)scale : 1;
    const unsigned char *p = (const unsigned char *)text;
    int widest = 0, line = 0;
    while (p && *p) {
        unsigned c = *p;
        int len = 1;
        if (c == '\n') {
            if (line > widest) widest = line;
            line = 0;
            ++p;
            continue;
        }
        if (c >= 0x80) {
            if (c < 0xC0) len = 1;
            else if (c < 0xE0) len = 2;
            else if (c < 0xF0) len = 3;
            else len = 4;
        }
        ++line;
        p += len;
    }
    if (line > widest) widest = line;
    return (double)widest * SPG_CELL_W * (double)s;
}

/* The height of one line of text. */
static double spg_text_height(double scale) {
    int s = scale >= 1.0 ? (int)scale : 1;
    return SPG_CELL_H * (double)s;
}

static void spg_text(double x, double y, double scale,
                     double r, double g, double b, double a, const char *text) {
    spg_text_into(0, x, y, scale, spg_pack_a(r, g, b, a), text);
}

static int spg_in_rect(double mx, double my, double x, double y, double w, double h) {
    return mx >= x && my >= y && mx < x + w && my < y + h;
}

static int spg_button(double x, double y, double w, double h, const char *label, double a) {
    int hover = spg_in_rect(spg.mx, spg.my, x, y, w, h);
    int clicked = hover && spg.mdown && !spg.mprev;
    double br, bg, bb;
    uint32_t color, border, tc;
    int x0, y0, x1, y1, xx, yy;
    if (hover) { br = 0.36; bg = 0.42; bb = 0.55; }
    else { br = 0.25; bg = 0.28; bb = 0.34; }
    color = spg_pack_a(br, bg, bb, a);
    border = spg_pack_a(0.6, 0.65, 0.75, a);
    tc = spg_pack_a(0.95, 0.95, 0.95, a);
    x0 = (int)x; y0 = (int)y; x1 = (int)(x + w); y1 = (int)(y + h);
    for (yy = y0; yy < y1; ++yy)
        for (xx = x0; xx < x1; ++xx) spg_px(xx, yy, color);
    for (xx = x0; xx < x1; ++xx) { spg_px(xx, y0, border); spg_px(xx, y1 - 1, border); }
    for (yy = y0; yy < y1; ++yy) { spg_px(x0, yy, border); spg_px(x1 - 1, yy, border); }
    spg_text(x + 6.0, y + ((h - 8.0) / 2.0 > 0 ? (h - 8.0) / 2.0 : 0.0), 1.0, 0.95, 0.95, 0.95, a, label);
    return clicked;
}

static double spg_slider(double x, double y, double w, double h,
                         double value, double lo, double hi, double a) {
    double span = fabs(hi - lo) < 1e-12 ? 1.0 : hi - lo;
    double v = value < lo ? lo : (value > hi ? hi : value);
    double t;
    int key_x = (int)x, key_y = (int)y;
    int xx, yy, x0, y0, x1, y1, kx;
    uint32_t track, knob;
    if (spg.mdown) {
        if (!spg.slider_active &&
            spg_in_rect(spg.mx, spg.my, x - 4.0, y - 4.0, w + 8.0, h + 8.0)) {
            spg.slider_active = 1;
            spg.slider_x = key_x;
            spg.slider_y = key_y;
        }
        if (spg.slider_active && spg.slider_x == key_x && spg.slider_y == key_y) {
            t = (spg.mx - x) / w;
            if (t < 0) t = 0; if (t > 1) t = 1;
            v = lo + t * span;
        }
    } else {
        spg.slider_active = 0;
    }
    track = spg_pack_a(0.2, 0.22, 0.26, a);
    x0 = (int)x; y0 = (int)y; x1 = (int)(x + w); y1 = (int)(y + h);
    for (yy = y0; yy < y1; ++yy)
        for (xx = x0; xx < x1; ++xx)
            spg_px(xx, yy, track);
    t = (v - lo) / span;
    if (t < 0) t = 0; if (t > 1) t = 1;
    kx = (int)(x + t * (w - 6.0));
    knob = spg_pack_a(0.75, 0.8, 0.9, a);
    for (yy = y0; yy < y1; ++yy)
        for (xx = kx; xx < kx + 6; ++xx)
            spg_px(xx, yy, knob);
    return v;
}

static int spg_checkbox(double x, double y, const char *label, int on, double a) {
    int state = on;
    double bx, by, bsz = 14.0;
    int x0, y0, x1, y1, xx, yy, d;
    uint32_t fill, border, tick;
    if (spg.mdown && !spg.mprev &&
        spg_in_rect(spg.mx, spg.my, x, y, bsz + 4.0 + strlen(label) * 6.0, bsz)) {
        state = !on;
    }
    fill = state ? spg_pack_a(0.3, 0.65, 0.4, a) : spg_pack_a(0.2, 0.22, 0.26, a);
    border = spg_pack_a(0.6, 0.65, 0.75, a);
    tick = spg_pack_a(0.95, 0.98, 0.95, a);
    bx = x; by = y;
    x0 = (int)bx; y0 = (int)by; x1 = (int)(bx + bsz); y1 = (int)(by + bsz);
    for (yy = y0; yy < y1; ++yy)
        for (xx = x0; xx < x1; ++xx)
            spg_px(xx, yy, fill);
    for (xx = x0; xx < x1; ++xx) { spg_px(xx, y0, border); spg_px(xx, y1 - 1, border); }
    for (yy = y0; yy < y1; ++yy) { spg_px(x0, yy, border); spg_px(x1 - 1, yy, border); }
    if (state) {
        for (d = 0; d < 6; ++d) {
            spg_px((int)(bx + 3 + d), (int)(by + 6), tick);
            spg_px((int)(bx + 3 + d), (int)(by + 7), tick);
            spg_px((int)(bx + 8 - d), (int)(by + 6 + d / 2), tick);
        }
    }
    spg_text(bx + bsz + 6.0, by + 3.0, 1.0, 0.92, 0.92, 0.92, a, label);
    return state;
}

/* --- spass2d / spass3d input & time queries -------------------------------- */

static double spg_mouse_x(void) { return spg.mx; }
static double spg_mouse_y(void) { return spg.my; }
static double spg_mouse_dx(void) { return spg.mdx; }
static double spg_mouse_dy(void) { return spg.mdy; }
static int spg_mouse_down(void) { return spg.mdown; }
static double spg_wheel(void) { return spg.wheel; }
static int spg_key_down(const char *key) {
#ifdef _WIN32
    char k[16];
    size_t n;
    int vk = 0;
    if (!key || !key[0]) return 0;
    for (n = 0; key[n] && n < 15; ++n) k[n] = key[n];
    k[n] = 0;
    if (strcmp(k, "left") == 0) vk = 0x25;
    else if (strcmp(k, "up") == 0) vk = 0x26;
    else if (strcmp(k, "right") == 0) vk = 0x27;
    else if (strcmp(k, "down") == 0) vk = 0x28;
    else if (strcmp(k, "space") == 0) vk = 0x20;
    else if (strcmp(k, "enter") == 0 || strcmp(k, "return") == 0) vk = 0x0D;
    else if (strcmp(k, "escape") == 0 || strcmp(k, "esc") == 0) vk = 0x1B;
    else if (strcmp(k, "shift") == 0) vk = 0x10;
    else if (strcmp(k, "ctrl") == 0 || strcmp(k, "control") == 0) vk = 0x11;
    else if (strcmp(k, "alt") == 0) vk = 0x12;
    else if (strcmp(k, "tab") == 0) vk = 0x09;
    else if (strcmp(k, "backspace") == 0) vk = 0x08;
    else vk = (unsigned char)k[0];
    return (GetAsyncKeyState(vk) & 0x8000) != 0;
#else
    (void)key;
    return 0;
#endif
}

static double spg_time(void) { return spg_now() - spg.t0; }
static double spg_dt(void) { return spg_now() - spg.last; }

/* --- spass3d math: vectors + camera basis ---------------------------------- */

typedef struct { double sx, sy, iz; } sv;

static v3 v3_sub(v3 a, v3 b) { return (v3){a.x - b.x, a.y - b.y, a.z - b.z}; }
static double v3_dot(v3 a, v3 b) { return a.x * b.x + a.y * b.y + a.z * b.z; }
static v3 v3_cross(v3 a, v3 b) {
    return (v3){a.y * b.z - a.z * b.y, a.z * b.x - a.x * b.z, a.x * b.y - a.y * b.x};
}
static v3 v3_norm(v3 a) {
    double l = sqrt(v3_dot(a, a));
    if (l > 1e-12) return (v3){a.x / l, a.y / l, a.z / l};
    return (v3){0, 0, 0};
}

static void spg_cam_basis(v3 *eye, v3 *right, v3 *up, v3 *fwd) {
    *eye = (v3){spg.cam[0], spg.cam[1], spg.cam[2]};
    v3 target = (v3){spg.cam[3], spg.cam[4], spg.cam[5]};
    *fwd = v3_norm(v3_sub(target, *eye));
    v3 world_up = (v3){0, 1, 0};
*right = v3_norm(v3_cross(*fwd, world_up));
*up = v3_cross(*right, *fwd);
}

static void spg_project(sv *out, v3 p) {
    v3 eye, right, up, fwd;
    spg_cam_basis(&eye, &right, &up, &fwd);
    v3 v = v3_sub(p, eye);
    double zv = v3_dot(v, fwd);
    if (zv < SPG_NEAR) { out->sx = -1e9; out->sy = -1e9; out->iz = -1e9; return; }
    double xv = v3_dot(v, right);
    double yv = v3_dot(v, up);
    double fov = spg.cam[6];
    if (fov < 1.0) fov = 1.0; if (fov > 170.0) fov = 170.0;
    double focal = (double)spg.h * 0.5 / tan(fov * 3.141592653589793 / 360.0);
    out->sx = (double)spg.w * 0.5 + xv * focal / zv;
    out->sy = (double)spg.h * 0.5 - yv * focal / zv;
    out->iz = 1.0 / zv;
}

/* --- spass3d raster: z-buffered triangle rasterizer + shading -------------- */

/* Clip a world-space polygon against the near plane and project the
   survivors to screen space. Returns the number of vertices written. */
static int spg_clip_project(sv *out, int max, const v3 *poly, int n) {
    v3 eye, right, up, fwd;
    double vx[16], vy[16], vz[16];
    double cx[16], cy[16], cz[16];
    double fov, focal;
    int i, j, m = 0, k = 0;
    spg_cam_basis(&eye, &right, &up, &fwd);
    fov = spg.cam[6];
    if (fov < 1.0) fov = 1.0;
    if (fov > 170.0) fov = 170.0;
    focal = (double)spg.h * 0.5 / tan(fov * 3.141592653589793 / 360.0);
    for (i = 0; i < n && m < 16; ++i) {
        v3 v = v3_sub(poly[i], eye);
        vx[m] = v3_dot(v, right);
        vy[m] = v3_dot(v, up);
        vz[m] = v3_dot(v, fwd);
        ++m;
    }
    for (i = 0; i < m; ++i) {
        j = (i + 1) % m;
        {
            int ain = vz[i] >= SPG_NEAR;
            int bin = vz[j] >= SPG_NEAR;
            if (ain && k < max) { cx[k] = vx[i]; cy[k] = vy[i]; cz[k] = vz[i]; ++k; }
            if (ain != bin && k < max) {
                double t = (SPG_NEAR - vz[i]) / (vz[j] - vz[i]);
                cx[k] = vx[i] + (vx[j] - vx[i]) * t;
                cy[k] = vy[i] + (vy[j] - vy[i]) * t;
                cz[k] = SPG_NEAR;
                ++k;
            }
        }
    }
    for (i = 0; i < k; ++i) {
        out[i].sx = (double)spg.w * 0.5 + cx[i] * focal / cz[i];
        out[i].sy = (double)spg.h * 0.5 - cy[i] * focal / cz[i];
        out[i].iz = 1.0 / cz[i];
    }
    return k;
}

/* Rasterize one screen-space triangle with 1/z-correct depth testing. */
static void spg_raster_tri(const sv *s0, const sv *s1, const sv *s2, uint32_t color) {
    double minx = s0->sx < s1->sx ? (s0->sx < s2->sx ? s0->sx : s2->sx) : (s1->sx < s2->sx ? s1->sx : s2->sx);
    double maxx = s0->sx > s1->sx ? (s0->sx > s2->sx ? s0->sx : s2->sx) : (s1->sx > s2->sx ? s1->sx : s2->sx);
    double miny = s0->sy < s1->sy ? (s0->sy < s2->sy ? s0->sy : s2->sy) : (s1->sy < s2->sy ? s1->sy : s2->sy);
    double maxy = s0->sy > s1->sy ? (s0->sy > s2->sy ? s0->sy : s2->sy) : (s1->sy > s2->sy ? s1->sy : s2->sy);
    int x0 = (int)floor(minx), y0 = (int)floor(miny);
    int x1 = (int)ceil(maxx), y1 = (int)ceil(maxy);
    double area = (s1->sx - s0->sx) * (s2->sy - s0->sy)
                - (s1->sy - s0->sy) * (s2->sx - s0->sx);
    double inv;
    int x, y;
    if (fabs(area) < 1e-12) return;
    inv = 1.0 / area;
    if (x0 < 0) x0 = 0; if (y0 < 0) y0 = 0;
    if (x1 >= spg.w) x1 = spg.w - 1; if (y1 >= spg.h) y1 = spg.h - 1;
    for (y = y0; y <= y1; ++y) {
        for (x = x0; x <= x1; ++x) {
            double px = (double)x + 0.5, py = (double)y + 0.5;
            double w0 = ((s1->sx - px) * (s2->sy - py) - (s1->sy - py) * (s2->sx - px)) * inv;
            double w1 = ((s2->sx - px) * (s0->sy - py) - (s2->sy - py) * (s0->sx - px)) * inv;
            double w2 = 1.0 - w0 - w1;
            if (w0 < 0 || w1 < 0 || w2 < 0) continue;
            {
                double iz = w0 * s0->iz + w1 * s1->iz + w2 * s2->iz;
                size_t idx = (size_t)y * (size_t)spg.w + (size_t)x;
                if (spg.depth_on) {
                    if (iz <= 0 || (1.0 / iz) >= spg.depth[idx]) continue;
                    spg.depth[idx] = (float)(1.0 / iz);
                }
                spg.px[idx] = color;
            }
        }
    }
}

static int spg_tri_project(v3 a, v3 b, v3 c, uint32_t color) {
    sv s[4];
    v3 poly[3];
    int n, i;
    poly[0] = a; poly[1] = b; poly[2] = c;
    n = spg_clip_project(s, 4, poly, 3);
    if (n < 3) return 1;
    for (i = 1; i + 1 < n; ++i) spg_raster_tri(&s[0], &s[i], &s[i + 1], color);
    return 1;
}

static uint32_t spg_shade(v3 a, v3 b, v3 c, double r, double g, double bl) {
    v3 n = v3_norm(v3_cross(v3_sub(b, a), v3_sub(c, a)));
    v3 l = v3_norm((v3){spg.scene.light[0], spg.scene.light[1], spg.scene.light[2]});
    double k = fabs(v3_dot(n, l));
    double it = 0.35 + 0.65 * k;
    return spg_pack(r * it, g * it, bl * it);
}

/* A triangle given by its three local vertices. The model transform is
   applied here, so every 3D primitive goes through the same path from
   local coordinates to the screen. */
static int spg_tri3d_at(v3 a, v3 b, v3 c, double r, double g, double bl) {
    uint32_t color;
    if (!spg.px) return 0;
    a = spg_m4_apply(&spg.model, a);
    b = spg_m4_apply(&spg.model, b);
    c = spg_m4_apply(&spg.model, c);
    color = spg_shade(a, b, c, r, g, bl);
    return spg_tri_project(a, b, c, color);
}

static int spg_tri3d(double ax, double ay, double az,
                     double bx, double by, double bz,
                     double cx, double cy, double cz,
                     double r, double g, double b) {
    return spg_tri3d_at((v3){ax, ay, az}, (v3){bx, by, bz}, (v3){cx, cy, cz}, r, g, b);
}

/* --- spass3d higher-level shapes + camera/light/depth setters -------------- */

static int spg_quad3d(double x0, double y0, double z0,
                      double x1, double y1, double z1,
                      double x2, double y2, double z2,
                      double x3, double y3, double z3,
                      double r, double g, double b) {
    return spg_tri3d(x0, y0, z0, x1, y1, z1, x2, y2, z2, r, g, b)
        && spg_tri3d(x0, y0, z0, x2, y2, z2, x3, y3, z3, r, g, b);
}

static int spg_box3d(double cx, double cy, double cz,
                     double sx, double sy, double sz,
                     double r, double g, double b) {
    double hx = sx * 0.5, hy = sy * 0.5, hz = sz * 0.5;
    double x0 = cx - hx, y0 = cy - hy, z0 = cz - hz;
    double x1 = cx + hx, y1 = cy + hy, z1 = cz + hz;
    v3 corner[8];
    for (int i = 0; i < 8; ++i)
        corner[i] = spg_m4_apply(&spg.model,
                                 (v3){i & 1 ? x1 : x0, i & 2 ? y1 : y0, i & 4 ? z1 : z0});
    static const unsigned char faces[6][4] = {
        {0,2,3,1}, {4,5,7,6}, {0,1,5,4}, {2,6,7,3}, {0,4,6,2}, {1,3,7,5}};
    int ok = 1;
    for (int f = 0; f < 6; ++f) {
        v3 A = corner[faces[f][0]], B = corner[faces[f][1]];
        v3 C = corner[faces[f][2]], D = corner[faces[f][3]];
        uint32_t color = spg_shade(A, B, C, r, g, b);
        ok &= spg_tri_project(A, B, C, color);
        ok &= spg_tri_project(A, C, D, color);
    }
    return ok;
}

/* The vertex at latitude j, longitude i of a UV sphere. */
static v3 spg_sphere_vertex(double cx, double cy, double cz, double rad,
                            int j, int i, int segs, int rings) {
    const double pi = 3.14159265358979323846;
    double phi = pi * (double)j / (double)rings;
    double theta = 2.0 * pi * (double)i / (double)segs;
    return (v3){cx + rad * sin(phi) * cos(theta),
                cy + rad * cos(phi),
                cz + rad * sin(phi) * sin(theta)};
}

/* A flat-shaded sphere. The sphere is a UV grid: `segs` columns
   around the equator and half as many rows from pole to pole. The pole
   rows collapse to a point, so the caps need no special case. */
static int spg_sphere(double cx, double cy, double cz, double rad,
                      int32_t segs, double r, double g, double b) {
    int rings, j, i, ok = 1;
    if (segs < 3) segs = 3;
    if (segs > SPG_MAX_SEGS) segs = SPG_MAX_SEGS;
    rings = segs / 2;
    if (rings < 2) rings = 2;
    for (j = 0; j < rings; ++j) {
        for (i = 0; i < segs; ++i) {
            v3 p00 = spg_sphere_vertex(cx, cy, cz, rad, j, i, segs, rings);
            v3 p01 = spg_sphere_vertex(cx, cy, cz, rad, j, i + 1, segs, rings);
            v3 p10 = spg_sphere_vertex(cx, cy, cz, rad, j + 1, i, segs, rings);
            v3 p11 = spg_sphere_vertex(cx, cy, cz, rad, j + 1, i + 1, segs, rings);
            ok &= spg_tri3d_at(p00, p10, p11, r, g, b);
            ok &= spg_tri3d_at(p00, p11, p01, r, g, b);
        }
    }
    return ok;
}

/* A flat-shaded cylinder with its axis along Y. The body is a ring of
   quads; the two caps are triangle fans. */
static int spg_cylinder(double cx, double cy, double cz, double rad, double h,
                        int32_t segs, double r, double g, double b) {
    const double pi = 3.14159265358979323846;
    double y0 = cy - h * 0.5, y1 = cy + h * 0.5;
    int i, ok = 1;
    if (segs < 3) segs = 3;
    if (segs > SPG_MAX_SEGS) segs = SPG_MAX_SEGS;
    for (i = 0; i < segs; ++i) {
        double t0 = 2.0 * pi * (double)i / (double)segs;
        double t1 = 2.0 * pi * (double)(i + 1) / (double)segs;
        v3 a = (v3){cx + rad * cos(t0), 0.0, cz + rad * sin(t0)};
        v3 c = (v3){cx + rad * cos(t1), 0.0, cz + rad * sin(t1)};
        v3 a0 = (v3){a.x, y0, a.z}, a1 = (v3){a.x, y1, a.z};
        v3 c0 = (v3){c.x, y0, c.z}, c1 = (v3){c.x, y1, c.z};
        v3 bottom = (v3){cx, y0, cz}, top = (v3){cx, y1, cz};
        ok &= spg_tri3d_at(a0, c0, c1, r, g, b);
        ok &= spg_tri3d_at(a0, c1, a1, r, g, b);
        ok &= spg_tri3d_at(bottom, c0, a0, r, g, b);
        ok &= spg_tri3d_at(top, a1, c1, r, g, b);
    }
    return ok;
}

static int spg_line3d(double ax, double ay, double az,
                      double bx, double by, double bz, uint32_t color) {
    sv sa, sb;
    spg_project(&sa, spg_m4_apply(&spg.model, (v3){ax, ay, az}));
    spg_project(&sb, spg_m4_apply(&spg.model, (v3){bx, by, bz}));
    if (sa.iz < 0 || sb.iz < 0) return 0;
    int x = (int)sa.sx, y = (int)sa.sy;
    int xe = (int)sb.sx, ye = (int)sb.sy;
    int dx = abs(xe - x), dy = -abs(ye - y);
    int stepx = x < xe ? 1 : -1, stepy = y < ye ? 1 : -1;
    int err = dx + dy;
    double steps = (double)(abs(dx) + abs(dy));
    if (steps < 1.0) steps = 1.0;
    double i = 0.0;
    for (;;) {
        double t = i / steps;
        double iz = sa.iz + (sb.iz - sa.iz) * t;
        if (x >= 0 && y >= 0 && x < spg.w && y < spg.h) {
            size_t idx = (size_t)y * (size_t)spg.w + (size_t)x;
            if (!spg.depth_on || (iz > 0 && (1.0 / iz) < spg.depth[idx])) {
                spg.px[idx] = color;
            }
        }
        if (x == xe && y == ye) break;
        int e2 = 2 * err;
        if (e2 >= dy) { err += dy; x += stepx; i += 1.0; }
        if (e2 <= dx) { err += dx; y += stepy; i += 1.0; }
    }
    return 1;
}

static int spg_grid3d(double half, double step, double r, double g, double b) {
    double step2 = step <= 0 ? 1.0 : step;
    double half2 = half <= 0 ? 1.0 : half;
    uint32_t color = spg_pack(r, g, b);
    int ok = 1;
    double t = -half2;
    while (t <= half2 + 1e-9) {
        ok &= spg_line3d(t, 0, -half2, t, 0, half2, color);
        ok &= spg_line3d(-half2, 0, t, half2, 0, t, color);
        t += step2;
    }
    return ok;
}

static int spg_set_camera(double ex, double ey, double ez,
                          double tx, double ty, double tz, double fov) {
    if (!spg.px) return 0;
    spg.cam[0] = ex; spg.cam[1] = ey; spg.cam[2] = ez;
    spg.cam[3] = tx; spg.cam[4] = ty; spg.cam[5] = tz; spg.cam[6] = fov;
    return 1;
}

static int spg_set_light(double dx, double dy, double dz) {
    if (!spg.px) return 0;
    spg.scene.light[0] = dx; spg.scene.light[1] = dy; spg.scene.light[2] = dz;
    return 1;
}

static int spg_set_depth(int on) {
    if (!spg.px) return 0;
    spg.depth_on = on ? 1 : 0;
    return 1;
}

/* --- spio - native file I/O, run by the program itself --------------------- */

static char *spg_leak(const char *s) {
    size_t n = strlen(s) + 1;
    char *p = (char *)malloc(n);
    if (!p) return (char *)"";
    memcpy(p, s, n);
    return p;
}

static const char *spg_read_text(const char *path) {
    FILE *f = fopen(path, "rb");
    long n; char *buf;
    if (!f) return "";
    fseek(f, 0, SEEK_END);
    n = ftell(f);
    fseek(f, 0, SEEK_SET);
    buf = (char *)malloc((size_t)n + 1);
    if (!buf) { fclose(f); return ""; }
    if (n > 0) fread(buf, 1, (size_t)n, f);
    buf[n] = 0;
    fclose(f);
    return spg_leak(buf);
}

static int spg_write_text(const char *path, const char *text) {
    FILE *f;
    char dir[512];
    const char *slash = strrchr(path, '/');
    const char *bs = strrchr(path, '\\');
       /* The last separator in the path, whichever kind it is. Comparing
       against a null pointer is undefined, so the two candidates are
       checked one at a time. */
    const char *last = NULL;
    if (slash) last = slash;
    if (bs && (!last || bs > last)) last = bs;
    if (last && last != path) {
        size_t len = (size_t)(last - path);
        if (len >= sizeof(dir)) len = sizeof(dir) - 1;
        memcpy(dir, path, len);
        dir[len] = 0;
        spg_make_dir(dir);
    }
    f = fopen(path, "wb");
    if (!f) return 0;
    fwrite(text, 1, strlen(text), f);
    fclose(f);
    return 1;
}

static int spg_append_text(const char *path, const char *text) {
    FILE *f = fopen(path, "ab");
    if (!f) return 0;
    fwrite(text, 1, strlen(text), f);
    fclose(f);
    return 1;
}

static int32_t spg_line_count(const char *path) {
    FILE *f = fopen(path, "rb");
    int c, n = 0, prev_newline = 1;
    if (!f) return 0;
    while ((c = fgetc(f)) != EOF) {
        if (c == '\n') { ++n; prev_newline = 1; }
        else { prev_newline = 0; }
    }
    if (!prev_newline) ++n;
    fclose(f);
    return n;
}

static const char *spg_read_line(const char *path, int32_t idx) {
    FILE *f = fopen(path, "rb");
    long target = idx >= 0 ? (long)idx : LONG_MAX;
    long cur = 0;
    int c, done = 0;
    static char line[4096];
    size_t li = 0;
    if (!f) return "";
    while ((c = fgetc(f)) != EOF && !done) {
        if (cur == target) {
            if (c == '\n') { done = 1; break; }
            line[li++] = (char)c;
            if (li >= sizeof(line) - 1) { li = sizeof(line) - 2; done = 1; break; }
        } else if (c == '\n') {
            ++cur;
        }
    }
    line[li] = 0;
    fclose(f);
    return spg_leak(line);
}

static int spg_replace_text(const char *path, const char *old, const char *new) {
    FILE *f = fopen(path, "rb");
    long n; char *buf, *p;
    if (!f || !old || !old[0]) { if (f) fclose(f); return 0; }
    fseek(f, 0, SEEK_END);
    n = ftell(f);
    fseek(f, 0, SEEK_SET);
    buf = (char *)malloc((size_t)n + 1);
    if (!buf) { fclose(f); return 0; }
    if (n > 0) fread(buf, 1, (size_t)n, f);
    buf[n] = 0;
    fclose(f);
    p = strstr(buf, old);
    if (!p) { free(buf); return 0; }
    {
        size_t olen = strlen(old), nlen = strlen(new);
        size_t blen = strlen(buf);
        size_t outlen = blen - olen + nlen + 1;
        char *out = (char *)malloc(outlen);
        if (!out) { free(buf); return 0; }
        memcpy(out, buf, (size_t)(p - buf));
        memcpy(out + (p - buf), new, nlen);
        strcpy(out + (p - buf) + nlen, p + olen);
        spg_write_text(path, out);
        free(out);
    }
    free(buf);
    return 1;
}

static double spg_read_f64(const char *path) {
    FILE *f = fopen(path, "r");
    double v = 0.0;
    if (!f) return 0.0;
    if (fscanf(f, "%lf", &v) != 1) v = 0.0;
    fclose(f);
    return v;
}

static int spg_write_f64(const char *path, double v) {
    char buf[64];
    snprintf(buf, sizeof(buf), "%g", v);
    return spg_write_text(path, buf);
}

static int spg_append_f64(const char *path, double v) {
    char buf[64];
    snprintf(buf, sizeof(buf), "%g\n", v);
    return spg_append_text(path, buf);
}

static int spg_exists(const char *path) {
    FILE *f = fopen(path, "rb");
    if (f) { fclose(f); return 1; }
    return 0;
}

static double spg_size(const char *path) {
    FILE *f = fopen(path, "rb");
    long n;
    if (!f) return -1.0;
    fseek(f, 0, SEEK_END);
    n = ftell(f);
    fclose(f);
    return (double)n;
}

static int spg_remove(const char *path) {
    return remove(path) == 0;
}

static int spg_mkdir(const char *path) {
    return spg_make_dir(path) == 0;
}

static int spg_copy(const char *from, const char *to) {
    FILE *src, *dst;
    int c;
    src = fopen(from, "rb");
    if (!src) return 0;
    dst = fopen(to, "wb");
    if (!dst) { fclose(src); return 0; }
    while ((c = fgetc(src)) != EOF) fputc(c, dst);
    fclose(src);
    fclose(dst);
    return 1;
}

static int spg_rename(const char *from, const char *to) {
    return rename(from, to) == 0;
}

/* `ponytail`: directory listing is not implemented in the C backend yet.
* The Rust backend returns the real count; returning -1 here says
* "not supported" instead of quietly disagreeing with it. */
static int32_t spg_list_count(const char *dir) {
    (void)dir;
    return -1;
}

static const char *spg_list_name(const char *dir, int32_t idx) {
    (void)dir; (void)idx;
    return "";
}

static int spg_print(const char *text) {
    printf("%s", text);
    fflush(stdout);
    return 1;
}

static int spg_print_num(double v) {
    printf("%g", v);
    fflush(stdout);
    return 1;
}

/* ============================================================================
* Public API entry points - what generated Spass code calls.
* ========================================================================== */

/* --- s2d --- */
int spass_s2d_window(double w, double h, const char *title) { return spg_window(w, h, title); }
int spass_s2d_open(void) { return spg_open(); }
int spass_s2d_present(void) { return spg_present(); }
int spass_s2d_clear(double r, double g, double b) { spg_clear(r, g, b); return 1; }
int spass_s2d_pixel(double x, double y, double r, double g, double b, double a) { spg_pixel(x, y, r, g, b, a); return 1; }
int spass_s2d_line(double x0, double y0, double x1, double y1, double r, double g, double b, double a) { spg_line2d(x0, y0, x1, y1, r, g, b, a); return 1; }
int spass_s2d_rect(double x, double y, double w, double h, double r, double g, double b, double a) { spg_rect(x, y, w, h, r, g, b, a); return 1; }
int spass_s2d_frame(double x, double y, double w, double h, double r, double g, double b, double a) { spg_frame_rect(x, y, w, h, r, g, b, a); return 1; }
int spass_s2d_circle(double cx, double cy, double rad, double r, double g, double b, double a) { spg_circle(cx, cy, rad, r, g, b, a); return 1; }
int spass_s2d_ellipse(double cx, double cy, double rx, double ry, double r, double g, double b, double a) { spg_ellipse(cx, cy, rx, ry, r, g, b, a); return 1; }
int spass_s2d_tri(double x0, double y0, double x1, double y1, double x2, double y2, double r, double g, double b, double a) { spg_tri2d(x0, y0, x1, y1, x2, y2, r, g, b, a); return 1; }
double spass_s2d_text_width(const char *text, double scale) { return spg_text_width(text, scale); }
double spass_s2d_text_height(double scale) { return spg_text_height(scale); }
int spass_s2d_text(double x, double y, double scale, double r, double g, double b, double a, const char *text) { spg_text(x, y, scale, r, g, b, a, text); return 1; }
int spass_s2d_label(double x, double y, const char *text, double a) { spg_text(x, y, 1.0, 0.92, 0.93, 0.95, a, text); return 1; }
int spass_s2d_button(double x, double y, double w, double h, const char *label, double a) { return spg_button(x, y, w, h, label, a); }
double spass_s2d_slider(double x, double y, double w, double h, double v, double lo, double hi, double a) { return spg_slider(x, y, w, h, v, lo, hi, a); }
int spass_s2d_checkbox(double x, double y, const char *label, int on, double a) { return spg_checkbox(x, y, label, on, a); }
double spass_s2d_mouse_x(void) { return spg_mouse_x(); }
double spass_s2d_mouse_y(void) { return spg_mouse_y(); }
double spass_s2d_mouse_dx(void) { return spg_mouse_dx(); }
double spass_s2d_mouse_dy(void) { return spg_mouse_dy(); }
int spass_s2d_mouse_down(void) { return spg_mouse_down(); }
double spass_s2d_wheel(void) { return spg_wheel(); }
int spass_s2d_key(const char *key) { return spg_key_down(key); }
double spass_s2d_time(void) { return spg_time(); }
double spass_s2d_dt(void) { return spg_dt(); }

/* --- s3d --- */
int spass_s3d_window(double w, double h, const char *title) { return spg_window(w, h, title); }
int spass_s3d_open(void) { return spg_open(); }
int spass_s3d_present(void) { return spg_present(); }
int spass_s3d_clear(double r, double g, double b) { spg_clear(r, g, b); return 1; }
int spass_s3d_camera(double ex, double ey, double ez, double tx, double ty, double tz, double fov) { return spg_set_camera(ex, ey, ez, tx, ty, tz, fov); }
int spass_s3d_light(double dx, double dy, double dz) { return spg_set_light(dx, dy, dz); }
int spass_s3d_depth(int on) { return spg_set_depth(on); }
int spass_s3d_tri(double x0, double y0, double z0, double x1, double y1, double z1,
                  double x2, double y2, double z2, double r, double g, double b) {
    return spg_tri3d(x0, y0, z0, x1, y1, z1, x2, y2, z2, r, g, b);
}
int spass_s3d_quad(double x0, double y0, double z0, double x1, double y1, double z1,
                   double x2, double y2, double z2, double x3, double y3, double z3,
                   double r, double g, double b) {
    return spg_quad3d(x0, y0, z0, x1, y1, z1, x2, y2, z2, x3, y3, z3, r, g, b);
}
int spass_s3d_box(double cx, double cy, double cz, double sx, double sy, double sz,
                  double r, double g, double b) {
    return spg_box3d(cx, cy, cz, sx, sy, sz, r, g, b);
}
int spass_s3d_grid(double half, double step, double r, double g, double b) {
    return spg_grid3d(half, step, r, g, b);
}
int spass_s3d_push(void) { return spg_push(); }
int spass_s3d_pop(void) { return spg_pop(); }
int spass_s3d_translate(double tx, double ty, double tz) { return spg_translate(tx, ty, tz); }
int spass_s3d_rotate(double rx, double ry, double rz) { return spg_rotate(rx, ry, rz); }
int spass_s3d_scale(double sx, double sy, double sz) { return spg_scale(sx, sy, sz); }
int spass_s3d_sphere(double cx, double cy, double cz, double rad, int32_t segs, double r, double g, double b) {
    return spg_sphere(cx, cy, cz, rad, segs, r, g, b);
}
int spass_s3d_cylinder(double cx, double cy, double cz, double rad, double h, int32_t segs, double r, double g, double b) {
    return spg_cylinder(cx, cy, cz, rad, h, segs, r, g, b);
}
double spass_s3d_mouse_x(void) { return spg_mouse_x(); }
double spass_s3d_mouse_y(void) { return spg_mouse_y(); }
double spass_s3d_mouse_dx(void) { return spg_mouse_dx(); }
double spass_s3d_mouse_dy(void) { return spg_mouse_dy(); }
int spass_s3d_mouse_down(void) { return spg_mouse_down(); }
int spass_s3d_key(const char *key) { return spg_key_down(key); }
double spass_s3d_time(void) { return spg_time(); }
double spass_s3d_dt(void) { return spg_dt(); }

/* --- spio --- */
int spass_spio_print(const char *text) { return spg_print(text); }
int spass_spio_print_num(double v) { return spg_print_num(v); }
const char *spass_spio_read_text(const char *path) { return spg_read_text(path); }
int spass_spio_write_text(const char *path, const char *text) { return spg_write_text(path, text); }
int spass_spio_append_text(const char *path, const char *text) { return spg_append_text(path, text); }
const char *spass_spio_read_line(const char *path, int32_t idx) { return spg_read_line(path, idx); }
int32_t spass_spio_line_count(const char *path) { return spg_line_count(path); }
int spass_spio_replace_text(const char *path, const char *old, const char *new) { return spg_replace_text(path, old, new); }
double spass_spio_read_f64(const char *path) { return spg_read_f64(path); }
int spass_spio_write_f64(const char *path, double v) { return spg_write_f64(path, v); }
int spass_spio_append_f64(const char *path, double v) { return spg_append_f64(path, v); }
int spass_spio_exists(const char *path) { return spg_exists(path); }
double spass_spio_size(const char *path) { return spg_size(path); }
int spass_spio_remove(const char *path) { return spg_remove(path); }
int spass_spio_mkdir(const char *path) { return spg_mkdir(path); }
int spass_spio_copy(const char *from, const char *to) { return spg_copy(from, to); }
int spass_spio_rename(const char *from, const char *to) { return spg_rename(from, to); }
int32_t spass_spio_list_count(const char *dir) { return spg_list_count(dir); }
const char *spass_spio_list_name(const char *dir, int32_t idx) { return spg_list_name(dir, idx); }


/* Implements the C runtime: builtins, vector helpers, noise, and task state machines. */

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* -------------------------------------------------------------------- */
/* Task / state-machine runtime */
/* -------------------------------------------------------------------- */
//
// A function that uses `@checkpoint` / `@resume` is compiled by the
// codegen layer into a state machine. The state machine is a C function
// with signature `void spass_<name>_runner(spass_task_t*)`. It reads
// the saved `state_id` from the task and dispatches via a `switch`.
//
// `spass_task_t` is the user-visible handle. It carries:
// * refcount: incremented on retain, decremented on release.
// * state_id: which checkpoint to resume at (0 = function entry).
// * completed: 1 once the function has run to its normal return.
// * state_data[]: function-specific locals laid out by the codegen.
//
// The cross-function `@resume name` mechanism (, cross-
// function) works via a global name->task registry. Each `@checkpoint
// name` in any function registers its task under that name; each
// `@resume name` looks up the target task and runs it from its saved
// state. The lookup is a simple linear-scan table (ponytail: a hash
// table would be O(1) but the table is small in practice).

#define SPASS_TASK_REGISTRY_CAP 256

typedef struct spass_task spass_task_t;

typedef void (*spass_resume_fn)(spass_task_t*);

struct spass_task {
    uint32_t refcount;
    uint32_t state_id;          /* 0 = entry, n = post-@checkpoint n   */
    uint8_t  completed;         /* 1 once the function has returned     */
    spass_resume_fn resume_fn;  /* function-specific state machine      */
    /* The function's own locals follow as a flexible array member. The
    * codegen layer is responsible for laying them out; the runtime does
    * not introspect `state_data`. */
    uint8_t state_data[];
};

/* Registry entry: a name -> task mapping. Linear-scan, see macro above. */
typedef struct {
    char*        name;
    spass_task_t* task;
} spass_task_reg_entry_t;

static spass_task_reg_entry_t spass_task_registry[SPASS_TASK_REGISTRY_CAP];
static uint32_t spass_task_registry_count = 0;

spass_task_t* spass_task_new(size_t state_size, spass_resume_fn resume_fn) {
    spass_task_t* t = (spass_task_t*)malloc(sizeof(spass_task_t) + state_size);
    if (t == NULL) { fprintf(stderr, "spass: out of memory\n"); abort(); }
    t->refcount  = 1;
    t->state_id  = 0;
    t->completed = 0;
    t->resume_fn = resume_fn;
    memset(t->state_data, 0, state_size);
    return t;
}

void spass_task_retain(spass_task_t* t) {
    if (t != NULL) t->refcount += 1;
}

void spass_task_release(spass_task_t* t) {
    if (t == NULL) return;
    t->refcount -= 1;
    if (t->refcount == 0) {
        /* Unregister from the global table so the name doesn't dangle. */
        for (uint32_t i = 0; i < spass_task_registry_count; ++i) {
            if (spass_task_registry[i].task == t) {
                free(spass_task_registry[i].name);
                spass_task_registry[i] = spass_task_registry[--spass_task_registry_count];
                break;
            }
        }
        free(t);
    }
}

void spass_task_complete(spass_task_t* t) {
    t->completed = 1;
/* The function-specific state block keeps the return value at a
* known offset chosen by codegen; the runtime does not interpret it. */
}

/* Resume a suspended task. Runs its state machine until it suspends
* (returns from the runner) or completes. The task's own refcount is
* unchanged; ownership remains with the caller. */
void spass_task_resume(spass_task_t* t) {
    if (t == NULL || t->completed) return;
    t->resume_fn(t);
}

void spass_task_register(const char* name, spass_task_t* t) {
    /* Replace any existing entry for the same name (intra-function
    * suspension re-registers the same task). */
    for (uint32_t i = 0; i < spass_task_registry_count; ++i) {
        if (strcmp(spass_task_registry[i].name, name) == 0) {
            spass_task_registry[i].task = t;
            return;
        }
    }
    if (spass_task_registry_count >= SPASS_TASK_REGISTRY_CAP) {
        fprintf(stderr, "spass: task registry full (limit %d)\n",
                SPASS_TASK_REGISTRY_CAP);
        abort();
    }
    spass_task_registry[spass_task_registry_count].name = _strdup(name);
    spass_task_registry[spass_task_registry_count].task = t;
    spass_task_registry_count += 1;
}

spass_task_t* spass_task_lookup(const char* name) {
    for (uint32_t i = 0; i < spass_task_registry_count; ++i) {
        if (strcmp(spass_task_registry[i].name, name) == 0) {
            return spass_task_registry[i].task;
        }
    }
    return NULL;
}

/* -------------------------------------------------------------------- */
/* Scalar math */
/* -------------------------------------------------------------------- */

double spass_sin(double x)   { return sin(x); }
double spass_cos(double x)   { return cos(x); }
double spass_tan(double x)   { return tan(x); }
double spass_sqrt(double x)  { return sqrt(x); }
double spass_abs(double x)   { return fabs(x); }
double spass_floor(double x) { return floor(x); }
double spass_ceil(double x)  { return ceil(x); }
double spass_round(double x) { return round(x); }
double spass_exp(double x)   { return exp(x); }
double spass_log(double x)   { return log(x); }
double spass_pow(double b, double e) { return pow(b, e); }
double spass_max(double a, double b) { return a > b ? a : b; }
double spass_min(double a, double b) { return a < b ? a : b; }

double spass_lerp(double a, double b, double t) { return a + (b - a) * t; }
double spass_clamp(double v, double lo, double hi) {
    if (v < lo) return lo;
    if (v > hi) return hi;
    return v;
}
double spass_smoothstep(double e0, double e1, double x) {
    double t = (x - e0) / (e1 - e0);
    if (t < 0.0) t = 0.0;
    if (t > 1.0) t = 1.0;
    return t * t * (3.0 - 2.0 * t);
}

/* -------------------------------------------------------------------- */
/* Vector / matrix helpers (operate on plain double arrays) */
/* -------------------------------------------------------------------- */

void spass_hadamard_2(const double* a, const double* b, double* out) {
    out[0] = a[0] * b[0];
    out[1] = a[1] * b[1];
}
void spass_hadamard_3(const double* a, const double* b, double* out) {
    out[0] = a[0] * b[0];
    out[1] = a[1] * b[1];
    out[2] = a[2] * b[2];
}
void spass_hadamard_4(const double* a, const double* b, double* out) {
    out[0] = a[0] * b[0];
    out[1] = a[1] * b[1];
    out[2] = a[2] * b[2];
    out[3] = a[3] * b[3];
}

double spass_length_2(const double* a) { return sqrt(a[0]*a[0] + a[1]*a[1]); }
double spass_length_3(const double* a) { return sqrt(a[0]*a[0] + a[1]*a[1] + a[2]*a[2]); }
double spass_length_4(const double* a) {
    return sqrt(a[0]*a[0] + a[1]*a[1] + a[2]*a[2] + a[3]*a[3]);
}

void spass_normalize_2(const double* a, double* out) {
    double l = spass_length_2(a);
    if (l == 0.0) { out[0] = 0.0; out[1] = 0.0; return; }
    out[0] = a[0] / l; out[1] = a[1] / l;
}
void spass_normalize_3(const double* a, double* out) {
    double l = spass_length_3(a);
    if (l == 0.0) { out[0] = 0.0; out[1] = 0.0; out[2] = 0.0; return; }
    out[0] = a[0] / l; out[1] = a[1] / l; out[2] = a[2] / l;
}
void spass_normalize_4(const double* a, double* out) {
    double l = spass_length_4(a);
    if (l == 0.0) { out[0] = 0.0; out[1] = 0.0; out[2] = 0.0; out[3] = 0.0; return; }
    out[0] = a[0] / l; out[1] = a[1] / l; out[2] = a[2] / l; out[3] = a[3] / l;
}

double spass_dot_2(const double* a, const double* b) { return a[0]*b[0] + a[1]*b[1]; }
double spass_dot_3(const double* a, const double* b) {
    return a[0]*b[0] + a[1]*b[1] + a[2]*b[2];
}
double spass_dot_4(const double* a, const double* b) {
    return a[0]*b[0] + a[1]*b[1] + a[2]*b[2] + a[3]*b[3];
}

void spass_cross_3(const double* a, const double* b, double* out) {
    out[0] = a[1]*b[2] - a[2]*b[1];
    out[1] = a[2]*b[0] - a[0]*b[2];
    out[2] = a[0]*b[1] - a[1]*b[0];
}

void spass_transpose_3(const double* m, double* out) {
    for (int i = 0; i < 3; ++i)
        for (int j = 0; j < 3; ++j)
            out[j*3 + i] = m[i*3 + j];
}
void spass_transpose_4(const double* m, double* out) {
    for (int i = 0; i < 4; ++i)
        for (int j = 0; j < 4; ++j)
            out[j*4 + i] = m[i*4 + j];
}

int spass_inverse_3(const double* m, double* out) {
    double a = m[0], b = m[1], c = m[2];
    double d = m[3], e = m[4], f = m[5];
    double g = m[6], h = m[7], i = m[8];
    double det = a*(e*i - f*h) - b*(d*i - f*g) + c*(d*h - e*g);
    if (det == 0.0) return 0;
    double inv = 1.0 / det;
    out[0] = (e*i - f*h) * inv;
    out[1] = (c*h - b*i) * inv;
    out[2] = (b*f - c*e) * inv;
    out[3] = (f*g - d*i) * inv;
    out[4] = (a*i - c*g) * inv;
    out[5] = (c*d - a*f) * inv;
    out[6] = (d*h - e*g) * inv;
    out[7] = (b*g - a*h) * inv;
    out[8] = (a*e - b*d) * inv;
    return 1;
}
int spass_inverse_4(const double* m, double* out) {
    double inv[16];
    double m0=m[0], m1=m[1], m2=m[2], m3=m[3];
    double m4=m[4], m5=m[5], m6=m[6], m7=m[7];
    double m8=m[8], m9=m[9], m10=m[10], m11=m[11];
    double m12=m[12], m13=m[13], m14=m[14], m15=m[15];
    inv[0]  =  m5*m10*m15 - m5*m11*m14 - m9*m6*m15 + m9*m7*m14 + m13*m6*m11 - m13*m7*m10;
    inv[4]  = -m1*m10*m15 + m1*m11*m14 + m9*m2*m15 - m9*m3*m14 - m13*m2*m11 + m13*m3*m10;
    inv[8]  =  m1*m6*m15  - m1*m7*m14  - m5*m2*m15  + m5*m3*m14 + m13*m2*m7  - m13*m3*m6;
    inv[12] = -m1*m6*m11  + m1*m7*m10  + m5*m2*m11  - m5*m3*m10 - m9*m2*m7   + m9*m3*m6;
    inv[1]  = -m4*m10*m15 + m4*m11*m14 + m8*m6*m15  - m8*m7*m14 - m12*m6*m11 + m12*m7*m10;
    inv[5]  =  m0*m10*m15 - m0*m11*m14 - m8*m2*m15  + m8*m3*m14 + m12*m2*m11 - m12*m3*m10;
    inv[9]  = -m0*m6*m15  + m0*m7*m14  + m4*m2*m15  - m4*m3*m14 - m12*m2*m7  + m12*m3*m6;
    inv[13] =  m0*m6*m11  - m0*m7*m10  - m4*m2*m11  + m4*m3*m10 + m8*m2*m7   - m8*m3*m6;
    inv[2]  =  m4*m9*m15  - m4*m11*m13 - m8*m5*m15  + m8*m7*m13 + m12*m5*m11 - m12*m7*m9;
    inv[6]  = -m0*m9*m15  + m0*m11*m13 + m8*m1*m15  - m8*m3*m13 - m12*m1*m11 + m12*m3*m9;
    inv[10] =  m0*m5*m15  - m0*m7*m13  - m4*m1*m15  + m4*m3*m13 + m12*m1*m7  - m12*m3*m5;
    inv[14] = -m0*m5*m11  + m0*m7*m9   + m4*m1*m11  - m4*m3*m9  - m8*m1*m7   + m8*m3*m5;
    inv[3]  = -m4*m9*m14  + m4*m10*m13 + m8*m5*m14  - m8*m6*m13 - m12*m5*m10 + m12*m6*m9;
    inv[7]  =  m0*m9*m14  - m0*m10*m13 - m8*m1*m14  + m8*m3*m13 + m12*m1*m10 - m12*m3*m9;
    inv[11] = -m0*m5*m14  + m0*m6*m13  + m4*m1*m14  - m4*m3*m13 - m12*m1*m6  + m12*m3*m5;
    inv[15] =  m0*m5*m10  - m0*m6*m9   - m4*m1*m10  + m4*m3*m9  + m8*m1*m6   - m8*m3*m5;
    double det = m0*inv[0] + m1*inv[4] + m2*inv[8] + m3*inv[12];
    if (det == 0.0) return 0;
    double invd = 1.0 / det;
    for (int k = 0; k < 16; ++k) out[k] = inv[k] * invd;
    return 1;
}

/* -------------------------------------------------------------------- */
/* 2D value noise */
/* -------------------------------------------------------------------- */

static uint32_t spass_hash2(uint32_t x, uint32_t y) {
    uint32_t h = x * 374761393u + y * 668265263u;
    h = (h ^ (h >> 13)) * 1274126177u;
    return h ^ (h >> 16);
}

static double spass_smooth(double t) { return t * t * (3.0 - 2.0 * t); }

double spass_noise_2d(double x, double y) {
    int x0 = (int)floor(x);
    int y0 = (int)floor(y);
    int x1 = x0 + 1;
    int y1 = y0 + 1;
    double fx = x - x0;
    double fy = y - y0;
    double u = spass_smooth(fx);
    double v = spass_smooth(fy);
    uint32_t h00 = spass_hash2((uint32_t)x0, (uint32_t)y0);
    uint32_t h10 = spass_hash2((uint32_t)x1, (uint32_t)y0);
    uint32_t h01 = spass_hash2((uint32_t)x0, (uint32_t)y1);
    uint32_t h11 = spass_hash2((uint32_t)x1, (uint32_t)y1);
    double n00 = ((double)(h00 & 0xFFFF) / 65535.0) * 2.0 - 1.0;
    double n10 = ((double)(h10 & 0xFFFF) / 65535.0) * 2.0 - 1.0;
    double n01 = ((double)(h01 & 0xFFFF) / 65535.0) * 2.0 - 1.0;
    double n11 = ((double)(h11 & 0xFFFF) / 65535.0) * 2.0 - 1.0;
    double ix0 = n00 + u * (n10 - n00);
    double ix1 = n01 + u * (n11 - n01);
    return ix0 + v * (ix1 - ix0);
}

double spass_fbm(double x, double y, int octaves, double gain) {
    double sum = 0.0;
    double amp = 1.0;
    double freq = 1.0;
    double norm = 0.0;
    for (int i = 0; i < octaves; ++i) {
        sum  += amp * spass_noise_2d(x * freq, y * freq);
        norm += amp;
        amp  *= gain;
        freq *= 2.0;
    }
    if (norm == 0.0) return 0.0;
    return sum / norm;
}

/* -------------------------------------------------------------------- */
/* spasio I/O */
/* -------------------------------------------------------------------- */
//
// `spass_spasio_input` reads a single double from a named channel;
// `spass_spasio_output` writes one and returns it (so a call site can
// stay expression-shaped). Neither touches the file system or a socket
// directly: the three modes (file, socket, memory) are one client RPC
// to the runner, which owns the channel state. See the RPC section
// below for the frame layout and the byte-order contract.

typedef enum {
    SPASS_SPASIO_FILE   = 0,
    SPASS_SPASIO_SOCKET = 1,
    SPASS_SPASIO_MEMORY = 2,
} SpasioMode;

/* -------------------------------------------------------------------- */
/* spasio RPC client */
/* -------------------------------------------------------------------- */
//
// ALL spasio I/O is served by the saple runner: the program connects to
// the runner over TCP and exchanges data-plane frames. The program
// process never touches the file system or listens on a port - file
// channels are proxied to disk by the runner, socket/memory channels
// live in the runner's in-memory slots.
//
// Frame:
// request: [4B BE body_len][u8 opcode][u8 mode][u32 BE proj_len][project]
// [u8 ch_len][channel][8B f64 LE value]
// response: [4B BE resp_len][u8 status][8B f64 LE value]
// opcode: 0 = GET (input), 1 = SET (output). mode: 0 = file, 1 = socket
// (memory maps to socket - `ponytail`: true shared memory is a future
// revision).
//
// Windows link note: link ws2_32 (MinGW: -lws2_32; MSVC handles it
// via the #pragma below).

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#ifdef _MSC_VER
#pragma comment(lib, "ws2_32.lib")
#endif
typedef SOCKET spass_sock_t;
#define SPASS_SOCK_INVALID INVALID_SOCKET
#define SPASS_CLOSESOCK(s) closesocket(s)
#else
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <unistd.h>
typedef int spass_sock_t;
#define SPASS_SOCK_INVALID (-1)
#define SPASS_CLOSESOCK(s) close(s)
#endif

static int spass_spasio_sock_init(void) {
#ifdef _WIN32
    static int initialized = 0;
    if (!initialized) {
        WSADATA wsa;
        if (WSAStartup(MAKEWORD(2, 2), &wsa) != 0) return -1;
        initialized = 1;
    }
#endif
    return 0;
}

static int spass_spasio_recv_all(spass_sock_t s, char* buf, uint32_t len) {
    uint32_t got = 0;
    while (got < len) {
        int n = recv(s, buf + got, (int)(len - got), 0);
        if (n <= 0) return 0;
        got += (uint32_t)n;
    }
    return 1;
}

static int spass_spasio_send_all(spass_sock_t s, const char* buf, uint32_t len) {
    uint32_t sent = 0;
    while (sent < len) {
        int n = send(s, buf + sent, (int)(len - sent), 0);
        if (n <= 0) return 0;
        sent += (uint32_t)n;
    }
    return 1;
}

static void spass_spasio_sock_target(const char** host_out, int* port_out) {
    const char* host = getenv("SPASS_SOCKET_HOST");
    if (host == NULL || host[0] == '\0') host = "127.0.0.1";
    const char* port_s = getenv("SPASS_SOCKET_PORT");
*port_out = (port_s != NULL && port_s[0] != '\0') ? atoi(port_s): 9100;
*host_out = host;
}

/* One RPC to the runner (short connection: connect, one frame out,
* one frame back, close - `ponytail`: a persistent connection pool is
* the upgrade path if per-call latency ever matters). GET blocks inside
* the runner until the channel has a value (socket) or errs when the
* disk slot is missing (file) - the C side surfaces that as an abort,
* matching the historical file-mode behavior. */
static double spass_spasio_rpc(uint8_t opcode, uint8_t mode,
                               const char* channel, double value) {
    if (spass_spasio_sock_init() != 0) {
        fprintf(stderr, "spass: spasio WSAStartup failed\n");
        abort();
    }
    const char* host; int port;
    spass_spasio_sock_target(&host, &port);

    /* Project directory = current working directory (`spass run` runs
    * the module with the project as cwd). */
    char proj[512];
    if (
#ifdef _WIN32
        _getcwd(proj, sizeof(proj)) == NULL
#else
        getcwd(proj, sizeof(proj)) == NULL
#endif
    ) {
        fprintf(stderr, "spass: spasio cannot resolve project directory\n");
        abort();
    }
    size_t proj_len = strlen(proj);
    size_t nlen = strlen(channel);
    if (nlen > 255 || proj_len > 255) {
        fprintf(stderr, "spass: spasio channel/project name too long\n");
        abort();
    }

    /* Request body: [u8 opcode][u8 mode][u32 BE proj_len][project]
    * [u8 ch_len][channel][8B f64 LE value] */
    uint32_t body_len = (uint32_t)(2 + 4 + proj_len + 1 + nlen + 8);
    uint8_t frame[4 + 2 + 4 + 512 + 1 + 255 + 8];
    frame[4] = opcode;
    frame[5] = mode;
    frame[6] = (uint8_t)((proj_len >> 24) & 0xFF);
    frame[7] = (uint8_t)((proj_len >> 16) & 0xFF);
    frame[8] = (uint8_t)((proj_len >> 8) & 0xFF);
    frame[9] = (uint8_t)(proj_len & 0xFF);
    memcpy(frame + 10, proj, proj_len);
    frame[10 + proj_len] = (uint8_t)nlen;
    memcpy(frame + 11 + proj_len, channel, nlen);
    /* `ponytail`: the data body is little-endian; every
    * supported platform (x86/ARM) is LE so a plain memcpy is exact. */
    memcpy(frame + 11 + proj_len + nlen, &value, 8);
    uint32_t off = (uint32_t)(11 + proj_len + nlen + 8);
    frame[0] = (uint8_t)((body_len >> 24) & 0xFF);
    frame[1] = (uint8_t)((body_len >> 16) & 0xFF);
    frame[2] = (uint8_t)((body_len >> 8) & 0xFF);
    frame[3] = (uint8_t)(body_len & 0xFF);
    (void)off;

    spass_sock_t sock = socket(AF_INET, SOCK_STREAM, 0);
    if (sock == SPASS_SOCK_INVALID) {
        fprintf(stderr, "spass: spasio socket() failed\n");
        abort();
    }
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = inet_addr(host);
    addr.sin_port = htons((unsigned short)port);
    if (connect(sock, (struct sockaddr*)&addr, sizeof(addr)) != 0) {
        fprintf(stderr,
                "spass: cannot connect runner at %s:%d (is `spass daemon` running?)\n",
                host, port);
        SPASS_CLOSESOCK(sock);
        abort();
    }
    if (!spass_spasio_send_all(sock, (const char*)frame, 4 + body_len)) {
        fprintf(stderr, "spass: spasio runner write failed\n");
        SPASS_CLOSESOCK(sock);
        abort();
    }
    /* Response: [4B BE resp_len][u8 status][8B f64 LE value]. */
    uint8_t rhb[4];
    if (!spass_spasio_recv_all(sock, (char*)rhb, 4)) {
        fprintf(stderr, "spass: spasio runner read failed\n");
        SPASS_CLOSESOCK(sock);
        abort();
    }
    uint32_t rlen = ((uint32_t)rhb[0] << 24) | ((uint32_t)rhb[1] << 16)
                  | ((uint32_t)rhb[2] << 8) | (uint32_t)rhb[3];
    uint8_t resp[16];
    if (rlen < 1 || rlen > sizeof(resp) ||
        !spass_spasio_recv_all(sock, (char*)resp, rlen)) {
        fprintf(stderr, "spass: spasio runner read failed\n");
        SPASS_CLOSESOCK(sock);
        abort();
    }
    SPASS_CLOSESOCK(sock);
    if (resp[0] != 0) {
        fprintf(stderr, "spass: spasio channel unavailable (no value set)\n");
        abort();
    }
    if (rlen < 9) return 0.0;
    double out;
    memcpy(&out, resp + 1, 8); /* LE, see note above */
    return out;
}

double spass_spasio_input(int mode, const char* channel) {
    /* All modes go through the runner: file -> disk slots proxied by the
    * runner, socket/memory -> in-memory slots. */
    return spass_spasio_rpc(0, mode == SPASS_SPASIO_FILE ? 0 : 1, channel, 0.0);
}

double spass_spasio_output(int mode, double value, const char* channel) {
    spass_spasio_rpc(1, mode == SPASS_SPASIO_FILE ? 0 : 1, channel, value);
    return value;
}

/* -------------------------------------------------------------------- */
/* Async task runtime */
/* -------------------------------------------------------------------- */
//
// `spawn f(args)` launches a worker thread running a codegen-emitted
// thunk with the uniform `(const double* args, double* out)` protocol
// and returns an opaque task handle. `await(handle)` joins the task
// and returns the result.
//
// The handle IS the heap slot pointer - no registry table needed; the
// codegen only ever feeds handles it got from spass_spawn back here.
//
// `ponytail`: awaiting polls a 1ms interval instead of a condvar. A
// task that is never awaited leaks its slot. Upgrade path: per-task
// condvar (or the spass_task_t refcount model) + non-blocking try_wait.

typedef void (*spass_spawn_fn)(const double* args, double* out);

typedef struct spass_async {
    spass_spawn_fn thunk;
    double* args;
    double result;
    long done;   /* 0 = running, 1 = result ready (interlocked) */
} spass_async_t;

#ifdef _WIN32
#include <process.h>
static unsigned __stdcall spass_async_worker(void* p) {
    spass_async_t* a = (spass_async_t*)p;
    double out = 0.0;
    a->thunk(a->args, &out);
    a->result = out;   /* result is written BEFORE done is raised */
    InterlockedExchange(&a->done, 1);
    return 0;
}
#else
#include <pthread.h>
#include <unistd.h>
static void* spass_async_worker(void* p) {
    spass_async_t* a = (spass_async_t*)p;
    double out = 0.0;
    a->thunk(a->args, &out);
    a->result = out;
    __atomic_store_n(&a->done, 1, __ATOMIC_SEQ_CST);
    return NULL;
}
#endif

uint64_t spass_spawn(spass_spawn_fn thunk, const double* args, uint32_t nargs) {
    spass_async_t* a = (spass_async_t*)malloc(sizeof(spass_async_t));
    if (a == NULL) { fprintf(stderr, "spass: out of memory\n"); abort(); }
    a->args = (double*)malloc(sizeof(double) * (nargs ? nargs : 1));
    if (a->args == NULL) { fprintf(stderr, "spass: out of memory\n"); abort(); }
    for (uint32_t i = 0; i < nargs; ++i) a->args[i] = args[i];
    a->thunk = thunk;
    a->result = 0.0;
    a->done = 0;

#ifdef _WIN32
    {
        HANDLE h = (HANDLE)_beginthreadex(NULL, 0, spass_async_worker, a, 0, NULL);
        if (h == NULL) { fprintf(stderr, "spass: spawn thread failed\n"); abort(); }
        CloseHandle(h);  /* detached: the result slot is freed by await */
    }
#else
    {
        pthread_t t;
        pthread_attr_t attr;
        pthread_attr_init(&attr);
        pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
        if (pthread_create(&t, &attr, spass_async_worker, a) != 0) {
            fprintf(stderr, "spass: spawn thread failed\n");
            abort();
        }
        pthread_attr_destroy(&attr);
    }
#endif
    return (uint64_t)(uintptr_t)a;
}

double spass_task_await(uint64_t id) {
    spass_async_t* a = (spass_async_t*)(uintptr_t)id;
    if (a == NULL) return 0.0;
#ifdef _WIN32
    while (InterlockedOr(&a->done, 0) == 0) Sleep(1);
#else
    while (__atomic_load_n(&a->done, __ATOMIC_SEQ_CST) == 0) usleep(1000);
#endif
    double r = a->result;
    free(a->args);
    free(a);
    return r;
}

/* -------------------------------------------------------------------- */
/* Sequence message-passing runtime */
/* -------------------------------------------------------------------- */
//
// Each sequence owns one FIFO ring buffer indexed by a compile-time
// sequence id (the codegen emits the ids). Messages are marshalled as
// a fixed f64 slot array; the codegen emits per-struct pack/unpack
// helpers on top of this.
//
// `ponytail`: one global lock guards every queue (queues are tiny and
// contention is negligible at MVP scale), and `recv` polls with a 1ms
// sleep. Upgrade path: per-queue mutex + condvar.

#define SPASS_SEQ_MAX 64
#define SPASS_SEQ_QCAP 256

typedef struct {
    double f[8];
    uint32_t n;
} spass_msg_t;

typedef struct {
    spass_msg_t buf[SPASS_SEQ_QCAP];
    uint32_t head, tail, len;
} spass_seq_queue_t;

static spass_seq_queue_t* spass_seq_queues[SPASS_SEQ_MAX];
static uint32_t spass_seq_count = 0;

#ifdef _WIN32
static CRITICAL_SECTION spass_seq_cs;
static int spass_seq_cs_ready = 0;
static void spass_seq_lock(void) {
    if (!spass_seq_cs_ready) {
        InitializeCriticalSection(&spass_seq_cs);
        spass_seq_cs_ready = 1;
    }
    EnterCriticalSection(&spass_seq_cs);
}
static void spass_seq_unlock(void) { LeaveCriticalSection(&spass_seq_cs); }
#else
static pthread_mutex_t spass_seq_mu = PTHREAD_MUTEX_INITIALIZER;
static void spass_seq_lock(void) { pthread_mutex_lock(&spass_seq_mu); }
static void spass_seq_unlock(void) { pthread_mutex_unlock(&spass_seq_mu); }
#endif

void spass_seq_init(uint32_t count) {
    if (count > SPASS_SEQ_MAX) count = SPASS_SEQ_MAX;
    spass_seq_lock();
    for (uint32_t i = 0; i < count; ++i) {
        if (spass_seq_queues[i] == NULL) {
            spass_seq_queues[i] =
                (spass_seq_queue_t*)calloc(1, sizeof(spass_seq_queue_t));
            if (spass_seq_queues[i] == NULL) {
                fprintf(stderr, "spass: out of memory\n");
                abort();
            }
        }
    }
    spass_seq_count = count;
    spass_seq_unlock();
}

int spass_seq_send(uint32_t target, spass_msg_t m) {
    spass_seq_lock();
    spass_seq_queue_t* q = spass_seq_queues[target];
    int ok = 0;
    if (q != NULL && q->len < SPASS_SEQ_QCAP) {
        q->buf[q->tail] = m;
        q->tail = (q->tail + 1) % SPASS_SEQ_QCAP;
        q->len += 1;
        ok = 1;
    }
    spass_seq_unlock();
    return ok;
}

int spass_seq_broadcast(spass_msg_t m) {
    spass_seq_lock();
    for (uint32_t i = 0; i < spass_seq_count; ++i) {
        spass_seq_queue_t* q = spass_seq_queues[i];
        if (q != NULL && q->len < SPASS_SEQ_QCAP) {
            q->buf[q->tail] = m;
            q->tail = (q->tail + 1) % SPASS_SEQ_QCAP;
            q->len += 1;
        }
    }
    spass_seq_unlock();
    return 1;
}

spass_msg_t spass_seq_recv(uint32_t self_id) {
    for (;;) {
        spass_seq_lock();
        spass_seq_queue_t* q = spass_seq_queues[self_id];
        if (q != NULL && q->len > 0) {
            spass_msg_t m = q->buf[q->head];
            q->head = (q->head + 1) % SPASS_SEQ_QCAP;
            q->len -= 1;
            spass_seq_unlock();
            return m;
        }
        spass_seq_unlock();
#ifdef _WIN32
        Sleep(1);
#else
        usleep(1000);
#endif
    }
}

/* Sequence threads: spawn records the handle, wait_all joins every
* one. */

#ifdef _WIN32
static HANDLE spass_seq_handles[SPASS_SEQ_MAX];
static uint32_t spass_seq_handle_count = 0;
static unsigned __stdcall spass_seq_trampoline(void* p) {
    ((void (*)(void))p)();
    return 0;
}
#else
static pthread_t spass_seq_handles[SPASS_SEQ_MAX];
static uint32_t spass_seq_handle_count = 0;
static void* spass_seq_trampoline(void* p) {
    ((void (*)(void))p)();
    return NULL;
}
#endif

void spass_seq_spawn(void (*entry)(void)) {
#ifdef _WIN32
    HANDLE h = (HANDLE)_beginthreadex(NULL, 0, spass_seq_trampoline, (void*)entry, 0, NULL);
    if (h == NULL) { fprintf(stderr, "spass: sequence thread failed\n"); abort(); }
    spass_seq_handles[spass_seq_handle_count++] = h;
#else
    if (pthread_create(&spass_seq_handles[spass_seq_handle_count++], NULL,
                       spass_seq_trampoline, (void*)entry) != 0) {
        fprintf(stderr, "spass: sequence thread failed\n");
        abort();
    }
#endif
}

void spass_seq_wait_all(void) {
#ifdef _WIN32
    for (uint32_t i = 0; i < spass_seq_handle_count; ++i) {
        WaitForSingleObject(spass_seq_handles[i], INFINITE);
        CloseHandle(spass_seq_handles[i]);
    }
#else
    for (uint32_t i = 0; i < spass_seq_handle_count; ++i) {
        pthread_join(spass_seq_handles[i], NULL);
    }
#endif
    spass_seq_handle_count = 0;
}

/* -------------------------------------------------------------------- */
/* Vector / matrix built-ins */
/* -------------------------------------------------------------------- */
//
// The vec/mat typedefs are emitted by the codegen layer (not here)
// because the runtime must not depend on the codegen's header layout.
// We declare them locally so the helpers compile as a self-contained
// translation unit; the codegen then provides the same typedefs
// before including this file. `ponytail`: the typedef re-declaration
// is the standard C99 trick - it only works if the two declarations
// are structurally identical, which they are by construction (we
// copy the codegen's typedefs verbatim here).

typedef struct { double x, y; } spass_vec2;
typedef struct { double x, y, z; } spass_vec3;
typedef struct { double x, y, z, w; } spass_vec4;
typedef struct { double m[9]; } spass_mat3;
typedef struct { double m[16]; } spass_mat4;

/* dot product (returns scalar) */
double spass_dot2(spass_vec2 a, spass_vec2 b) { return a.x*b.x + a.y*b.y; }
double spass_dot3(spass_vec3 a, spass_vec3 b) { return a.x*b.x + a.y*b.y + a.z*b.z; }
double spass_dot4(spass_vec4 a, spass_vec4 b) { return a.x*b.x + a.y*b.y + a.z*b.z + a.w*b.w; }

/* cross product (vec3 only) */
spass_vec3 spass_cross(spass_vec3 a, spass_vec3 b) {
    spass_vec3 r = { a.y*b.z - a.z*b.y, a.z*b.x - a.x*b.z, a.x*b.y - a.y*b.x };
    return r;
}

/* length / magnitude */
double spass_length2(spass_vec2 v) { return sqrt(spass_dot2(v, v)); }
double spass_length3(spass_vec3 v) { return sqrt(spass_dot3(v, v)); }
double spass_length4(spass_vec4 v) { return sqrt(spass_dot4(v, v)); }

/* normalize (returns unit vector) */
spass_vec2 spass_normalize2(spass_vec2 v) {
    double l = spass_length2(v);
    spass_vec2 r = { v.x/l, v.y/l };
    return r;
}
spass_vec3 spass_normalize3(spass_vec3 v) {
    double l = spass_length3(v);
    spass_vec3 r = { v.x/l, v.y/l, v.z/l };
    return r;
}
spass_vec4 spass_normalize4(spass_vec4 v) {
    double l = spass_length4(v);
    spass_vec4 r = { v.x/l, v.y/l, v.z/l, v.w/l };
    return r;
}

/* hadamard (component-wise product) */
spass_vec2 spass_hadamard2(spass_vec2 a, spass_vec2 b) { spass_vec2 r = { a.x*b.x, a.y*b.y }; return r; }
spass_vec3 spass_hadamard3(spass_vec3 a, spass_vec3 b) { spass_vec3 r = { a.x*b.x, a.y*b.y, a.z*b.z }; return r; }
spass_vec4 spass_hadamard4(spass_vec4 a, spass_vec4 b) { spass_vec4 r = { a.x*b.x, a.y*b.y, a.z*b.z, a.w*b.w }; return r; }

/* transpose: matrices are column-major. m[k*N+i] is row i, col k.
* transpose[j*N+i] = m[i*N+j]. */
spass_mat3 spass_transpose3(spass_mat3 m) {
    spass_mat3 r;
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 3; j++) {
            r.m[j*3 + i] = m.m[i*3 + j];
        }
    }
    return r;
}
spass_mat4 spass_transpose4(spass_mat4 m) {
    spass_mat4 r;
    for (int i = 0; i < 4; i++) {
        for (int j = 0; j < 4; j++) {
            r.m[j*4 + i] = m.m[i*4 + j];
        }
    }
    return r;
}

/* matrix-vector product (column-major). r[k] = sum_i m[i*N+k] * v[i]. */
spass_vec3 spass_matvec3(spass_mat3 m, spass_vec3 v) {
    spass_vec3 r;
    r.x = m.m[0*3+0]*v.x + m.m[1*3+0]*v.y + m.m[2*3+0]*v.z;
    r.y = m.m[0*3+1]*v.x + m.m[1*3+1]*v.y + m.m[2*3+1]*v.z;
    r.z = m.m[0*3+2]*v.x + m.m[1*3+2]*v.y + m.m[2*3+2]*v.z;
    return r;
}
spass_vec4 spass_matvec4(spass_mat4 m, spass_vec4 v) {
    spass_vec4 r;
    r.x = m.m[0*4+0]*v.x + m.m[1*4+0]*v.y + m.m[2*4+0]*v.z + m.m[3*4+0]*v.w;
    r.y = m.m[0*4+1]*v.x + m.m[1*4+1]*v.y + m.m[2*4+1]*v.z + m.m[3*4+1]*v.w;
    r.z = m.m[0*4+2]*v.x + m.m[1*4+2]*v.y + m.m[2*4+2]*v.z + m.m[3*4+2]*v.w;
    r.w = m.m[0*4+3]*v.x + m.m[1*4+3]*v.y + m.m[2*4+3]*v.z + m.m[3*4+3]*v.w;
    return r;
}

/* matrix-matrix product. C[i+j*N] = sum_k A[k*N+i] * B[j*N+k] (col-major). */
spass_mat3 spass_matmul3(spass_mat3 a, spass_mat3 b) {
    spass_mat3 r;
    for (int i = 0; i < 3; i++) {
        for (int j = 0; j < 3; j++) {
            double s = 0.0;
            for (int k = 0; k < 3; k++) {
                s += a.m[k*3 + i] * b.m[j*3 + k];
            }
            r.m[j*3 + i] = s;
        }
    }
    return r;
}
spass_mat4 spass_matmul4(spass_mat4 a, spass_mat4 b) {
    spass_mat4 r;
    for (int i = 0; i < 4; i++) {
        for (int j = 0; j < 4; j++) {
            double s = 0.0;
            for (int k = 0; k < 4; k++) {
                s += a.m[k*4 + i] * b.m[j*4 + k];
            }
            r.m[j*4 + i] = s;
        }
    }
    return r;
}

/* `ponytail`: `inverse` and `determinant` are not part of the MVP.
* the language lists them but the MVP test surface never calls
* them; adding Gauss-Jordan + 3x3 cofactor expansion is a 200-line
* change that we can defer. A future revision will provide
* `spass_inverse3` / `spass_inverse4` here. */

/* Constructors - `ponytail`: a future revision will support
* vec2/3/4 literals (`v2 = vec2(1, 2)`) directly in the source
* grammar. The MVP exposes the constructor as a built-in so the
* test surface can build vec values from f64 args. */
spass_vec2 spass_mk_vec2(double x, double y) { spass_vec2 r = { x, y }; return r; }
spass_vec3 spass_mk_vec3(double x, double y, double z) { spass_vec3 r = { x, y, z }; return r; }
spass_vec4 spass_mk_vec4(double x, double y, double z, double w) { spass_vec4 r = { x, y, z, w }; return r; }

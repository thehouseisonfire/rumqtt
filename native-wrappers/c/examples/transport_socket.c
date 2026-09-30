/* A portable example host: all socket work runs on host threads, reads may
 * overlap writes, and cancellation wakes workers by shutting down the socket.
 * Production hosts should use their existing event reactor instead of creating
 * a thread per operation. DNS resolution here uses the platform resolver. */
#ifndef _WIN32
#define _POSIX_C_SOURCE 200809L
#endif
#include "transport_socket.h"
#include "example_common.h"
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
typedef SOCKET socket_fd;
#define BAD_SOCKET INVALID_SOCKET
#define close_socket closesocket
#define STOP_BOTH SD_BOTH
#define STOP_WRITE SD_SEND
#else
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>
typedef int socket_fd;
#define BAD_SOCKET (-1)
#define close_socket close
#define STOP_BOTH SHUT_RDWR
#define STOP_WRITE SHUT_WR
#endif
#ifdef MSG_NOSIGNAL
#define SEND_FLAGS MSG_NOSIGNAL
#else
#define SEND_FLAGS 0
#endif

typedef struct socket_stream socket_stream;
typedef struct socket_work socket_work;
struct socket_transport {
    atomic_flag lock;
    socket_work *work;
    socket_stream *streams;
    uint16_t tunnel_port;
    unsigned destroyed;
};
struct socket_stream {
    socket_transport_t *host;
    socket_stream *next;
    socket_fd fd;
    rumqttc_transport_stream_t *handle;
};
struct socket_work {
    socket_transport_t *host;
    socket_work *next;
    example_thread_t *thread;
    socket_stream *stream;
    rumqttc_callback_completion_t *completion;
    uint64_t id;
    uint64_t deadline;
    atomic_bool cancelled;
    uint32_t kind;
    size_t limit;
    rumqttc_bytes_view_t input;
    char *target;
    socket_fd connecting_fd;
};
static void lock_host(socket_transport_t *host) {
    while (atomic_flag_test_and_set_explicit(&host->lock, memory_order_acquire)) {
    }
}
static void unlock_host(socket_transport_t *host) {
    atomic_flag_clear_explicit(&host->lock, memory_order_release);
}
static uint64_t now_ms(void) {
#ifdef _WIN32
    return GetTickCount64();
#else
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
        abort();
    return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
#endif
}
static int nonblocking(socket_fd fd) {
#ifdef _WIN32
    u_long enabled = 1;
    return ioctlsocket(fd, FIONBIO, &enabled) == 0;
#else
    return fcntl(fd, F_SETFL, O_NONBLOCK) == 0;
#endif
}
static int would_block(void) {
#ifdef _WIN32
    int error = WSAGetLastError();
    return error == WSAEWOULDBLOCK || error == WSAEINPROGRESS;
#else
    return errno == EAGAIN || errno == EWOULDBLOCK || errno == EINPROGRESS || errno == EINTR;
#endif
}
static int wait_socket(socket_work *work, socket_fd fd, int write) {
    for (;;) {
        if (atomic_load(&work->cancelled) || (work->deadline && now_ms() >= work->deadline))
            return 0;
        fd_set set;
        FD_ZERO(&set);
        FD_SET(fd, &set);
        struct timeval timeout = {0, 50000};
#ifdef _WIN32
        int nfds = 0; /* Winsock ignores this argument; SOCKET is pointer-sized. */
#else
        int nfds = fd + 1;
#endif
        int ready = select(nfds, write ? NULL : &set, write ? &set : NULL, NULL, &timeout);
        if (ready > 0)
            return 1;
        if (ready < 0 && !would_block())
            return 0;
    }
}
static void cancel_work(void *data, uint64_t id) {
    socket_transport_t *host = data;
    lock_host(host);
    for (socket_work *work = host->work; work != NULL; work = work->next) {
        if (work->id == id) {
            atomic_store(&work->cancelled, 1);
            socket_fd fd = work->stream ? work->stream->fd : work->connecting_fd;
            if (fd != BAD_SOCKET)
                (void)shutdown(fd, STOP_BOTH);
            break;
        }
    }
    unlock_host(host);
}
static void cancel_stream(void *data, uint64_t id) {
    socket_stream *stream = data;
    cancel_work(stream->host, id);
}
static void destroy_registration(void *data) {
    socket_transport_t *host = data;
    ++host->destroyed;
}
static void destroy_stream(void *data) {
    socket_stream *stream = data;
    close_socket(stream->fd);
}
static void perform(void *, const rumqttc_transport_io_request_t *, rumqttc_callback_completion_t *);

static int send_bytes(socket_work *work, socket_fd fd, const char *data, size_t length) {
    while (length) {
        if (!wait_socket(work, fd, 1))
            return 0;
        int count = (int)send(fd, data, (int)length, SEND_FLAGS);
        if (count < 0 && would_block())
            continue;
        if (count <= 0)
            return 0;
        data += count;
        length -= (size_t)count;
    }
    return 1;
}
static socket_fd dial(socket_work *work) {
    char *endpoint = work->target;
    char *colon = strrchr(endpoint, ':');
    if (colon == NULL)
        return BAD_SOCKET;
    size_t hostname_len = (size_t)(colon - endpoint);
    char *hostpart = malloc(hostname_len + 1);
    if (hostpart == NULL)
        abort();
    memcpy(hostpart, endpoint, hostname_len);
    hostpart[hostname_len] = '\0';
    char *hostname = hostpart;
    if (*hostname == '[') {
        ++hostname;
        size_t len = strlen(hostname);
        if (len && hostname[len - 1] == ']')
            hostname[len - 1] = '\0';
    }
    char tunnel_port[8];
    if (work->host->tunnel_port) {
        snprintf(tunnel_port, sizeof(tunnel_port), "%u", (unsigned)work->host->tunnel_port);
        hostname = "127.0.0.1";
    }
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_socktype = SOCK_STREAM;
    struct addrinfo *addresses = NULL;
    int status = getaddrinfo(hostname, work->host->tunnel_port ? tunnel_port : colon + 1, &hints, &addresses);
    free(hostpart);
    if (status != 0)
        return BAD_SOCKET;
    socket_fd connected = BAD_SOCKET;
    for (struct addrinfo *address = addresses; address && !atomic_load(&work->cancelled); address = address->ai_next) {
        socket_fd fd = socket(address->ai_family, SOCK_STREAM, address->ai_protocol);
        if (fd == BAD_SOCKET)
            continue;
#ifndef _WIN32
        if (fd >= FD_SETSIZE) {
            close_socket(fd);
            continue;
        }
#endif
#ifdef SO_NOSIGPIPE
        int no_sigpipe = 1;
        if (setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe)) != 0) {
            close_socket(fd);
            continue;
        }
#endif
        if (!nonblocking(fd)) {
            close_socket(fd);
            continue;
        }
        lock_host(work->host);
        work->connecting_fd = fd;
        unlock_host(work->host);
        int okay = connect(fd, address->ai_addr, (int)address->ai_addrlen) == 0;
        if (!okay && would_block() && wait_socket(work, fd, 1)) {
            int error = 0;
#ifdef _WIN32
            int length = sizeof(error);
#else
            socklen_t length = sizeof(error);
#endif
            okay = getsockopt(fd, SOL_SOCKET, SO_ERROR, (char *)&error, &length) == 0 && error == 0;
        }
        lock_host(work->host);
        work->connecting_fd = BAD_SOCKET;
        unlock_host(work->host);
        if (okay && !atomic_load(&work->cancelled)) {
            connected = fd;
            break;
        }
        close_socket(fd);
    }
    freeaddrinfo(addresses);
    if (connected == BAD_SOCKET)
        return connected;
    if (work->host->tunnel_port) {
        if (!send_bytes(work, connected, "RUMQTTC-TUNNEL ", 15) ||
            !send_bytes(work, connected, work->target, strlen(work->target)) || !send_bytes(work, connected, "\n", 1)) {
            close_socket(connected);
            return BAD_SOCKET;
        }
    }
    return connected;
}
static int worker(void *data) {
    socket_work *work = data;
    rumqttc_transport_response_t response = {0};
    response.struct_size = sizeof(response);
    uint8_t *bytes = NULL;
    if (work->kind == 0) {
        socket_fd fd = dial(work);
        if (fd == BAD_SOCKET)
            response.result = work->deadline && now_ms() >= work->deadline ? RUMQTTC_TRANSPORT_FAILURE_TIMEOUT
                                                                           : RUMQTTC_TRANSPORT_FAILURE_CONNECT;
        else {
            socket_stream *stream = calloc(1, sizeof(*stream));
            if (stream == NULL)
                abort();
            stream->host = work->host;
            stream->fd = fd;
            rumqttc_transport_stream_vtable_t table = {0};
            table.struct_size = sizeof(table);
            table.mode = RUMQTTC_TRANSPORT_BASE;
            table.perform = perform;
            table.cancel = cancel_stream;
            table.destroy = destroy_stream;
            if (rumqttc_transport_stream_new(work->completion, &table, stream, &stream->handle, NULL) != RUMQTTC_OK) {
                close_socket(fd);
                free(stream);
                response.result = RUMQTTC_TRANSPORT_FAILURE_CONNECT;
            } else {
                lock_host(work->host);
                stream->next = work->host->streams;
                work->host->streams = stream;
                unlock_host(work->host);
                response.stream = stream->handle;
                response.network_handling = RUMQTTC_TRANSPORT_NETWORK_NOT_APPLICABLE;
            }
        }
    } else if (work->kind == RUMQTTC_TRANSPORT_READ) {
        bytes = malloc(work->limit);
        if (bytes == NULL)
            abort();
        int count = -1;
        while (wait_socket(work, work->stream->fd, 0)) {
            count = (int)recv(work->stream->fd, (char *)bytes, (int)work->limit, 0);
            if (count >= 0 || !would_block())
                break;
        }
        if (count < 0)
            response.result = RUMQTTC_TRANSPORT_FAILURE_IO;
        else {
            response.bytes.data = bytes;
            response.bytes.len = (size_t)count;
        }
    } else if (work->kind == RUMQTTC_TRANSPORT_WRITE) {
        int count = -1;
        while (wait_socket(work, work->stream->fd, 1)) {
            count = (int)send(work->stream->fd, (const char *)work->input.data, (int)work->input.len, SEND_FLAGS);
            if (count >= 0 || !would_block())
                break;
        }
        if (count <= 0)
            response.result = RUMQTTC_TRANSPORT_FAILURE_IO;
        else
            response.count = (size_t)count;
    }
    uint32_t status = rumqttc_callback_transport_complete(work->completion, &response);
    free(bytes);
    rumqttc_callback_completion_destroy(work->completion);
    work->completion = NULL;
    return status == RUMQTTC_OK || status == RUMQTTC_INVALID_STATE ? 0 : 1;
}
static void start_work(socket_work *work, rumqttc_callback_completion_t *completion) {
    if (rumqttc_callback_completion_retain(completion, &work->completion) != RUMQTTC_OK)
        abort();
    lock_host(work->host);
    work->next = work->host->work;
    work->host->work = work;
    unlock_host(work->host);
    work->thread = example_thread_start(worker, work);
    if (work->thread == NULL)
        abort();
}
static void connect_stream(void *data, const rumqttc_transport_connect_request_t *request,
                           rumqttc_callback_completion_t *completion) {
    socket_transport_t *host = data;
    if (request->send_buffer_present || request->receive_buffer_present || request->tcp_nodelay || request->mptcp ||
        request->local_address.len || request->bind_device.len) {
        rumqttc_transport_response_t response = {0};
        response.struct_size = sizeof(response);
        response.result = RUMQTTC_TRANSPORT_FAILURE_NETWORK_OPTIONS;
        (void)rumqttc_callback_transport_complete(completion, &response);
        return;
    }
    socket_work *work = calloc(1, sizeof(*work));
    if (work == NULL)
        abort();
    work->host = host;
    work->id = request->operation_id;
    work->connecting_fd = BAD_SOCKET;
    atomic_init(&work->cancelled, 0);
    work->deadline = now_ms() + request->remaining_timeout_ns / 1000000;
    work->target = malloc(request->target.len + 1);
    if (work->target == NULL)
        abort();
    memcpy(work->target, request->target.data, request->target.len);
    work->target[request->target.len] = '\0';
    start_work(work, completion);
}
static void perform(void *data, const rumqttc_transport_io_request_t *request,
                    rumqttc_callback_completion_t *completion) {
    socket_stream *stream = data;
    if (request->operation == RUMQTTC_TRANSPORT_FLUSH || request->operation == RUMQTTC_TRANSPORT_SHUTDOWN) {
        rumqttc_transport_response_t response = {0};
        response.struct_size = sizeof(response);
        if (request->operation == RUMQTTC_TRANSPORT_SHUTDOWN && shutdown(stream->fd, STOP_WRITE) != 0)
            response.result = RUMQTTC_TRANSPORT_FAILURE_IO;
        (void)rumqttc_callback_transport_complete(completion, &response);
        return;
    }
    socket_work *work = calloc(1, sizeof(*work));
    if (work == NULL)
        abort();
    work->host = stream->host;
    work->stream = stream;
    work->id = request->operation_id;
    work->kind = request->operation;
    work->limit = request->read_limit;
    work->input = request->input;
    atomic_init(&work->cancelled, 0);
    work->connecting_fd = BAD_SOCKET;
    start_work(work, completion);
}
socket_transport_t *socket_transport_new(uint16_t tunnel_port, rumqttc_transport_registration_t **out) {
#ifdef _WIN32
    WSADATA data;
    if (WSAStartup(MAKEWORD(2, 2), &data) != 0)
        return NULL;
#endif
    socket_transport_t *host = calloc(1, sizeof(*host));
    if (host == NULL) {
#ifdef _WIN32
        WSACleanup();
#endif
        return NULL;
    }
    atomic_flag_clear(&host->lock);
    host->tunnel_port = tunnel_port;
    rumqttc_transport_vtable_t table = {0};
    table.struct_size = sizeof(table);
    table.mode = RUMQTTC_TRANSPORT_BASE;
    table.max_retained_operations = 4096;
    table.connect = connect_stream;
    table.cancel = cancel_work;
    table.destroy = destroy_registration;
    if (rumqttc_transport_registration_new(&table, host, out, NULL) != RUMQTTC_OK) {
        free(host);
#ifdef _WIN32
        WSACleanup();
#endif
        return NULL;
    }
    return host;
}
int socket_transport_join_destroy(socket_transport_t *host) {
    int okay = 1;
    /* The client driver is joined, so no new perform callbacks can append work.
     * Workers may still append streams; join all of them before inspecting it. */
    for (socket_work *work = host->work; work != NULL; work = work->next) {
        int result = 0;
        if (work->thread == NULL)
            continue;
        if (example_thread_join(work->thread, 5000, &result) != 0)
            return 0;
        work->thread = NULL;
        if (result != 0)
            okay = 0;
    }
    for (socket_stream *stream = host->streams; stream != NULL;) {
        socket_stream *next = stream->next;
        rumqttc_transport_stream_destroy(stream->handle);
        free(stream);
        stream = next;
    }
    for (socket_work *work = host->work; work != NULL;) {
        socket_work *next = work->next;
        free(work->target);
        free(work);
        work = next;
    }
    if (host->destroyed != 1)
        okay = 0;
    free(host);
#ifdef _WIN32
    WSACleanup();
#endif
    return okay;
}

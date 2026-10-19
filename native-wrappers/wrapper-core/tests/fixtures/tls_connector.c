/* Private prototype, not a public or proposed rumqttc ABI. OpenSSL stays host-owned. */
#include <openssl/ssl.h>
#include <openssl/pem.h>
#include <openssl/err.h>
#include <stdlib.h>

enum { PROOF_OK = 1, PROOF_READ = 2, PROOF_WRITE = 3, PROOF_EOF = 4,
       PROOF_AUTH = 10, PROOF_TLS = 11, PROOF_TRUNCATED = 12 };

static int result(SSL *ssl, int rc) {
    int error;
    if (rc > 0) return PROOF_OK;
    error = SSL_get_error(ssl, rc);
    if (error == SSL_ERROR_WANT_READ) return PROOF_READ;
    if (error == SSL_ERROR_WANT_WRITE) return PROOF_WRITE;
    if (error == SSL_ERROR_ZERO_RETURN) return PROOF_EOF;
    if (SSL_get_verify_result(ssl) != X509_V_OK) return PROOF_AUTH;
    if (error == SSL_ERROR_SYSCALL ||
        (ERR_GET_REASON(ERR_peek_last_error()) == SSL_R_UNEXPECTED_EOF_WHILE_READING))
        return PROOF_TRUNCATED;
    /* Protocol and signature failures cannot become eligible transport failures. */
    return PROOF_TLS;
}

void *proof_tls_new(const unsigned char *ca, int ca_len, const char *host) {
    SSL_CTX *ctx = NULL;
    SSL *ssl = NULL;
    BIO *pem = NULL, *input = NULL, *output = NULL;
    X509 *cert = NULL;
    ctx = SSL_CTX_new(TLS_client_method());
    if (!ctx) goto fail;
    SSL_CTX_set_verify(ctx, SSL_VERIFY_PEER, NULL);
    /* The reference consumer owns version, trust and resumption policy. */
    if (!SSL_CTX_set_min_proto_version(ctx, TLS1_3_VERSION) ||
        !SSL_CTX_set_max_proto_version(ctx, TLS1_3_VERSION)) goto fail;
    SSL_CTX_set_session_cache_mode(ctx, SSL_SESS_CACHE_OFF);
    pem = BIO_new_mem_buf(ca, ca_len);
    if (!pem) goto fail;
    cert = PEM_read_bio_X509(pem, NULL, NULL, NULL);
    if (!cert || !X509_STORE_add_cert(SSL_CTX_get_cert_store(ctx), cert)) goto fail;
    ssl = SSL_new(ctx);
    if (!ssl) goto fail;
    if (!SSL_set_tlsext_host_name(ssl, host) || !SSL_set1_host(ssl, host)) goto fail;
    input = BIO_new(BIO_s_mem());
    output = BIO_new(BIO_s_mem());
    if (!input || !output) goto fail;
    BIO_set_mem_eof_return(input, -1);
    SSL_set_bio(ssl, input, output);
    input = output = NULL;
    SSL_set_mode(ssl, SSL_MODE_ENABLE_PARTIAL_WRITE | SSL_MODE_ACCEPT_MOVING_WRITE_BUFFER);
    SSL_set_connect_state(ssl);
    X509_free(cert);
    BIO_free(pem);
    SSL_CTX_free(ctx);
    return ssl;
fail:
    BIO_free(input);
    BIO_free(output);
    X509_free(cert);
    BIO_free(pem);
    SSL_free(ssl);
    SSL_CTX_free(ctx);
    ERR_clear_error();
    return NULL;
}

void proof_tls_free(void *opaque) { SSL_free(opaque); ERR_clear_error(); }
int proof_tls_arm_update_on_read(void *opaque) {
    SSL *ssl = opaque;
    /* A host policy may refresh keys after receiving application data. */
    return SSL_set_app_data(ssl, ssl);
}
int proof_tls_handshake(void *opaque) {
    SSL *ssl = opaque;
    int rc;
    ERR_clear_error();
    rc = SSL_do_handshake(ssl);
    return result(ssl, rc);
}
int proof_tls_read(void *opaque, unsigned char *out, size_t capacity, size_t *count) {
    SSL *ssl = opaque;
    int rc;
    ERR_clear_error();
    rc = SSL_read_ex(ssl, out, capacity, count);
    if (rc > 0 && SSL_get_app_data(ssl)) {
        SSL_set_app_data(ssl, NULL);
        if (!SSL_key_update(ssl, SSL_KEY_UPDATE_REQUESTED) || SSL_do_handshake(ssl) <= 0)
            return PROOF_TLS;
    }
    return result(ssl, rc);
}
int proof_tls_write(void *opaque, const unsigned char *data, size_t size, size_t *count) {
    SSL *ssl = opaque;
    int rc;
    ERR_clear_error();
    rc = SSL_write_ex(ssl, data, size, count);
    return result(ssl, rc);
}
int proof_tls_shutdown(void *opaque) {
    SSL *ssl = opaque;
    int rc;
    ERR_clear_error();
    rc = SSL_shutdown(ssl);
    /* Sending close-notify is sufficient; do not wait for the peer's response. */
    return rc == 0 ? PROOF_OK : result(ssl, rc);
}
int proof_tls_input(void *opaque, const unsigned char *data, int size) {
    SSL *ssl = opaque;
    return BIO_write(SSL_get_rbio(ssl), data, size);
}
int proof_tls_output(void *opaque, unsigned char *data, int capacity) {
    SSL *ssl = opaque;
    return BIO_read(SSL_get_wbio(ssl), data, capacity);
}
void proof_tls_input_eof(void *opaque) {
    SSL *ssl = opaque;
    BIO_set_mem_eof_return(SSL_get_rbio(ssl), 0);
}

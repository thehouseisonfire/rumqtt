#ifndef RUMQTTC_TRANSPORT_SOCKET_H
#define RUMQTTC_TRANSPORT_SOCKET_H
#include "rumqttc.h"
/* Example host adapter. Join only after closing/destroying every client and
 * configuration using it. A nonzero tunnel_port enables the fixture's byte
 * tunnel; zero connects directly to the requested endpoint. */
typedef struct socket_transport socket_transport_t;
socket_transport_t *socket_transport_new(uint16_t tunnel_port, rumqttc_transport_registration_t **registration_out);
int socket_transport_join_destroy(socket_transport_t *host);
#endif

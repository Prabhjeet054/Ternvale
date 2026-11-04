/* vmnet.framework shared-mode helpers. Compiled only with the `vmnet` feature. */

#include <vmnet/vmnet.h>
#include <dispatch/dispatch.h>
#include <xpc/xpc.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/uio.h>
#include <time.h>
#include <AvailabilityMacros.h>

#define TV_VMNET_BATCH 32
#define TV_VMNET_CAP 65536

struct tv_vmnet {
	interface_ref iface;
	dispatch_queue_t queue;
	uint64_t mtu;
	uint64_t max_packet;
	uint64_t max_read;
	uint64_t max_write;
	uint8_t mac[6];
	void (*on_event)(void *ctx);
	void *ctx;
};

static void
parse_mac(const char *s, uint8_t mac[6])
{
	unsigned a[6] = {0};
	if (s == NULL) {
		return;
	}
	(void)sscanf(s, "%x:%x:%x:%x:%x:%x", &a[0], &a[1], &a[2], &a[3], &a[4], &a[5]);
	for (int i = 0; i < 6; i++) {
		mac[i] = (uint8_t)a[i];
	}
}

static const char *
status_str(vmnet_return_t st)
{
	switch (st) {
	case VMNET_SUCCESS:
		return "SUCCESS";
	case VMNET_FAILURE:
		return "FAILURE";
	case VMNET_MEM_FAILURE:
		return "MEM_FAILURE";
	case VMNET_INVALID_ARGUMENT:
		return "INVALID_ARGUMENT";
	case VMNET_SETUP_INCOMPLETE:
		return "SETUP_INCOMPLETE";
	case VMNET_INVALID_ACCESS:
		return "INVALID_ACCESS";
	case VMNET_PACKET_TOO_BIG:
		return "PACKET_TOO_BIG";
	case VMNET_BUFFER_EXHAUSTED:
		return "BUFFER_EXHAUSTED";
	case VMNET_TOO_MANY_PACKETS:
		return "TOO_MANY_PACKETS";
	case VMNET_SHARING_SERVICE_BUSY:
		return "SHARING_SERVICE_BUSY";
	case VMNET_NOT_AUTHORIZED:
		return "NOT_AUTHORIZED";
	default:
		return "UNKNOWN";
	}
}

struct tv_vmnet *
tv_vmnet_start_shared(char *err, unsigned errlen)
{
	struct tv_vmnet *out = calloc(1, sizeof(*out));
	if (out == NULL) {
		snprintf(err, errlen, "calloc tv_vmnet");
		return NULL;
	}
	out->max_read = TV_VMNET_BATCH;
	out->max_write = TV_VMNET_BATCH;
	out->max_packet = 1514;
	out->mtu = 1500;

	xpc_object_t desc = xpc_dictionary_create(NULL, NULL, 0);
	xpc_dictionary_set_uint64(desc, vmnet_operation_mode_key, VMNET_SHARED_MODE);

	dispatch_queue_t q = dispatch_queue_create("ternvale.vmnet", DISPATCH_QUEUE_SERIAL);
	dispatch_semaphore_t sem = dispatch_semaphore_create(0);
	__block vmnet_return_t st = VMNET_FAILURE;
	__block xpc_object_t params = NULL;

	interface_ref iface = vmnet_start_interface(desc, q, ^(vmnet_return_t status, xpc_object_t p) {
		st = status;
		if (p != NULL) {
			params = xpc_retain(p);
		}
		dispatch_semaphore_signal(sem);
	});
	xpc_release(desc);
	if (iface == NULL) {
		snprintf(err, errlen, "vmnet_start_interface returned null");
		dispatch_release(q);
		dispatch_release(sem);
		free(out);
		return NULL;
	}
	(void)dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 10LL * NSEC_PER_SEC));
	dispatch_release(sem);
	if (st != VMNET_SUCCESS || params == NULL) {
		snprintf(err, errlen, "vmnet_start_interface: %s", status_str(st));
		if (params != NULL) {
			xpc_release(params);
		}
		dispatch_semaphore_t stop = dispatch_semaphore_create(0);
		(void)vmnet_stop_interface(iface, q, ^(vmnet_return_t status) {
			(void)status;
			dispatch_semaphore_signal(stop);
		});
		(void)dispatch_semaphore_wait(stop, dispatch_time(DISPATCH_TIME_NOW, 5LL * NSEC_PER_SEC));
		dispatch_release(stop);
		dispatch_release(q);
		free(out);
		return NULL;
	}

	out->mtu = xpc_dictionary_get_uint64(params, vmnet_mtu_key);
	out->max_packet = xpc_dictionary_get_uint64(params, vmnet_max_packet_size_key);
#if defined(MAC_OS_VERSION_15_0)
	uint64_t rd = xpc_dictionary_get_uint64(params, vmnet_read_max_packets_key);
	uint64_t wr = xpc_dictionary_get_uint64(params, vmnet_write_max_packets_key);
	if (rd > 0 && rd < out->max_read) {
		out->max_read = rd;
	}
	if (wr > 0 && wr < out->max_write) {
		out->max_write = wr;
	}
#endif
	if (out->mtu == 0) {
		out->mtu = 1500;
	}
	if (out->max_packet == 0) {
		out->max_packet = 1514;
	}
	parse_mac(xpc_dictionary_get_string(params, vmnet_mac_address_key), out->mac);
	xpc_release(params);

	out->iface = iface;
	out->queue = q;
	return out;
}

int
tv_vmnet_set_event_cb(struct tv_vmnet *s, void (*cb)(void *), void *ctx)
{
	s->on_event = cb;
	s->ctx = ctx;
	vmnet_return_t st = vmnet_interface_set_event_callback(
	    s->iface, VMNET_INTERFACE_PACKETS_AVAILABLE, s->queue,
	    ^(interface_event_t mask, xpc_object_t event) {
		    (void)mask;
		    (void)event;
		    if (s->on_event != NULL) {
			    s->on_event(s->ctx);
		    }
	    });
	return st == VMNET_SUCCESS ? 0 : (int)st;
}

int
tv_vmnet_read(struct tv_vmnet *s, uint8_t *buf, uint32_t stride, uint32_t n, uint32_t *out_n,
    uint32_t *out_lens)
{
	if (n == 0 || n > TV_VMNET_BATCH || stride == 0 || stride > TV_VMNET_CAP) {
		return (int)VMNET_INVALID_ARGUMENT;
	}
	struct vmpktdesc pkts[TV_VMNET_BATCH];
	struct iovec iov[TV_VMNET_BATCH];
	int pktcnt = (int)n;
	for (uint32_t i = 0; i < n; i++) {
		iov[i].iov_base = buf + (size_t)i * stride;
		iov[i].iov_len = stride;
		pkts[i].vm_pkt_size = stride;
		pkts[i].vm_pkt_iov = &iov[i];
		pkts[i].vm_pkt_iovcnt = 1;
		pkts[i].vm_flags = 0;
	}
	vmnet_return_t st = vmnet_read(s->iface, pkts, &pktcnt);
	if (st != VMNET_SUCCESS) {
		return (int)st;
	}
	*out_n = (uint32_t)pktcnt;
	for (int i = 0; i < pktcnt; i++) {
		out_lens[i] = (uint32_t)pkts[i].vm_pkt_size;
	}
	return 0;
}

int
tv_vmnet_write(struct tv_vmnet *s, const uint8_t *buf, const uint32_t *lens, uint32_t stride,
    uint32_t n, uint32_t *written)
{
	if (n == 0 || n > TV_VMNET_BATCH || stride > TV_VMNET_CAP) {
		return (int)VMNET_INVALID_ARGUMENT;
	}
	struct vmpktdesc pkts[TV_VMNET_BATCH];
	struct iovec iov[TV_VMNET_BATCH];
	int pktcnt = (int)n;
	for (uint32_t i = 0; i < n; i++) {
		iov[i].iov_base = (void *)(buf + (size_t)i * stride);
		iov[i].iov_len = lens[i];
		pkts[i].vm_pkt_size = lens[i];
		pkts[i].vm_pkt_iov = &iov[i];
		pkts[i].vm_pkt_iovcnt = 1;
		pkts[i].vm_flags = 0;
	}
	vmnet_return_t st = vmnet_write(s->iface, pkts, &pktcnt);
	*written = (uint32_t)pktcnt;
	return st == VMNET_SUCCESS ? 0 : (int)st;
}

int
tv_vmnet_stop(struct tv_vmnet *s)
{
	if (s->iface == NULL) {
		return 0;
	}
	dispatch_semaphore_t sem = dispatch_semaphore_create(0);
	vmnet_return_t st = vmnet_stop_interface(s->iface, s->queue, ^(vmnet_return_t status) {
		(void)status;
		dispatch_semaphore_signal(sem);
	});
	(void)dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 5LL * NSEC_PER_SEC));
	dispatch_release(sem);
	if (s->queue != NULL) {
		dispatch_release(s->queue);
	}
	s->iface = NULL;
	s->queue = NULL;
	free(s);
	return st == VMNET_SUCCESS ? 0 : (int)st;
}

void
tv_vmnet_copy_info(const struct tv_vmnet *s, uint64_t *mtu, uint64_t *max_packet, uint64_t *max_read,
    uint64_t *max_write, uint8_t mac[6])
{
	*mtu = s->mtu;
	*max_packet = s->max_packet;
	*max_read = s->max_read;
	*max_write = s->max_write;
	memcpy(mac, s->mac, 6);
}

const char *
tv_vmnet_status_str(int st)
{
	if (st == 0) {
		return "SUCCESS";
	}
	return status_str((vmnet_return_t)st);
}

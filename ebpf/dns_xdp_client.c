// SPDX-License-Identifier: GPL-2.0
// XDP program for dnsmark: capture DNS responses (UDP src_port=53) and
// redirect them into an AF_XDP socket for zero-copy user-space receive.
// All other packets pass through to the kernel network stack unchanged.

#include <linux/bpf.h>
#include <linux/if_ether.h>
#include <linux/ip.h>
#include <linux/ipv6.h>
#include <linux/udp.h>
#include <bpf/bpf_helpers.h>

struct {
    __uint(type, BPF_MAP_TYPE_XSKMAP);
    __type(key, __u32);
    __type(value, __u32);
    __uint(max_entries, 64);
} XSKS SEC(".maps");

SEC("xdp")
int dns_xdp_client(struct xdp_md *ctx)
{
    void *data     = (void *)(long)ctx->data;
    void *data_end = (void *)(long)ctx->data_end;

    struct ethhdr *eth = data;
    if ((void *)(eth + 1) > data_end)
        return XDP_PASS;

    __u16 eth_proto = __builtin_bswap16(eth->h_proto);
    struct udphdr *udp;

    if (eth_proto == 0x0800) {
        /* IPv4 — constant IHL=20 (no options); packets with options pass through */
        struct iphdr *ip = (void *)(eth + 1);
        if ((void *)(ip + 1) > data_end)
            return XDP_PASS;
        if (ip->protocol != 17)
            return XDP_PASS;
        if ((ip->ihl & 0xF) != 5)
            return XDP_PASS;
        udp = (struct udphdr *)((void *)ip + 20);
    } else if (eth_proto == 0x86DD) {
        /* IPv6 */
        struct ipv6hdr *ip6 = (void *)(eth + 1);
        if ((void *)(ip6 + 1) > data_end)
            return XDP_PASS;
        if (ip6->nexthdr != 17)
            return XDP_PASS;
        udp = (struct udphdr *)(ip6 + 1);
    } else {
        return XDP_PASS;
    }

    if ((void *)(udp + 1) > data_end)
        return XDP_PASS;

    /* DNS response: source port = 53 */
    if (udp->source != __builtin_bswap16(53))
        return XDP_PASS;

    return bpf_redirect_map(&XSKS, ctx->rx_queue_index, XDP_PASS);
}

char _license[] SEC("license") = "GPL";

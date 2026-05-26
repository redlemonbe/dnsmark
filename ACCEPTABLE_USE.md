# Acceptable Use Policy — dnsmark

dnsmark is a DNS benchmarking tool. At full throttle it generates
hundreds of thousands of queries per second. Pointed at a server
you do not own, it is a denial-of-service weapon.

## Permitted use

- DNS infrastructure you own or operate
- Systems for which you hold explicit **written** authorization
- Controlled academic or security research environments
- CI/CD performance gates on your own resolvers

## Prohibited use

- Any system without prior written authorization from its owner
- Denial-of-service attacks or amplification attacks
- Integration into botnets, attack scripts, or automated attack pipelines
- Any activity that violates applicable local, national, or international law

## License and consequences

dnsmark is distributed under the **GNU Affero General Public License v3 (AGPL-3.0-only)**.
Any use of dnsmark as part of a network service requires making the full source code
available under the same license.

Unauthorized use voids all liability protections and constitutes a criminal offense
in most jurisdictions, including but not limited to violations of the Computer Fraud
and Abuse Act (US), the Computer Misuse Act (UK), and equivalent legislation worldwide.

The authors bear no responsibility for misuse.

---

*dnsmark is designed to benchmark [Runbound](https://github.com/redlemonbe/Runbound)
and other RFC 1035-compliant DNS servers in authorized environments.*

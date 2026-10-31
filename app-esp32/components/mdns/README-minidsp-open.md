# espressif/mdns 1.14.1, patched

A copy of [espressif/mdns](https://github.com/espressif/esp-protocols/tree/master/components/mdns)
at tag `mdns-v1.14.1` (Apache-2.0, see `LICENSE`), without its tests, examples
and design notes.

One change, in `mdns_send.c`: with IPv6 compiled out, the responder answers an
AAAA question for its own hostname with an NSEC record that lists only A
(RFC 6762 section 6.1). Upstream leaves the question unanswered, and macOS,
which asks for A and AAAA together, then waits about 5 s before using the IPv4
address. The board runs without IPv6 (`CONFIG_LWIP_IPV6=n`).

To update, copy the new release's component files over these, reapply the
patch (search for `append_nsec_a_only_record`), and check that
`curl -w '%{time_namelookup}' http://minidsp-<serial>.local:5380/firmware`
still resolves in milliseconds.

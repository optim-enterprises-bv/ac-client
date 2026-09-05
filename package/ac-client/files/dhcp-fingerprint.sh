#!/bin/sh
# ac-client DHCP fingerprint capture hook.
#
# Sourced by /usr/lib/dnsmasq/dhcp-script.sh (via USER_DHCPSCRIPT) on every
# DHCP event. dnsmasq exports the client's DHCP options as DNSMASQ_* env vars;
# we persist the ones that identify the device/OS so ac-client can report them
# as a fingerprint. Written to a tmpfs file keyed by MAC so ac-client can read
# it back without holding a lock or a socket.
#
# The fingerprint is the vendor class (option 60) plus the requested options
# (option 55) — the two fields that most reliably distinguish an iPhone from
# an Android from a smart TV, even when the client randomises its MAC.

FINGERPRINT_DIR=/tmp/dhcp-fingerprints

# Only capture on lease add/update (the client is actually present).
case "$1" in
	add|old)
		;;
	*)
		exit 0
		;;
esac

MAC="$2"
[ -n "$MAC" ] || exit 0

mkdir -p "$FINGERPRINT_DIR"

# Normalise the MAC to lowercase colon form for a stable filename.
mac_lc=$(echo "$MAC" | tr 'A-F' 'a-f')

{
	echo "mac=$mac_lc"
	echo "vendor_class=${DNSMASQ_VENDOR_CLASS:-}"
	echo "requested_options=${DNSMASQ_REQUESTED_OPTIONS:-}"
	echo "user_class=${DNSMASQ_USER_CLASS:-}"
	echo "hostname=${DNSMASQ_SUPPLIED_HOSTNAME:-}"
} > "$FINGERPRINT_DIR/$mac_lc"

exit 0

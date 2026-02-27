#
# ac-client — Rust USP Agent daemon for OpenWrt access points
#
# Drop this directory into any OpenWrt buildroot as a package feed entry, or
# reference it from a feeds.conf line such as:
#
#   src-git-full  ac-client  git@github.com:optim-enterprises-bv/ac-client.git
#
# (Use src-git-full so the full repo history is cloned — needed for subdir builds.)
#
# Requirements
# ───────────────────────────────────────────────────────────────────────────
#   • OpenWrt 22.03 or later (for kernel headers and musl 1.2)
#   • Rust host toolchain (from packages/lang/rust in the packages feed)
#     Minimum Rust edition: 2021  (rustc ≥ 1.75)
#   • cmake (host) – required to cross-compile aws-lc-rs (used by
#     rustls-post-quantum for X25519+ML-KEM-768 hybrid key exchange)
#
# Build dependency chain
# ───────────────────────────────────────────────────────────────────────────
#   aws-lc-sys  →  cmake (host build tool) + target C compiler
#   prost-build →  protoc is NOT needed: prost-build ships its own parser
#

include $(TOPDIR)/rules.mk

PKG_NAME    := ac-client
PKG_VERSION := 0.1.0
PKG_RELEASE := 1

# ── Source ───────────────────────────────────────────────────────────────────
# The ac-client crate is the root of this repository.  Proto files are
# vendored in proto/ so no sibling repos are required at build time.
#
# Replace PKG_SOURCE_VERSION with the exact commit hash you want to pin to.
PKG_SOURCE_PROTO   := git
PKG_SOURCE_URL     := git@github.com:optim-enterprises-bv/ac-client.git
PKG_SOURCE_VERSION := main
PKG_MIRROR_HASH    := skip

PKG_BUILD_PARALLEL := 1

# ── Includes ─────────────────────────────────────────────────────────────────
include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

# ── Package metadata ─────────────────────────────────────────────────────────
define Package/ac-client
  SECTION   := net
  CATEGORY  := Network
  SUBMENU   := Management
  TITLE     := USP Agent daemon for OpenWrt APs (TR-369 / USP 1.3)
  URL       := https://github.com/optim-enterprises-bv/ac-client
  DEPENDS   := +libc
  MAINTAINER:= Optim Enterprises BV
endef

define Package/ac-client/description
  Rust daemon implementing the TR-369 / USP 1.3 Agent for OpenWrt access-point
  devices managed by an OptimACS controller (ac-server).  Responsibilities:

    • Device provisioning via mTLS certificate exchange
    • Periodic configuration polling and UCI application
    • Firmware upgrade via sysupgrade
    • ValueChange telemetry heartbeats (uptime, load, GPS, wireless, modem)
    • Axis IP-camera discovery and JPEG upload

  TLS uses TLS 1.3 with an X25519+ML-KEM-768 post-quantum hybrid KEM,
  matching the ac-server rustls-post-quantum configuration.
endef

# Config file is preserved across package upgrades
define Package/ac-client/conffiles
/etc/apclient/ac_client.conf
endef

# ── Additional host build tools ───────────────────────────────────────────────
# aws-lc-sys (pulled in by rustls-post-quantum) needs cmake on the build host.
HOST_BUILD_DEPENDS += cmake/host

# ── Install ───────────────────────────────────────────────────────────────────
define Package/ac-client/install
	# Binary
	$(INSTALL_DIR) $(1)/usr/sbin
	$(INSTALL_BIN) \
		$(PKG_INSTALL_DIR)/usr/bin/ac-client \
		$(1)/usr/sbin/ac-client

	# Default configuration (not installed if already present on device)
	$(INSTALL_DIR) $(1)/etc/apclient
	$(INSTALL_CONF) ./files/ac_client.conf $(1)/etc/apclient/ac_client.conf

	# Certificate directories (empty; populated by server provisioning)
	$(INSTALL_DIR) $(1)/etc/apclient/init
	$(INSTALL_DIR) $(1)/etc/apclient/certs

	# procd init script
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/ac-client.init $(1)/etc/init.d/ac-client

	# Image and firmware scratch directories
	$(INSTALL_DIR) $(1)/var/apclient/images
	$(INSTALL_DIR) $(1)/var/apclient/firmware
endef

define Package/ac-client/postinst
#!/bin/sh
set -e

echo ""
echo "ac-client installed."
echo ""
echo "Before starting, deploy the init certificate from the OptimACS server:"
echo "  scp <server>:<client_dir>/00:00:00:00:00:00/client.crt /etc/apclient/init/"
echo "  scp <server>:<client_dir>/00:00:00:00:00:00/client.key /etc/apclient/init/"
echo "  scp <server>:<ssl.ca_file>                             /etc/apclient/init/ca.crt"
echo ""
echo "Then edit /etc/apclient/ac_client.conf and run:"
echo "  /etc/init.d/ac-client enable"
echo "  /etc/init.d/ac-client start"
echo ""
endef

$(eval $(call RustBinPackage,ac-client))
$(eval $(call BuildPackage,ac-client))

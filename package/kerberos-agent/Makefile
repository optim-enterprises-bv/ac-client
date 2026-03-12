#
# kerberos-agent — Kerberos.io video surveillance agent for OpenWrt
#
# Edge camera agent with ONVIF/RTSP support, motion detection, recording,
# and MQTT integration with OptimACS (via EMQX).
#
# Part of the ac-client OpenWrt feed.  Add to your feeds.conf:
#
#   src-git-full  ac-client  git@github.com:optim-enterprises-bv/ac-client.git
#
# Requirements
# ───────────────────────────────────────────────────────────────────────────
#   • OpenWrt 22.03 or later
#   • Go host toolchain (from packages/lang/golang in the packages feed)
#   • Node.js host toolchain (from packages/lang/node in the packages feed)
#   • ffmpeg (runtime dependency for video processing)
#

include $(TOPDIR)/rules.mk

PKG_NAME    := kerberos-agent
PKG_VERSION := 3.2.3
PKG_RELEASE := 1

PKG_SOURCE_PROTO   := git
PKG_SOURCE_URL     := https://github.com/kerberos-io/agent.git
PKG_SOURCE_VERSION := v$(PKG_VERSION)
PKG_MIRROR_HASH    := skip

PKG_LICENSE      := MIT
PKG_LICENSE_FILES := LICENSE
PKG_MAINTAINER   := Optim Enterprises BV

PKG_BUILD_DEPENDS := golang/host node/host
PKG_BUILD_PARALLEL := 1

GO_PKG           := github.com/kerberos-io/agent/machinery
GO_PKG_BUILD_PKG := $(GO_PKG)
GO_PKG_LDFLAGS_X := \
	$(GO_PKG)/src/models.VERSION=$(PKG_VERSION)

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/golang/golang-package.mk

# ── Package metadata ─────────────────────────────────────────────────────────
define Package/kerberos-agent
  SECTION    := multimedia
  CATEGORY   := Multimedia
  TITLE      := Kerberos.io Video Surveillance Agent
  URL        := https://kerberos.io
  DEPENDS    := $(GO_ARCH_DEPENDS) +ca-bundle +ffmpeg +libstdcpp
  USERID     := kerberos:kerberos
  MAINTAINER := Optim Enterprises BV
endef

define Package/kerberos-agent/description
  Edge video surveillance agent from Kerberos.io.
  Connects to IP cameras via RTSP/ONVIF, performs motion detection,
  records video, and uploads to Kerberos Vault or S3-compatible storage.
  Communicates with the OptimACS platform via MQTT (EMQX).
  Includes a local web UI for per-camera configuration.

  Configured via UCI (/etc/config/kerberos-agent) and manageable from LuCI
  when luci-app-kerberos-agent is installed.
endef

define Package/kerberos-agent/conffiles
/etc/kerberos-agent/config.json
/etc/kerberos-agent/user.json
/etc/config/kerberos-agent
endef

# ── Build the React web UI first, then the Go binary ─────────────────────────
define Build/Compile
	# Build web UI (React → static files)
	cd $(PKG_BUILD_DIR)/ui && \
		npm_config_cache=$(DL_DIR)/npm-cache \
		$(STAGING_DIR_HOSTPKG)/bin/node $(STAGING_DIR_HOSTPKG)/bin/npm install --legacy-peer-deps && \
		$(STAGING_DIR_HOSTPKG)/bin/node $(STAGING_DIR_HOSTPKG)/bin/npm run build

	# Build Go binary (cross-compiled for target arch)
	cd $(PKG_BUILD_DIR)/machinery && \
		$(call GoPackage/Build/Compile)
endef

# ── Install ───────────────────────────────────────────────────────────────────
define Package/kerberos-agent/install
	# Binary
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(GO_PKG_BUILD_BIN_DIR)/machinery $(1)/usr/bin/kerberos-agent

	# Configuration
	$(INSTALL_DIR) $(1)/etc/kerberos-agent
	$(INSTALL_CONF) ./files/config.json $(1)/etc/kerberos-agent/
	$(INSTALL_CONF) ./files/user.json $(1)/etc/kerberos-agent/

	# UCI config
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) ./files/kerberos-agent.uci $(1)/etc/config/kerberos-agent

	# procd init script
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/kerberos-agent.init $(1)/etc/init.d/kerberos-agent

	# Web UI static files
	$(INSTALL_DIR) $(1)/usr/share/kerberos-agent/www
	$(CP) $(PKG_BUILD_DIR)/ui/build/* $(1)/usr/share/kerberos-agent/www/

	# NOTE: /var on OpenWrt is a symlink to /tmp (tmpfs).  Do NOT install
	# directories under /var here.  The init script creates data directories
	# at runtime with mkdir -p before handing control to procd.
endef

define Package/kerberos-agent/postinst
#!/bin/sh
set -e

echo ""
echo "kerberos-agent installed."
echo ""
echo "Configure your camera via UCI:"
echo "  uci set kerberos-agent.camera.rtsp_url='rtsp://192.168.1.100:554/stream1'"
echo "  uci set kerberos-agent.mqtt.uri='tcp://emqx.optimacs:1883'"
echo "  uci set kerberos-agent.vault.uri='http://kerberos-vault.optimacs'"
echo "  uci commit kerberos-agent"
echo ""
echo "Or install luci-app-kerberos-agent for a web UI configuration page."
echo ""
echo "Then enable and start:"
echo "  /etc/init.d/kerberos-agent enable"
echo "  /etc/init.d/kerberos-agent start"
echo ""
echo "Local web UI: http://<device-ip>:8080"
echo ""
endef

$(eval $(call GoBinPackage,kerberos-agent))
$(eval $(call BuildPackage,kerberos-agent))

#
# luci-app-optimacs-cameras — LuCI web interface for OptimACS native NVR cameras
#
# Provides a page under Services → Cameras for configuring:
#   Global     — MQTT, live stream, recording directory, ONVIF discovery
#   Cameras    — Multi-camera RTSP/ONVIF with per-camera recording and motion settings
#
# All settings saved to /etc/config/optimacs (camera + camera_global sections)
#

include $(TOPDIR)/rules.mk

PKG_NAME    := luci-app-optimacs-cameras
PKG_VERSION := 1.0.0
PKG_RELEASE := 1

PKG_BUILD_DIR := $(BUILD_DIR)/$(PKG_NAME)

include $(INCLUDE_DIR)/package.mk

define Package/luci-app-optimacs-cameras
  SECTION    := luci
  CATEGORY   := LuCI
  SUBMENU    := 3. Applications
  TITLE      := LuCI Support for OptimACS Camera Surveillance
  URL        := https://github.com/optim-enterprises-bv/ac-client
  DEPENDS    := +luci-base
  MAINTAINER := Optim Enterprises BV
endef

define Package/luci-app-optimacs-cameras/description
  LuCI web interface for the OptimACS native NVR camera module.

  Provides a Services → Cameras page with:
    Global settings   — MQTT broker, live stream port, recording directory
    Camera instances   — Add/remove cameras with RTSP URL, ONVIF, recording mode,
                         motion threshold, storage limits

  Manages UCI sections in /etc/config/optimacs:
    config camera_global 'camera_global'
    config camera 'cam0' .. 'camN'
endef

# ── Build (nothing to compile — pure JS + JSON) ───────────────────────────────
define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
endef

define Build/Compile
endef

# ── Install ───────────────────────────────────────────────────────────────────
define Package/luci-app-optimacs-cameras/install
	$(INSTALL_DIR) $(1)/usr/share/luci/menu.d
	$(INSTALL_DATA) ./root/usr/share/luci/menu.d/luci-app-optimacs-cameras.json \
		$(1)/usr/share/luci/menu.d/luci-app-optimacs-cameras.json

	$(INSTALL_DIR) $(1)/usr/share/rpcd/acl.d
	$(INSTALL_DATA) ./root/usr/share/rpcd/acl.d/luci-app-optimacs-cameras.json \
		$(1)/usr/share/rpcd/acl.d/luci-app-optimacs-cameras.json

	$(INSTALL_DIR) $(1)/www/luci-static/resources/view/optimacs
	$(INSTALL_DATA) ./root/www/luci-static/resources/view/optimacs/cameras.js \
		$(1)/www/luci-static/resources/view/optimacs/cameras.js
endef

$(eval $(call BuildPackage,luci-app-optimacs-cameras))

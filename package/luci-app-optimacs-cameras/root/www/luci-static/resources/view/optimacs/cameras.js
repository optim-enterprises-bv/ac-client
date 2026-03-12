'use strict';
'require view';
'require form';
'require uci';
'require ui';
'require rpc';
'require request';

var callServiceList = rpc.declare({
	object: 'service',
	method: 'list',
	params: ['name'],
	expect: { '': {} }
});

return view.extend({
	load: function () {
		return Promise.all([
			uci.load('optimacs')
		]);
	},

	/** Run ONVIF discovery via ac-client HTTP endpoint, return results. */
	runDiscovery: function () {
		var port = uci.get('optimacs', 'camera_global', 'live_stream_port') || '8080';
		return request.post('http://127.0.0.1:' + port + '/discover', null, {
			timeout: 15000
		}).then(function (res) {
			if (res.status === 200) {
				try { return JSON.parse(res.responseText); }
				catch (e) { return { cameras: [] }; }
			}
			return { cameras: [] };
		}).catch(function () {
			return { cameras: [] };
		});
	},

	/** Add a discovered camera to UCI config. */
	addDiscoveredCamera: function (cam) {
		// Generate a section ID like cam0, cam1, etc.
		var sections = uci.sections('optimacs', 'camera');
		var nextIdx = 0;
		sections.forEach(function (s) {
			var m = s['.name'].match(/^cam(\d+)$/);
			if (m) nextIdx = Math.max(nextIdx, parseInt(m[1]) + 1);
		});
		var sectionId = 'cam' + nextIdx;
		var name = (cam.name || cam.hardware || 'Camera ' + nextIdx).trim();

		uci.add('optimacs', 'camera', sectionId);
		uci.set('optimacs', sectionId, 'enabled', '1');
		uci.set('optimacs', sectionId, 'name', name);
		uci.set('optimacs', sectionId, 'rtsp_url', cam.xaddr || '');
		uci.set('optimacs', sectionId, 'onvif_enabled', '1');
		uci.set('optimacs', sectionId, 'onvif_xaddr', cam.xaddr || '');
		uci.set('optimacs', sectionId, 'recording_mode', 'motion');

		return uci.save().then(function () {
			ui.addNotification(null, E('p', {},
				_('Added camera "%s" as %s. Click Save & Apply to activate.').format(name, sectionId)),
				'info');
		});
	},

	/** Push discovered cameras to OptimACS NVR server. */
	pushToNVR: function (cameras) {
		var vaultUri = uci.get('optimacs', 'camera_global', 'vault_uri');
		if (!vaultUri) return Promise.resolve();

		var tasks = cameras.map(function (cam) {
			return request.post(vaultUri + '/api/cameras', JSON.stringify({
				name: cam.name || cam.hardware || cam.ip,
				rtsp_url: cam.xaddr || '',
				recording_mode: 'motion',
				motion_threshold: 5
			}), {
				timeout: 10000,
				headers: { 'Content-Type': 'application/json' }
			}).catch(function () { /* best effort */ });
		});
		return Promise.all(tasks);
	},

	render: function () {
		var self = this;
		var m, s, o;

		m = new form.Map('optimacs', _('Camera Surveillance'),
			_('Configure cameras, recording, motion detection, and live streaming. ' +
			  'Changes take effect after saving and restarting the ac-client service.'));

		// ══════════════════════════════════════════════════════════════
		// Global Settings
		// ══════════════════════════════════════════════════════════════

		s = m.section(form.NamedSection, 'camera_global', 'camera_global',
			_('Global Settings'),
			_('Shared settings that apply to all cameras.'));
		s.anonymous = true;
		s.addremove = false;

		s.tab('streaming', _('Streaming'));
		s.tab('mqtt', _('MQTT'));
		s.tab('storage', _('Storage'));
		s.tab('discovery', _('Discovery'));

		// ── Streaming tab ────────────────────────────────────────────

		o = s.taboption('streaming', form.Value, 'live_stream_port', _('Live Stream Port'),
			_('HTTP port for live MJPEG/H.264 streams. Set to 0 to disable.'));
		o.default = '8080';
		o.datatype = 'port';
		o.placeholder = '8080';

		// ── MQTT tab ─────────────────────────────────────────────────

		o = s.taboption('mqtt', form.Value, 'mqtt_uri', _('MQTT Broker URI'),
			_('URI for publishing camera events (motion, recording, status).'));
		o.placeholder = 'mqtt://emqx:1883';

		o = s.taboption('mqtt', form.Value, 'mqtt_topic_prefix', _('Topic Prefix'),
			_('Base prefix for MQTT topics (e.g., cameras/{id}/motion).'));
		o.default = 'cameras';
		o.placeholder = 'cameras';

		// ── Storage tab ──────────────────────────────────────────────

		o = s.taboption('storage', form.Value, 'recording_dir', _('Recording Directory'),
			_('Local filesystem path for storing recordings.'));
		o.default = '/tmp/recordings';
		o.placeholder = '/tmp/recordings';

		o = s.taboption('storage', form.Value, 'vault_uri', _('OptimACS Server URL'),
			_('URL of the OptimACS server for NVR integration. Discovered cameras will be pushed here.'));
		o.placeholder = 'http://optimacs-server:8080';
		o.optional = true;

		o = s.taboption('storage', form.Value, 'vault_access_key', _('Access Key'));
		o.optional = true;

		o = s.taboption('storage', form.Value, 'vault_secret_key', _('Secret Key'));
		o.password = true;
		o.optional = true;

		// ── Discovery tab ────────────────────────────────────────────

		o = s.taboption('discovery', form.Flag, 'discovery_enabled', _('Enable ONVIF Discovery'),
			_('Periodically scan the network for ONVIF-compatible cameras.'));
		o.default = '1';

		o = s.taboption('discovery', form.Value, 'discovery_interval', _('Scan Interval (seconds)'),
			_('How often to scan for new cameras on the network.'));
		o.default = '300';
		o.datatype = 'uinteger';
		o.depends('discovery_enabled', '1');

		// Discovery button (custom DummyValue widget)
		o = s.taboption('discovery', form.DummyValue, '_discover_btn', _('Manual Scan'));
		o.rawhtml = true;
		o.cfgvalue = function () { return ''; };
		o.render = function (option_index, section_id) {
			var btnScan = E('button', {
				'class': 'btn cbi-button cbi-button-action',
				'click': ui.createHandlerFn(self, function () {
					var resultsDiv = document.getElementById('discovery-results');
					if (resultsDiv) resultsDiv.innerHTML = '<em>' + _('Scanning network…') + '</em>';

					return self.runDiscovery().then(function (data) {
						var cameras = data.cameras || [];
						if (!resultsDiv) return;

						if (cameras.length === 0) {
							resultsDiv.innerHTML = '<em>' + _('No cameras found on the network.') + '</em>';
							return;
						}

						var table = E('table', { 'class': 'table' }, [
							E('tr', { 'class': 'tr table-titles' }, [
								E('th', { 'class': 'th' }, _('IP')),
								E('th', { 'class': 'th' }, _('Manufacturer')),
								E('th', { 'class': 'th' }, _('Model')),
								E('th', { 'class': 'th' }, _('ONVIF Address')),
								E('th', { 'class': 'th' }, _('Actions'))
							])
						]);

						cameras.forEach(function (cam) {
							var addBtn = E('button', {
								'class': 'btn cbi-button cbi-button-add',
								'click': ui.createHandlerFn(self, function () {
									return self.addDiscoveredCamera(cam).then(function () {
										addBtn.disabled = true;
										addBtn.textContent = _('Added');
									});
								})
							}, _('Add to UCI'));

							var pushBtn = E('button', {
								'class': 'btn cbi-button cbi-button-action',
								'style': 'margin-left: 4px',
								'click': ui.createHandlerFn(self, function () {
									return self.pushToNVR([cam]).then(function () {
										pushBtn.disabled = true;
										pushBtn.textContent = _('Sent');
										ui.addNotification(null,
											E('p', {}, _('Camera pushed to OptimACS NVR.')),
											'info');
									});
								})
							}, _('Push to NVR'));

							table.appendChild(E('tr', { 'class': 'tr' }, [
								E('td', { 'class': 'td' }, cam.ip || '—'),
								E('td', { 'class': 'td' }, cam.name || '—'),
								E('td', { 'class': 'td' }, cam.hardware || '—'),
								E('td', { 'class': 'td' }, E('code', {}, cam.xaddr || '—')),
								E('td', { 'class': 'td' }, [addBtn, pushBtn])
							]));
						});

						resultsDiv.innerHTML = '';
						resultsDiv.appendChild(
							E('p', {}, _('Found %d camera(s):').format(cameras.length)));
						resultsDiv.appendChild(table);

						// Offer to push all to NVR
						var vaultUri = uci.get('optimacs', 'camera_global', 'vault_uri');
						if (vaultUri && cameras.length > 0) {
							var pushAllBtn = E('button', {
								'class': 'btn cbi-button cbi-button-action',
								'style': 'margin-top: 8px',
								'click': ui.createHandlerFn(self, function () {
									return self.pushToNVR(cameras).then(function () {
										pushAllBtn.disabled = true;
										pushAllBtn.textContent = _('All sent to NVR');
									});
								})
							}, _('Push All to NVR'));
							resultsDiv.appendChild(pushAllBtn);
						}
					});
				})
			}, _('Scan Now'));

			return E('div', {}, [
				btnScan,
				E('div', { 'id': 'discovery-results', 'style': 'margin-top: 10px' })
			]);
		};

		// ══════════════════════════════════════════════════════════════
		// Camera Instances (dynamic — add/remove)
		// ══════════════════════════════════════════════════════════════

		s = m.section(form.TypedSection, 'camera',
			_('Cameras'),
			_('Add and configure individual cameras. Each camera needs at minimum an RTSP URL. ' +
			  'Use the Discovery scan above to find cameras automatically.'));
		s.anonymous = false;
		s.addremove = true;
		s.addbtntitle = _('Add Camera');

		s.tab('general', _('General'));
		s.tab('recording', _('Recording'));
		s.tab('motion', _('Motion Detection'));
		s.tab('onvif', _('ONVIF / PTZ'));

		// ── General tab ──────────────────────────────────────────────

		o = s.taboption('general', form.Flag, 'enabled', _('Enabled'),
			_('Enable this camera.'));
		o.default = '1';
		o.rmempty = false;

		o = s.taboption('general', form.Value, 'name', _('Camera Name'),
			_('Friendly display name (e.g., Front Door, Parking Lot).'));
		o.rmempty = false;

		o = s.taboption('general', form.Value, 'rtsp_url', _('RTSP URL'),
			_('Main stream URL (high resolution) for recording and live view.'));
		o.placeholder = 'rtsp://192.168.1.100:554/stream1';
		o.rmempty = false;

		o = s.taboption('general', form.Value, 'sub_rtsp_url', _('Sub-stream URL'),
			_('Low-resolution stream for motion detection (saves CPU). Leave empty to use main stream.'));
		o.placeholder = 'rtsp://192.168.1.100:554/stream2';
		o.optional = true;

		o = s.taboption('general', form.Value, 'rtsp_username', _('RTSP Username'),
			_('Username for RTSP authentication. If empty, falls back to ONVIF username.'));
		o.optional = true;

		o = s.taboption('general', form.Value, 'rtsp_password', _('RTSP Password'),
			_('Password for RTSP authentication. If empty, falls back to ONVIF password.'));
		o.password = true;
		o.optional = true;

		// ── Recording tab ────────────────────────────────────────────

		o = s.taboption('recording', form.ListValue, 'recording_mode', _('Recording Mode'));
		o.value('motion', _('Motion — Record only when motion detected'));
		o.value('continuous', _('Continuous — Always record'));
		o.value('disabled', _('Disabled — No recording'));
		o.default = 'motion';

		o = s.taboption('recording', form.Value, 'prerecording_secs', _('Pre-recording (seconds)'),
			_('Buffer of frames to include before motion event starts.'));
		o.default = '3';
		o.datatype = 'uinteger';
		o.depends('recording_mode', 'motion');

		o = s.taboption('recording', form.Value, 'postrecording_secs', _('Post-recording (seconds)'),
			_('Continue recording after motion stops.'));
		o.default = '10';
		o.datatype = 'uinteger';
		o.depends('recording_mode', 'motion');

		o = s.taboption('recording', form.Value, 'max_recording_secs', _('Max Recording Duration (seconds)'),
			_('Split recordings at this duration.'));
		o.default = '30';
		o.datatype = 'uinteger';

		o = s.taboption('recording', form.Flag, 'auto_clean', _('Auto-clean Old Recordings'),
			_('Automatically delete oldest recordings when storage limit is reached.'));
		o.default = '1';

		o = s.taboption('recording', form.Value, 'max_storage_mb', _('Max Storage (MB)'),
			_('Maximum disk space for this camera\'s recordings.'));
		o.default = '500';
		o.datatype = 'uinteger';

		// ── Motion Detection tab ─────────────────────────────────────

		o = s.taboption('motion', form.Value, 'pixel_threshold', _('Pixel Threshold'),
			_('Minimum pixel change value (0–255) to consider as motion. Lower = more sensitive.'));
		o.default = '150';
		o.datatype = 'range(1, 255)';

		// ── ONVIF / PTZ tab ──────────────────────────────────────────

		o = s.taboption('onvif', form.Flag, 'onvif_enabled', _('Enable ONVIF'),
			_('Use ONVIF protocol for PTZ control and camera discovery.'));
		o.default = '0';

		o = s.taboption('onvif', form.Value, 'onvif_xaddr', _('ONVIF Service Address'),
			_('URL of the camera\'s ONVIF device service.'));
		o.placeholder = 'http://192.168.1.100/onvif/device_service';
		o.depends('onvif_enabled', '1');

		o = s.taboption('onvif', form.Value, 'onvif_username', _('ONVIF Username'));
		o.default = 'admin';
		o.depends('onvif_enabled', '1');

		o = s.taboption('onvif', form.Value, 'onvif_password', _('ONVIF Password'));
		o.password = true;
		o.depends('onvif_enabled', '1');

		return m.render();
	}
});

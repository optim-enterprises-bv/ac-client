'use strict';
'require view';
'require form';
'require uci';

/*
 * OptimACS ac-client — LuCI Configuration Page
 *
 * All settings map to /etc/config/optimacs, section 'agent'.
 * The form auto-saves to UCI when the user clicks "Save & Apply".
 *
 * Tab layout
 * ──────────
 *  1. Connection   – server host/port, MTP selection, WebSocket & MQTT URLs,
 *                    controller endpoint ID, TLS server CN
 *  2. TLS          – CA cert, bootstrap cert+key, provisioned cert+key, cert dir
 *  3. Device       – MAC address, USP endpoint ID, CPU arch, device model
 *  4. Intervals    – status heartbeat, config-poll, camera-scan periods
 *  5. GNSS / GPS   – serial device, baud rate (dropdown)
 *  6. Storage      – firmware dir, image dir, PID file
 *  7. Process      – syslog flag
 */

return view.extend({

	render: function() {
		var m, s, o;

		// ── Map (/etc/config/optimacs) ────────────────────────────────────────
		m = new form.Map('optimacs',
			_('OptimACS Agent Configuration'),
			_('Configure the ac-client USP Agent (TR-369 / USP 1.3). ' +
			  'All settings are saved to <code>/etc/config/optimacs</code> via UCI. ' +
			  'Changes take effect after the service is restarted.'));

		// ── Section: named section 'agent' ────────────────────────────────────
		s = m.section(form.NamedSection, 'agent', 'optimacs');
		s.addremove = false;
		s.anonymous = false;

		// Declare tabs
		s.tab('connection', _('Connection'));
		s.tab('tls',        _('TLS / Certs'));
		s.tab('device',     _('Device'));
		s.tab('intervals',  _('Intervals'));
		s.tab('gnss',       _('GNSS / GPS'));
		s.tab('paths',      _('Storage'));
		s.tab('process',    _('Process'));

		// ╔══════════════════════════════════════════════════════════════════════╗
		// ║  TAB 1 — Connection                                                  ║
		// ╚══════════════════════════════════════════════════════════════════════╝

		o = s.taboption('connection', form.Value, 'server_host',
			_('Controller Host'),
			_('Hostname or IP address of the OptimACS server. ' +
			  'Used to build the WebSocket URL when <em>WebSocket URL</em> is left empty.'));
		o.placeholder = 'controller.example.com';
		o.rmempty     = false;
		o.datatype    = 'host';

		o = s.taboption('connection', form.Value, 'server_port',
			_('Controller Port'),
			_('TCP port for the USP WebSocket MTP (default <strong>3491</strong>). ' +
			  'Also used as fallback when <em>WebSocket URL</em> is empty.'));
		o.placeholder = '3491';
		o.datatype    = 'port';

		o = s.taboption('connection', form.ListValue, 'mtp',
			_('Transport Protocol (MTP)'),
			_('USP Message Transport Protocol to use. ' +
			  '<strong>WebSocket</strong> is the default and recommended option.'));
		o.value('websocket', _('WebSocket (WSS)'));
		o.value('mqtt',      _('MQTT'));
		o.value('both',      _('Both — WebSocket + MQTT'));
		o.default = 'websocket';

		o = s.taboption('connection', form.Value, 'ws_url',
			_('WebSocket URL'),
			_('Direct WSS URL to the ac-server WebSocket MTP endpoint. ' +
			  'Auto-built from <em>Controller Host</em> + <em>Controller Port</em> ' +
			  'when empty. Example: <code>wss://controller.example.com:3491/usp</code>'));
		o.placeholder = 'wss://controller.example.com:3491/usp';
		o.depends('mtp', 'websocket');
		o.depends('mtp', 'both');

		o = s.taboption('connection', form.Value, 'mqtt_url',
			_('MQTT Broker URL'),
			_('URL of the EMQX (or other) MQTT broker. ' +
			  'Required when MTP is <em>MQTT</em> or <em>Both</em>. ' +
			  'Example: <code>mqtt://emqx.example.com:1883</code>'));
		o.placeholder = 'mqtt://emqx.example.com:1883';
		o.depends('mtp', 'mqtt');
		o.depends('mtp', 'both');

		o = s.taboption('connection', form.Value, 'mqtt_client_id',
			_('MQTT Client ID'),
			_('Client identifier sent to the MQTT broker. ' +
			  'Auto-generated from the USP endpoint ID when empty.'));
		o.placeholder = _('(auto from endpoint ID)');
		o.depends('mtp', 'mqtt');
		o.depends('mtp', 'both');

		o = s.taboption('connection', form.Value, 'controller_id',
			_('Controller Endpoint ID'),
			_('USP endpoint ID of the OptimACS controller. ' +
			  'The agent only processes messages from this endpoint.'));
		o.placeholder = 'oui:00005A:OptimACS-Controller-1';

		o = s.taboption('connection', form.Value, 'server_cn',
			_('Server TLS Common Name (SNI)'),
			_('Expected CN in the server\'s TLS certificate. ' +
			  'Used for SNI and certificate verification. ' +
			  'Defaults to <code>ac-server</code> when empty.'));
		o.placeholder = 'ac-server';

		// ╔══════════════════════════════════════════════════════════════════════╗
		// ║  TAB 2 — TLS / Certificates                                          ║
		// ╚══════════════════════════════════════════════════════════════════════╝

		o = s.taboption('tls', form.Value, 'ca_file',
			_('CA Certificate'),
			_('Root CA certificate used to verify the server\'s TLS certificate. ' +
			  'This file is issued by your step-ca instance.'));
		o.placeholder = '/etc/apclient/init/ca.crt';

		o = s.taboption('tls', form.Value, 'init_cert',
			_('Bootstrap Certificate'),
			_('Client certificate used <em>before</em> the device is provisioned ' +
			  '(CN <code>00:00:00:00:00:00</code>). Provided by the server ' +
			  'from <code>client_dir/00:00:00:00:00:00/client.crt</code>.'));
		o.placeholder = '/etc/apclient/init/client.crt';

		o = s.taboption('tls', form.Value, 'init_key',
			_('Bootstrap Private Key'),
			_('Private key matching the bootstrap certificate.'));
		o.placeholder = '/etc/apclient/init/client.key';

		o = s.taboption('tls', form.Value, 'cert_file',
			_('Provisioned Certificate'),
			_('Per-device mTLS certificate issued by the server after provisioning. ' +
			  'Written here automatically during the CERT exchange.'));
		o.placeholder = '/etc/apclient/certs/client.crt';

		o = s.taboption('tls', form.Value, 'key_file',
			_('Provisioned Private Key'),
			_('Private key matching the provisioned client certificate.'));
		o.placeholder = '/etc/apclient/certs/client.key';

		o = s.taboption('tls', form.Value, 'cert_dir',
			_('Certificate Directory'),
			_('Directory where provisioned certificate files are stored.'));
		o.placeholder = '/etc/apclient/certs';

		// ╔══════════════════════════════════════════════════════════════════════╗
		// ║  TAB 3 — Device Identity                                             ║
		// ╚══════════════════════════════════════════════════════════════════════╝

		o = s.taboption('device', form.Value, 'mac_addr',
			_('MAC Address'),
			_('Device MAC address included in INIT packets and used as the ' +
			  'device CN. Auto-detected from <code>br-lan</code> / ' +
			  '<code>eth0</code> / <code>wlan0</code> when empty.'));
		o.placeholder = _('(auto-detect)');

		o = s.taboption('device', form.Value, 'usp_endpoint_id',
			_('USP Endpoint ID'),
			_('Agent endpoint ID sent in every USP Record. ' +
			  'Auto-generated as <code>oui:&lt;OUI&gt;:&lt;MAC&gt;</code> from ' +
			  'the MAC address when empty.'));
		o.placeholder = _('(auto from MAC)');

		o = s.taboption('device', form.Value, 'arch',
			_('CPU Architecture'),
			_('Architecture string reported to the server ' +
			  '(e.g. <code>mipsel_24kc</code>, <code>aarch64_cortex-a53</code>). ' +
			  'Auto-detected when empty.'));
		o.placeholder = _('(auto-detect, e.g. mipsel_24kc)');

		o = s.taboption('device', form.Value, 'sys_model',
			_('Device Model'),
			_('Model string reported to the server ' +
			  '(e.g. <code>tplink-c7</code>, <code>dir300</code>). ' +
			  'Auto-detected when empty.'));
		o.placeholder = _('(auto-detect, e.g. tplink-c7)');

		// ╔══════════════════════════════════════════════════════════════════════╗
		// ║  TAB 4 — Intervals                                                   ║
		// ╚══════════════════════════════════════════════════════════════════════╝

		o = s.taboption('intervals', form.Value, 'status_interval',
			_('Status Heartbeat (seconds)'),
			_('How often ac-client sends a USP ValueChange NOTIFY with ' +
			  'uptime, load average, free memory, WiFi, and GPS data.'));
		o.datatype    = 'uinteger';
		o.placeholder = '300';

		o = s.taboption('intervals', form.Value, 'update_interval',
			_('Config Poll Interval (seconds)'),
			_('How often ac-client polls the OptimACS server for pending ' +
			  'configuration changes.'));
		o.datatype    = 'uinteger';
		o.placeholder = '60';

		o = s.taboption('intervals', form.Value, 'cam_interval',
			_('Camera Scan Interval (seconds)'),
			_('How often ac-client runs the Axis IP-camera discovery and ' +
			  'JPEG capture cycle. Set to 0 to disable camera support.'));
		o.datatype    = 'uinteger';
		o.placeholder = '360';

		// ╔══════════════════════════════════════════════════════════════════════╗
		// ║  TAB 5 — GNSS / GPS                                                  ║
		// ╚══════════════════════════════════════════════════════════════════════╝

		o = s.taboption('gnss', form.Value, 'gnss_dev',
			_('GPS Serial Device'),
			_('Path to the serial device for a NMEA-0183 GPS receiver. ' +
			  'Leave empty to disable GPS position reporting. ' +
			  'Example: <code>/dev/ttyUSB0</code>, <code>/dev/ttyS1</code>'));
		o.placeholder = '/dev/ttyUSB0';

		o = s.taboption('gnss', form.ListValue, 'gnss_baud',
			_('GPS Baud Rate'),
			_('Serial baud rate for the GPS receiver. Most NMEA receivers use 9600 bps.'));
		o.value('4800',   '4800 bps');
		o.value('9600',   '9600 bps  (default)');
		o.value('19200',  '19200 bps');
		o.value('38400',  '38400 bps');
		o.value('57600',  '57600 bps');
		o.value('115200', '115200 bps');
		o.default = '9600';

		// ╔══════════════════════════════════════════════════════════════════════╗
		// ║  TAB 6 — Storage Paths                                               ║
		// ╚══════════════════════════════════════════════════════════════════════╝

		o = s.taboption('paths', form.Value, 'fw_dir',
			_('Firmware Download Directory'),
			_('Scratch directory used to store firmware images downloaded from ' +
			  'the server before applying them via <code>sysupgrade</code>. ' +
			  'Must have enough free space to hold a full firmware image.'));
		o.placeholder = '/tmp/firmware';

		o = s.taboption('paths', form.Value, 'img_dir',
			_('Camera Image Directory'),
			_('Directory where JPEG snapshots captured from Axis IP cameras are stored.'));
		o.placeholder = '/var/apclient/images';

		o = s.taboption('paths', form.Value, 'pid_file',
			_('PID File'),
			_('File written by the daemon at startup containing its process ID. ' +
			  'Used by the init script for service management.'));
		o.placeholder = '/var/run/apclient.pid';

		// ╔══════════════════════════════════════════════════════════════════════╗
		// ║  TAB 7 — Process                                                     ║
		// ╚══════════════════════════════════════════════════════════════════════╝

		o = s.taboption('process', form.Flag, 'log_syslog',
			_('Log to Syslog'),
			_('Send all log messages to the system log daemon (<code>logd</code>). ' +
			  'Disable to write logs to <strong>stderr</strong> instead — ' +
			  'useful when running ac-client interactively for debugging.'));
		o.enabled  = '1';
		o.disabled = '0';
		o.default  = '1';

		return m.render();
	}
});

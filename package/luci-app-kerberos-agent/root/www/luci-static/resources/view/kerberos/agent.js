'use strict';
'require view';
'require form';
'require uci';

return view.extend({
	render: function () {
		var m, s, o;

		m = new form.Map('kerberos-agent', _('Kerberos Agent'),
			_('Edge video surveillance agent with ONVIF/RTSP camera support, ' +
			  'motion detection, recording, and MQTT integration with OptimACS.'));

		// ── Main Settings ─────────────────────────────────────────
		s = m.section(form.NamedSection, 'main', 'main', _('General'));
		s.anonymous = true;

		o = s.option(form.Flag, 'enabled', _('Enable'),
			_('Start the Kerberos Agent service'));
		o.default = '1';
		o.rmempty = false;

		o = s.option(form.Value, 'port', _('Web UI Port'),
			_('HTTP port for the agent web interface (avoid 80 — used by LuCI)'));
		o.default = '8080';
		o.datatype = 'port';

		o = s.option(form.Value, 'name', _('Agent Name'),
			_('Unique identifier for this agent instance'));
		o.default = 'agent';

		// ── Camera ────────────────────────────────────────────────
		s = m.section(form.NamedSection, 'camera', 'camera', _('Camera'));
		s.anonymous = true;

		o = s.option(form.Value, 'rtsp_url', _('RTSP URL'),
			_('Main RTSP stream URL (e.g., rtsp://192.168.1.100:554/stream1)'));
		o.placeholder = 'rtsp://192.168.1.100:554/stream1';

		o = s.option(form.Flag, 'onvif_enabled', _('Enable ONVIF'),
			_('Use ONVIF for camera discovery and PTZ control'));
		o.default = '0';

		o = s.option(form.Value, 'onvif_xaddr', _('ONVIF Address'),
			_('ONVIF device service endpoint'));
		o.placeholder = 'http://192.168.1.100:80/onvif/device_service';
		o.depends('onvif_enabled', '1');

		o = s.option(form.Value, 'onvif_username', _('ONVIF Username'));
		o.default = 'admin';
		o.depends('onvif_enabled', '1');

		o = s.option(form.Value, 'onvif_password', _('ONVIF Password'));
		o.password = true;
		o.depends('onvif_enabled', '1');

		// ── MQTT ──────────────────────────────────────────────────
		s = m.section(form.NamedSection, 'mqtt', 'mqtt', _('MQTT'));
		s.anonymous = true;

		o = s.option(form.Value, 'uri', _('Broker URI'),
			_('MQTT broker address for OptimACS integration'));
		o.placeholder = 'tcp://emqx.optimacs:1883';

		o = s.option(form.Value, 'username', _('Username'));
		o.optional = true;

		o = s.option(form.Value, 'password', _('Password'));
		o.password = true;
		o.optional = true;

		// ── Vault Storage ─────────────────────────────────────────
		s = m.section(form.NamedSection, 'vault', 'vault', _('Recording Storage'));
		s.anonymous = true;

		o = s.option(form.Value, 'uri', _('Kerberos Vault URI'),
			_('Vault API endpoint for uploading recordings'));
		o.placeholder = 'http://kerberos-vault.optimacs';

		o = s.option(form.Value, 'access_key', _('Access Key'));
		o.optional = true;

		o = s.option(form.Value, 'secret_key', _('Secret Key'));
		o.password = true;
		o.optional = true;

		return m.render();
	}
});

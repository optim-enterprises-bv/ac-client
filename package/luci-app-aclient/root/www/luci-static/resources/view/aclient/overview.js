'use strict';
'require view';
'require rpc';
'require uci';
'require ui';
'require poll';

// ── rpcd helpers ──────────────────────────────────────────────────────────────

/**
 * Query procd for the running state of a service.
 * Returns an object like:
 *   { "ac-client": { "instances": { "instance1": { "running": true, "pid": 123 } } } }
 */
var callServiceList = rpc.declare({
	object: 'service',
	method: 'list',
	params: [ 'name' ],
	expect: { '': {} }
});

/**
 * Query /etc/init.d enabled / disabled status via the rc rpcd object.
 * Returns an object keyed by service name: { "ac-client": { "enabled": true } }
 */
var callRcList = rpc.declare({
	object: 'rc',
	method: 'list',
	expect: { '': {} }
});

/**
 * Trigger an init.d action (start | stop | restart | enable | disable).
 */
var callRcAction = rpc.declare({
	object: 'rc',
	method: 'call',
	params: [ 'name', 'action' ],
	expect: { result: 0 }
});

// ── helpers ───────────────────────────────────────────────────────────────────

function isRunning(svcList) {
	var svc = svcList['ac-client'];
	if (!svc || !svc.instances) return false;
	return Object.values(svc.instances).some(function(inst) {
		return inst.running === true;
	});
}

function getPid(svcList) {
	var svc = svcList['ac-client'];
	if (!svc || !svc.instances) return null;
	for (var inst of Object.values(svc.instances)) {
		if (inst.running && inst.pid) return inst.pid;
	}
	return null;
}

// ── View ──────────────────────────────────────────────────────────────────────

return view.extend({

	/* Called once on page load. */
	load: function() {
		return Promise.all([
			callServiceList('ac-client'),
			callRcList(),
			uci.load('optimacs')
		]);
	},

	/* Service control button handler. */
	handleAction: function(action) {
		var self = this;
		return callRcAction('ac-client', action)
			.then(function(rc) {
				if (rc !== 0) {
					ui.addNotification(null,
						E('p', _('Action "%s" failed (rc=%d).').format(action, rc)),
						'danger');
				} else {
					ui.addNotification(null,
						E('p', _('Service action "%s" completed.').format(action)));
				}
				/* Re-render the page after a short delay so procd state settles. */
				return new Promise(function(resolve) { setTimeout(resolve, 800); })
					.then(function() { return self.load(); })
					.then(function(data) { return self.refreshOverview(data); });
			})
			.catch(function(err) {
				ui.addNotification(null,
					E('p', _('Error: %s').format(err.message || String(err))),
					'danger');
			});
	},

	/* Replace the overview content without a full page reload. */
	refreshOverview: function(data) {
		var container = document.getElementById('aclient-overview');
		if (container) {
			var newContent = this.buildOverview(data[0], data[1]);
			container.replaceChildren(newContent);
		}
	},

	/* Build the overview DOM tree from fresh data. */
	buildOverview: function(svcList, rcList) {
		var self        = this;
		var running     = isRunning(svcList);
		var pid         = getPid(svcList);
		var enabledOnBoot = rcList['ac-client'] ? rcList['ac-client'].enabled : false;

		/* Read current UCI settings for the connection summary. */
		var serverHost     = uci.get('optimacs', 'agent', 'server_host')     || '—';
		var serverPort     = uci.get('optimacs', 'agent', 'server_port')     || '3490';
		var mtp            = uci.get('optimacs', 'agent', 'mtp')             || 'websocket';
		var wsUrl          = uci.get('optimacs', 'agent', 'ws_url')          || '';
		var mqttUrl        = uci.get('optimacs', 'agent', 'mqtt_url')        || '';
		var controllerID   = uci.get('optimacs', 'agent', 'controller_id')   || 'oui:00005A:OptimACS-Controller-1';
		var endpointID     = uci.get('optimacs', 'agent', 'usp_endpoint_id') || _('(auto from MAC)');
		var statusInterval = uci.get('optimacs', 'agent', 'status_interval') || '300';
		var updateInterval = uci.get('optimacs', 'agent', 'update_interval') || '60';

		/* Derive display URL. */
		var displayWsUrl = wsUrl ||
			(serverHost !== '—' ? 'wss://' + serverHost + ':3491/usp' : '—');

		/* ── Status badge ── */
		var statusBadge = E('span', {
			'class': running
				? 'badge label-status ok'
				: 'badge label-status notok'
		}, running ? _('Running') : _('Stopped'));

		var pidBadge = running && pid
			? E('span', { 'style': 'margin-left:8px; color:#888; font-size:0.9em' },
				_('PID %d').format(pid))
			: '';

		var bootBadge = E('span', {
			'class': enabledOnBoot
				? 'badge label-status ok'
				: 'badge label-status off'
		}, enabledOnBoot ? _('Enabled at boot') : _('Disabled at boot'));

		/* ── MTP display label ── */
		var mtpLabels = {
			'websocket': 'WebSocket (WSS)',
			'mqtt':      'MQTT',
			'both':      'WebSocket + MQTT'
		};
		var mtpDisplay = mtpLabels[mtp] || mtp.toUpperCase();

		/* ── Control buttons ── */
		var btnStart = E('button', {
			'class': 'btn cbi-button cbi-button-positive',
			'click': function() { return self.handleAction('start'); },
			'disabled': running ? true : null
		}, _('Start'));

		var btnStop = E('button', {
			'class': 'btn cbi-button cbi-button-negative',
			'click': function() { return self.handleAction('stop'); },
			'disabled': !running ? true : null
		}, _('Stop'));

		var btnRestart = E('button', {
			'class': 'btn cbi-button cbi-button-action',
			'click': function() { return self.handleAction('restart'); }
		}, _('Restart'));

		var btnEnable = E('button', {
			'class': 'btn cbi-button cbi-button-positive',
			'click': function() { return self.handleAction('enable'); },
			'disabled': enabledOnBoot ? true : null
		}, _('Enable at Boot'));

		var btnDisable = E('button', {
			'class': 'btn cbi-button cbi-button-negative',
			'click': function() { return self.handleAction('disable'); },
			'disabled': !enabledOnBoot ? true : null
		}, _('Disable at Boot'));

		/* ── Status table rows helper ── */
		function row(label, value) {
			return E('tr', { 'class': 'tr' }, [
				E('td', { 'class': 'td col-xs-5 left', 'style': 'font-weight:600' }, label),
				E('td', { 'class': 'td' }, value)
			]);
		}

		/* ── Assemble DOM ── */
		return E([], [
			/* Status card */
			E('div', { 'class': 'cbi-section' }, [
				E('h3', {}, _('Service Status')),
				E('div', { 'class': 'cbi-section-node' }, [
					E('table', { 'class': 'table cbi-section-table' }, [
						row(_('Status'),              [ statusBadge, pidBadge ]),
						row(_('Boot behaviour'),      bootBadge),
						row(_('Controller Host'),     serverHost),
						row(_('Server Port'),         serverPort),
						row(_('Transport (MTP)'),     mtpDisplay),
						(mtp === 'websocket' || mtp === 'both')
							? row(_('WebSocket URL'),  displayWsUrl)
							: '',
						(mtp === 'mqtt' || mtp === 'both')
							? row(_('MQTT Broker'),    mqttUrl || '—')
							: '',
						row(_('Controller Endpoint ID'), controllerID),
						row(_('Agent Endpoint ID'),   endpointID),
						row(_('Status Heartbeat'),    statusInterval + ' s'),
						row(_('Config Poll'),         updateInterval + ' s')
					])
				])
			]),

			/* Controls card */
			E('div', { 'class': 'cbi-section' }, [
				E('h3', {}, _('Service Control')),
				E('div', { 'class': 'cbi-section-node' }, [
					E('div', { 'class': 'cbi-value' }, [
						btnStart, ' ', btnStop, ' ', btnRestart
					]),
					E('div', { 'class': 'cbi-value', 'style': 'margin-top:8px' }, [
						btnEnable, ' ', btnDisable
					])
				])
			])
		]);
	},

	/* Main render function. */
	render: function(data) {
		var svcList = data[0];
		var rcList  = data[1];

		return E('div', { 'class': 'cbi-map' }, [
			E('h2', {}, _('OptimACS USP Agent')),
			E('div', { 'id': 'aclient-overview' }, [
				this.buildOverview(svcList, rcList)
			])
		]);
	},

	handleSaveApply: null,
	handleSave:      null,
	handleReset:     null
});

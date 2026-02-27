//! USP MQTT MTP — agent side (connects to EMQX broker).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{debug, error, info, warn};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};

use crate::config::ClientConfig;
use super::super::{
    endpoint::EndpointId,
    message::decode_msg,
    record::{decode_record, encode_record, extract_msg_payload, mqtt_connect_record, no_session_record},
    usp_record::record::RecordType,
};

const RECONNECT_DELAY: Duration = Duration::from_secs(10);
const MAX_PACKET_SIZE: usize = 4 * 1024 * 1024;

fn sanitise_topic(s: &str) -> String {
    s.replace(':', "%3A").replace('#', "%23").replace('+', "%2B")
}

pub async fn run(cfg: Arc<ClientConfig>, agent_id: EndpointId) {
    let negotiated_ver: Arc<Mutex<String>> = Arc::new(Mutex::new("1.3".into()));
    loop {
        let mqtt_url = match &cfg.mqtt_url {
            Some(u) => u.clone(),
            None => { info!("MQTT MTP disabled"); return; }
        };
        info!("USP MQTT: connecting to {mqtt_url}");
        match mqtt_loop(cfg.clone(), agent_id.clone(), &mqtt_url, Arc::clone(&negotiated_ver)).await {
            Ok(()) => {}
            Err(e) => { error!("MQTT MTP error: {e}"); }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn mqtt_loop(
    cfg:            Arc<ClientConfig>,
    agent_id:       EndpointId,
    mqtt_url:       &str,
    negotiated_ver: Arc<Mutex<String>>,
) -> anyhow::Result<()> {
    let url = mqtt_url
        .trim_start_matches("mqtt://")
        .trim_start_matches("mqtts://");
    let (host, port) = if let Some((h, p)) = url.split_once(':') {
        (h.to_string(), p.parse::<u16>().unwrap_or(1883))
    } else {
        (url.to_string(), 1883)
    };

    let client_id = sanitise_topic(agent_id.as_str());
    let mut opts = MqttOptions::new(&client_id, &host, port);
    opts.set_keep_alive(Duration::from_secs(60));
    opts.set_max_packet_size(MAX_PACKET_SIZE, MAX_PACKET_SIZE);

    let (client, mut event_loop) = AsyncClient::new(opts, 128);

    // Subscribe to our own agent topic
    let agent_topic = format!("usp/v1/agent/{}", sanitise_topic(agent_id.as_str()));
    client.subscribe(&agent_topic, QoS::AtLeastOnce).await?;

    // Send MQTTConnectRecord to identify ourselves to the controller
    let controller_id = &cfg.controller_id;
    let controller_topic = format!("usp/v1/controller/{}", sanitise_topic(controller_id));
    let connect_rec = mqtt_connect_record(agent_id.as_str(), controller_id, &agent_topic);
    let connect_bytes = encode_record(&connect_rec)?;
    client.publish(&controller_topic, QoS::AtLeastOnce, false, connect_bytes).await?;

    info!("USP MQTT: connected; subscribed to {agent_topic}");

    loop {
        let event = event_loop.poll().await?;
        if let Event::Incoming(Packet::Publish(pub_msg)) = event {
            let payload = pub_msg.payload.to_vec();
            let record = match decode_record(&payload) {
                Ok(r)  => r,
                Err(e) => { warn!("MQTT: bad record: {e}"); continue; }
            };
            // TR-369 §5.1: discard records not addressed to this endpoint
            if !record.to_id.is_empty() && record.to_id != agent_id.as_str() {
                warn!("MQTT: to_id={} mismatch (expected {}), discarding",
                      record.to_id, agent_id.as_str());
                continue;
            }
            let msg_bytes = match extract_msg_payload(&record) {
                Some(b) => b.to_vec(),
                None    => continue,
            };
            if let Some(resp) = super::super::agent::handle_incoming(
                cfg.clone(), agent_id.clone(), &msg_bytes, Arc::clone(&negotiated_ver)
            ).await {
                let ver = negotiated_ver.lock().unwrap().clone();
                let resp_rec = no_session_record(agent_id.as_str(), &record.from_id, resp, &ver);
                if let Ok(encoded) = encode_record(&resp_rec) {
                    let _ = client.publish(&controller_topic, QoS::AtLeastOnce, false, encoded).await;
                }
            }
        }
    }
}

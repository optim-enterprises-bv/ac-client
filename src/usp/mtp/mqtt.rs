//! USP MQTT MTP — agent side (connects to EMQX broker).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{debug, error, info, trace, warn};
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};

use crate::config::ClientConfig;
use super::super::{
    endpoint::EndpointId,
    record::{decode_record, encode_record, extract_msg_payload, mqtt_connect_record, no_session_record},
};

const RECONNECT_DELAY: Duration = Duration::from_secs(10);
const MAX_PACKET_SIZE: usize = 4 * 1024 * 1024;

fn sanitise_topic(s: &str) -> String {
    s.replace(':', "%3A").replace('#', "%23").replace('+', "%2B")
}

pub async fn run(cfg: Arc<ClientConfig>, agent_id: EndpointId) {
    debug!("Starting MQTT MTP run loop for agent: {}", agent_id.as_str());
    let negotiated_ver: Arc<Mutex<String>> = Arc::new(Mutex::new("1.3".into()));
    
    loop {
        let mqtt_url = match &cfg.mqtt_url {
            Some(u) => {
                debug!("MQTT URL configured: {}", u);
                u.clone()
            }
            None => { 
                warn!("MQTT MTP disabled (no mqtt_url configured)");
                return; 
            }
        };
        
        info!("USP MQTT: connecting to {mqtt_url}");
        debug!("Starting mqtt_loop with agent_id={}", agent_id.as_str());
        
        match mqtt_loop(cfg.clone(), agent_id.clone(), &mqtt_url, Arc::clone(&negotiated_ver)).await {
            Ok(()) => {
                debug!("MQTT loop ended normally");
            }
            Err(e) => { 
                error!("MQTT MTP error: {e}");
                debug!("MQTT error details: {:?}", e);
            }
        }
        
        warn!("MQTT: reconnecting in {} seconds...", RECONNECT_DELAY.as_secs());
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn mqtt_loop(
    cfg:            Arc<ClientConfig>,
    agent_id:       EndpointId,
    mqtt_url:       &str,
    negotiated_ver: Arc<Mutex<String>>,
) -> anyhow::Result<()> {
    debug!("Parsing MQTT URL: {}", mqtt_url);
    let url = mqtt_url
        .trim_start_matches("mqtt://")
        .trim_start_matches("mqtts://");
    let (host, port) = if let Some((h, p)) = url.split_once(':') {
        let port_num = p.parse::<u16>().unwrap_or(1883);
        debug!("Parsed MQTT broker: {}: {}", h, port_num);
        (h.to_string(), port_num)
    } else {
        debug!("Parsed MQTT broker: {}:1883 (default port)", url);
        (url.to_string(), 1883)
    };

    let client_id = sanitise_topic(agent_id.as_str());
    debug!("MQTT client ID: {}", client_id);
    
    let mut opts = MqttOptions::new(&client_id, &host, port);
    opts.set_keep_alive(Duration::from_secs(60));
    opts.set_max_packet_size(MAX_PACKET_SIZE, MAX_PACKET_SIZE);
    debug!("MQTT options configured: keep_alive=60s, max_packet_size={}", MAX_PACKET_SIZE);

    let (client, mut event_loop) = AsyncClient::new(opts, 128);
    debug!("MQTT client created");

    // Subscribe to our own agent topic
    let agent_topic = format!("usp/v1/agent/{}", sanitise_topic(agent_id.as_str()));
    debug!("Subscribing to agent topic: {}", agent_topic);
    client.subscribe(&agent_topic, QoS::AtLeastOnce).await?;
    debug!("Successfully subscribed to {}", agent_topic);

    // Send MQTTConnectRecord to identify ourselves to the controller
    let controller_id = &cfg.controller_id;
    let controller_topic = format!("usp/v1/controller/{}", sanitise_topic(controller_id));
    debug!("Controller topic: {}", controller_topic);
    
    debug!("Sending MQTTConnectRecord...");
    let connect_rec = mqtt_connect_record(agent_id.as_str(), controller_id, &agent_topic);
    let connect_bytes = encode_record(&connect_rec)?;
    debug!("MQTTConnectRecord encoded ({} bytes)", connect_bytes.len());
    client.publish(&controller_topic, QoS::AtLeastOnce, false, connect_bytes).await?;
    debug!("MQTTConnectRecord published successfully");

    info!("USP MQTT: connected; subscribed to {agent_topic}");

    debug!("Entering MQTT event loop...");
    loop {
        let event = event_loop.poll().await?;
        trace!("MQTT event received: {:?}", event);
        
        if let Event::Incoming(Packet::Publish(pub_msg)) = event {
            let topic = &pub_msg.topic;
            let payload = pub_msg.payload.to_vec();
            
            debug!("MQTT message received on topic '{}' ({} bytes, QoS={:?})", 
                   topic, payload.len(), pub_msg.qos);
            trace!("MQTT payload (first 64 bytes): {:?}", &payload[..payload.len().min(64)]);
            
            let record = match decode_record(&payload) {
                Ok(r)  => {
                    debug!("Successfully decoded USP record from MQTT");
                    trace!("Record: from_id={}, to_id={}, version={}", r.from_id, r.to_id, r.version);
                    r
                }
                Err(e) => { 
                    error!("MQTT: failed to decode record: {e}");
                    trace!("Raw MQTT payload (first 128 bytes): {:?}", &payload[..payload.len().min(128)]);
                    continue; 
                }
            };
            
            // TR-369 §5.1: discard records not addressed to this endpoint
            if !record.to_id.is_empty() && record.to_id != agent_id.as_str() {
                warn!("MQTT: to_id={} mismatch (expected {}), discarding",
                      record.to_id, agent_id.as_str());
                continue;
            }
            
            let msg_bytes = match extract_msg_payload(&record) {
                Some(b) => {
                    debug!("Extracted {} bytes USP message payload", b.len());
                    b.to_vec()
                }
                None    => {
                    warn!("No USP message payload found in MQTT record");
                    continue;
                }
            };
            
            debug!("Calling handle_incoming for message from {}", record.from_id);
            if let Some(resp) = super::super::agent::handle_incoming(
                cfg.clone(), agent_id.clone(), &msg_bytes, Arc::clone(&negotiated_ver)
            ).await {
                let ver = negotiated_ver.lock().unwrap().clone();
                debug!("Sending response via MQTT (version={})", ver);
                let resp_rec = no_session_record(agent_id.as_str(), &record.from_id, resp, &ver);
                if let Ok(encoded) = encode_record(&resp_rec) {
                    debug!("Response encoded ({} bytes), publishing to {}", encoded.len(), controller_topic);
                    match client.publish(&controller_topic, QoS::AtLeastOnce, false, encoded).await {
                        Ok(()) => debug!("Response published successfully"),
                        Err(e) => error!("Failed to publish response: {}", e),
                    }
                } else {
                    error!("Failed to encode response record");
                }
            } else {
                debug!("No response needed for this message");
            }
        } else {
            trace!("Non-publish MQTT event received");
        }
    }
}

// Copyright (c) 2014, 2015 Robert Clipsham <robert@octarineparrot.com>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// This example shows a basic packet logger using libpnet
extern crate pnet;

use dns_lookup::lookup_host;
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use pnet::datalink::{self, NetworkInterface};

use pnet::packet::ethernet::{EtherTypes, EthernetPacket};
use pnet::packet::ip::{IpNextHeaderProtocol, IpNextHeaderProtocols};
use pnet::packet::ipv4::Ipv4Packet;
use pnet::packet::tcp::TcpPacket;
use pnet::packet::Packet;
use pnet::util::MacAddr;

use anyhow::{anyhow, Result};
use std::net::IpAddr;
use std::process;
use std::{
    collections::{HashMap, HashSet},
    env,
    ops::Deref,
};
use std::{
    io::{self, Write},
    ops::Sub,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, SystemTime},
};
use tokio::{
    sync::mpsc::{channel, Receiver, Sender},
    time::interval,
};

fn handle_ethernet_frame(ethernet: &EthernetPacket, ips: &HashSet<IpAddr>) -> Result<bool, ()> {
    match ethernet.get_ethertype() {
        EtherTypes::Ipv4 => {
            let header = Ipv4Packet::new(ethernet.payload()).ok_or(())?;
            let source = IpAddr::V4(header.get_source());
            let destination = IpAddr::V4(header.get_destination());
            let protocol = header.get_next_level_protocol();
            match protocol {
                IpNextHeaderProtocols::Tcp => {
                    if ips.contains(&source) {
                        log::trace!("{}: {} <- {}", protocol, source, destination);
                        Ok(false)
                    } else if ips.contains(&destination) {
                        log::trace!("{}: {} -> {}", protocol, source, destination);
                        Ok(true)
                    } else {
                        Err(())
                    }
                }
                _ => Err(()),
            }
        }
        _ => Err(()),
    }
}

use pnet::datalink::Channel::Ethernet;

use crate::task::v2ray_task_config::CheckNetworkProperty;

use super::{v2ray_task_config::DnsFlushProperty, RetryService};

pub struct DnsFlushTask {
    prop: DnsFlushProperty,
    retry_srv: RetryService,
}

impl DnsFlushTask {
    pub fn new(prop: DnsFlushProperty) -> Self {
        Self {
            retry_srv: RetryService::new(prop.retry.clone()),
            prop,
        }
    }

    pub async fn run(&self, tx: Sender<Vec<IpAddr>>) -> Result<()> {
        let mut interval = interval(self.prop.update_interval);
        loop {
            interval.tick().await;
            match self.retry_srv.retry_on(|| self.update(&tx), false).await {
                Ok(a) => {
                    log::debug!("dns flush task successfully duration: {:?}", a.1);
                }
                Err(e) => {
                    log::error!("dns flush retry all error: {}", e);
                }
            }
        }
    }

    async fn update(&self, tx: &Sender<Vec<IpAddr>>) -> Result<()> {
        log::trace!("updating dns for hosts: {:?}", self.prop.hosts);
        let ips = self
            .prop
            .hosts
            .iter()
            .map(|host| lookup_host(host).unwrap())
            .flatten()
            .collect::<Vec<_>>();
        log::debug!("sending updated ips: {:?}", ips);
        tx.send(ips).await.map_err(Into::into)
    }
}

pub struct CheckNetworkTask {
    prop: CheckNetworkProperty,
    check_ips: Arc<Mutex<HashSet<IpAddr>>>,
}

impl CheckNetworkTask {
    pub fn new(prop: CheckNetworkProperty) -> Self {
        Self {
            prop,
            check_ips: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub async fn run(
        &self,
        ips_rx: Receiver<Vec<IpAddr>>,
        switch_sender: Sender<()>,
    ) -> anyhow::Result<()> {
        self.update_check_ips(ips_rx).await?;

        // Find the network interface with the provided name
        log::trace!(
            "Looking for available network cards by name: {}",
            self.prop.ifname
        );
        let interface = datalink::interfaces()
            .into_iter()
            .find(|iface| iface.name == self.prop.ifname)
            .ok_or_else(|| anyhow!("not found interface with name: {}", self.prop.ifname))?;

        // Create a channel to receive on
        let (_, mut rx) = match datalink::channel(&interface, Default::default()) {
            Ok(Ethernet(tx, rx)) => Ok((tx, rx)),
            Ok(_) => Err(anyhow!("unhandled channel type")),
            Err(e) => Err(e).map_err(Into::into),
        }?;

        let mut count_failed = 0;
        let mut last = None::<SystemTime>;
        log::trace!("starting switch monitor network with prop: {:?}", self.prop);
        loop {
            match rx.next() {
                Ok(packet) => {
                    if let Ok(res) = EthernetPacket::new(packet)
                        .as_ref()
                        .ok_or(())
                        .and_then(|packet| handle_ethernet_frame(packet, &self.check_ips.lock()))
                    {
                        if res {
                            if let Some(t) = last {
                                count_failed += 1;
                                let dur = t.elapsed()?;
                                if dur < self.prop.timeout {
                                    log::trace!("No response found within {:?}", dur);
                                    continue;
                                }
                                log::info!(
                                    "sending switch signal in elapsed {:?} for timeout of {:?}, count failed: {}",
                                    dur,
                                    self.prop.timeout,
                                    count_failed
                                );
                                switch_sender.send(()).await.unwrap();
                            } else {
                                last.replace(SystemTime::now());
                                continue;
                            }
                        }
                        count_failed = 0;
                        last.take();
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    async fn update_check_ips(&self, mut ips_rx: Receiver<Vec<IpAddr>>) -> Result<()> {
        log::trace!("Waiting for the first update of ips");
        let ips = ips_rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("receive error"))?;

        let update = |ips: Vec<IpAddr>, check_ips: &Arc<Mutex<HashSet<IpAddr>>>| {
            log::debug!("Received ips: {:?}", ips);
            let mut cips = check_ips.lock();
            ips.into_iter().for_each(|ip| {
                cips.insert(ip);
            });
        };

        let check_ips = self.check_ips.clone();
        update(ips, &check_ips);
        tokio::spawn(async move {
            while let Some(ips) = ips_rx.recv().await {
                update(ips, &check_ips);
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Once;

    use log::LevelFilter;

    use crate::task::v2ray_task_config::DnsFlushProperty;

    use super::*;

    #[tokio::test]
    async fn basic() -> anyhow::Result<()> {
        let (ips_tx, ips_rx) = channel::<Vec<IpAddr>>(1);
        tokio::spawn(async {
            let dns_prop = DnsFlushProperty::default();
            DnsFlushTask::new(dns_prop).run(ips_tx).await.unwrap();
        });

        let (tx, mut rx) = channel::<()>(1);
        let prop = get_prop();
        let task = CheckNetworkTask::new(prop);

        tokio::spawn(async move {
            while let Some(_) = rx.recv().await {
                log::info!("switch nodes");
            }
        });
        task.run(ips_rx, tx).await?;
        Ok(())
    }

    fn get_prop() -> CheckNetworkProperty {
        CheckNetworkProperty {
            timeout: Duration::from_secs(5),
            ifname: "eth0".to_string(),
        }
    }
}

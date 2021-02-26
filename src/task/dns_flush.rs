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
use log::warn;
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;
use pnet::datalink::{self, NetworkInterface};

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

use pnet::datalink::Channel::Ethernet;

use crate::task::v2ray_task_config::NetworkMonitorProperty;

use super::{v2ray_task_config::DnsFlushProperty, RetryService};

pub struct HostDnsFlushTask {
    prop: DnsFlushProperty,
    retry_srv: RetryService,
}

impl HostDnsFlushTask {
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

// #[cfg(test)]
// mod tests {
//     use std::sync::Once;

//     use log::LevelFilter;

//     use crate::task::v2ray_task_config::DnsFlushProperty;

//     use super::*;

//     #[tokio::test]
//     async fn basic() -> anyhow::Result<()> {
//         let (ips_tx, ips_rx) = channel::<Vec<IpAddr>>(1);
//         tokio::spawn(async {
//             let dns_prop = DnsFlushProperty::default();
//             HostDnsFlushTask::new(dns_prop).run(ips_tx).await.unwrap();
//         });

//         let (tx, mut rx) = channel::<bool>(1);
//         let prop = Default::default();
//         let task = CheckNetworkTask::new(prop);

//         tokio::spawn(async move {
//             while let Some(_) = rx.recv().await {
//                 log::info!("switch nodes");
//             }
//         });
//         task.run(ips_rx, tx).await?;
//         Ok(())
//     }

//     #[tokio::test]
//     async fn test() {
//         let (ips_tx, mut ips_rx) = channel::<()>(1);
//         loop {
//             log::debug!("test");
//             if ips_rx.recv().await.is_some() {}
//         }
//     }
// }

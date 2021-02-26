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

use anyhow::Result;
use std::net::IpAddr;

use tokio::{sync::mpsc::Sender, time::interval};

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
//     use std::time::Duration;

//     use tokio::sync::mpsc::channel;

//     use super::*;

//     #[tokio::test]
//     async fn basic() -> Result<()> {
//         let prop = DnsFlushProperty {
//             update_interval: Duration::from_secs(2),
//             ..Default::default()
//         };
//         let task = HostDnsFlushTask::new(prop);
//         let (tx, mut rx) = channel::<Vec<IpAddr>>(1);
//         tokio::spawn(async move {
//             while let Some(v) = rx.recv().await {
//                 log::debug!("ips: {:?}", v);
//             }
//         });
//         task.run(tx).await?;
//         Ok(())
//     }
// }
